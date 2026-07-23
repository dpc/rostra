use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;

use crate::event::EventContentState;
use crate::{
    Database, DbError, db_version, events_content_state, social_news_rank_by_post_id, social_posts,
    social_posts_by_received_at, social_posts_by_time, social_posts_reactions,
    social_posts_received_at_keys, social_posts_replaced_by, social_posts_replaces,
    social_posts_replies, social_posts_self_mention,
};

#[derive(Clone, Copy, Debug)]
enum DeletingPostBody {
    Absent,
    Empty,
    Whitespace,
    Edit,
}

impl DeletingPostBody {
    fn is_edit(self) -> bool {
        matches!(self, Self::Edit)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ProjectionSnapshot {
    reply_count: u64,
    reaction_count: u64,
    replies: Vec<ShortEventId>,
    reactions: Vec<ShortEventId>,
    target_in_time: bool,
    target_in_news: bool,
    target_in_mentions: bool,
    receipt_event_ids: Vec<ShortEventId>,
    target_replaces_parent: bool,
    parent_replaced_by_target: bool,
    parent_deleted_by: ShortEventId,
    target_deleted_by: ShortEventId,
}

fn social_post(
    secret: RostraIdSecretKey,
    timestamp: i64,
    parent_prev: Option<rostra_core::EventId>,
    replaced: Option<rostra_core::EventId>,
    content: content_kind::SocialPost,
) -> VerifiedEventContent {
    let content = content
        .serialize_cbor()
        .expect("social post must serialize");
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid timestamp"))
        .content(&content)
        .maybe_parent_prev(parent_prev.map(Into::into))
        .maybe_delete(replaced.map(Into::into))
        .build()
        .signed_by(secret);
    let event = VerifiedEvent::verify_signed(secret.id(), event).expect("event must verify");
    VerifiedEventContent::assume_verified(event, content)
}

fn deletion(
    secret: RostraIdSecretKey,
    timestamp: i64,
    parent: rostra_core::EventId,
    target: rostra_core::EventId,
) -> VerifiedEvent {
    let content = EventContentRaw::new(vec![]);
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid timestamp"))
        .parent_prev(parent.into())
        .delete(target.into())
        .content(&content)
        .build()
        .signed_by(secret);
    VerifiedEvent::verify_signed(secret.id(), event).expect("event must verify")
}

fn delete_flag_without_aux_post(
    secret: RostraIdSecretKey,
    timestamp: i64,
    content: content_kind::SocialPost,
) -> VerifiedEventContent {
    let content = content
        .serialize_cbor()
        .expect("social post must serialize");
    let mut event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid timestamp"))
        .content(&content)
        .build();
    event.flags |= Event::DELETE_PARENT_AUX_CONTENT_FLAG;
    assert!(event.parent_aux().is_none());
    let event = event.signed_by(secret);
    let event = VerifiedEvent::verify_signed(secret.id(), event).expect("event must verify");
    VerifiedEventContent::assume_verified(event, content)
}

fn deleting_content(
    body: DeletingPostBody,
    reply_to: ExternalEventId,
    self_id: RostraId,
) -> content_kind::SocialPost {
    let post = match body {
        DeletingPostBody::Absent => {
            content_kind::SocialPost::new("👍".to_owned(), Some(reply_to), Default::default())
        }
        DeletingPostBody::Empty => {
            content_kind::SocialPost::new_text(String::new(), Some(reply_to), Default::default())
        }
        DeletingPostBody::Whitespace => content_kind::SocialPost::new_text(
            " \n\t".to_owned(),
            Some(reply_to),
            Default::default(),
        ),
        DeletingPostBody::Edit => content_kind::SocialPost::new_text(
            format!("edited; hello <rostra:{self_id}>"),
            Some(reply_to),
            Default::default(),
        ),
    };
    post.with_news_fields(None, Some("projection test".to_owned()))
}

async fn snapshot(
    db: &Database,
    author: RostraId,
    reply_target: ShortEventId,
    parent: ShortEventId,
    target: &VerifiedEventContent,
) -> BoxedErrorResult<ProjectionSnapshot> {
    let target_id = target.event_id().to_short();
    let target_ts = target.timestamp();
    Ok(db
        .read_with(|tx| {
            let aggregate = tx
                .open_table(&social_posts::TABLE)?
                .get(&reply_target)?
                .map(|entry| entry.value())
                .unwrap_or_default();
            let replies = tx
                .open_table(&social_posts_replies::TABLE)?
                .range(
                    &(reply_target, Timestamp::ZERO, ShortEventId::ZERO)
                        ..=&(reply_target, Timestamp::MAX, ShortEventId::MAX),
                )?
                .map(|entry| entry.map(|(key, _)| key.value().2))
                .collect::<Result<Vec<_>, _>>()?;
            let reactions = tx
                .open_table(&social_posts_reactions::TABLE)?
                .range(
                    &(reply_target, Timestamp::ZERO, ShortEventId::ZERO)
                        ..=&(reply_target, Timestamp::MAX, ShortEventId::MAX),
                )?
                .map(|entry| entry.map(|(key, _)| key.value().2))
                .collect::<Result<Vec<_>, _>>()?;
            let target_in_time = tx
                .open_table(&social_posts_by_time::TABLE)?
                .get(&(target_ts, target_id))?
                .is_some();
            let target_in_news = tx
                .open_table(&social_news_rank_by_post_id::TABLE)?
                .get(&ExternalEventId::new(author, target_id))?
                .is_some();
            let target_in_mentions = tx
                .open_table(&social_posts_self_mention::TABLE)?
                .get(&target_id)?
                .is_some();
            let receipt_entries = tx
                .open_table(&social_posts_by_received_at::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(key, event_id)| (key.value(), event_id.value())))
                .collect::<Result<Vec<_>, _>>()?;
            let receipt_keys = tx.open_table(&social_posts_received_at_keys::TABLE)?;
            let mut reverse_event_ids = receipt_keys
                .range(..)?
                .map(|entry| entry.map(|(event_id, _)| event_id.value()))
                .collect::<Result<Vec<_>, _>>()?;
            let mut receipt_event_ids = receipt_entries
                .iter()
                .map(|(_, event_id)| *event_id)
                .collect::<Vec<_>>();
            receipt_event_ids.sort_unstable();
            reverse_event_ids.sort_unstable();
            assert_eq!(reverse_event_ids, receipt_event_ids);
            for (key, event_id) in receipt_entries {
                assert_eq!(
                    receipt_keys.get(&event_id)?.map(|entry| entry.value()),
                    Some(key),
                    "forward and reverse receipt rows must agree"
                );
            }
            let target_replaces_parent = tx
                .open_table(&social_posts_replaces::TABLE)?
                .get(&(author, target_id, parent))?
                .is_some();
            let parent_replaced_by_target = tx
                .open_table(&social_posts_replaced_by::TABLE)?
                .get(&(author, parent, target_id))?
                .is_some();
            let states = tx.open_table(&events_content_state::TABLE)?;
            let parent_deleted_by = match states.get(&parent)?.map(|entry| entry.value()) {
                Some(EventContentState::Deleted { deleted_by }) => deleted_by,
                state => panic!("parent must be Deleted, got {state:?}"),
            };
            let target_deleted_by = match states.get(&target_id)?.map(|entry| entry.value()) {
                Some(EventContentState::Deleted { deleted_by }) => deleted_by,
                state => panic!("target must be Deleted, got {state:?}"),
            };

            Ok(ProjectionSnapshot {
                reply_count: aggregate.reply_count,
                reaction_count: aggregate.reaction_count,
                replies,
                reactions,
                target_in_time,
                target_in_news,
                target_in_mentions,
                receipt_event_ids,
                target_replaces_parent,
                parent_replaced_by_target,
                parent_deleted_by,
                target_deleted_by,
            })
        })
        .await?)
}

fn force_total_replay(path: &std::path::Path) -> BoxedErrorResult<()> {
    let db = redb_bincode::Database::from(redb::Database::open(path).boxed()?);
    let tx = db.begin_write().boxed()?;
    tx.open_table(&db_version::TABLE)?
        .insert(&(), &24)
        .boxed()?;
    tx.commit().boxed()?;
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn deleting_post_projection_reversion_is_symmetric() -> BoxedErrorResult<()> {
    let self_id = RostraIdSecretKey::from_bytes([82; 32]).id();
    let author_secret = RostraIdSecretKey::from_bytes([81; 32]);
    let author = author_secret.id();

    for body in [
        DeletingPostBody::Absent,
        DeletingPostBody::Empty,
        DeletingPostBody::Whitespace,
        DeletingPostBody::Edit,
    ] {
        for seed_unrelated in [false, true] {
            let mut final_snapshots = Vec::new();
            for delete_first in [false, true] {
                let dir = tempfile::tempdir()?;
                let path = dir.path().join("db.redb");
                let db = Database::open(&path, self_id).await.boxed()?;

                let reply_target = social_post(
                    author_secret,
                    10,
                    None,
                    None,
                    content_kind::SocialPost::new_text(
                        "reply target".to_owned(),
                        None,
                        Default::default(),
                    ),
                );
                let reply_target_id = reply_target.event_id().to_short();
                let mut expected_receipt_event_ids = vec![reply_target_id];
                let reply_to = ExternalEventId::new(author, reply_target.event_id());
                let parent = social_post(
                    author_secret,
                    11,
                    None,
                    None,
                    content_kind::SocialPost::new_text(
                        "header target".to_owned(),
                        None,
                        Default::default(),
                    ),
                );

                db.try_process_event_with_content(&reply_target).await?;
                db.try_process_event_with_content(&parent).await?;

                let mut expected_replies = Vec::new();
                let mut expected_reactions = Vec::new();
                if seed_unrelated {
                    let reply = social_post(
                        author_secret,
                        12,
                        Some(reply_target.event_id()),
                        None,
                        content_kind::SocialPost::new_text(
                            "unrelated reply".to_owned(),
                            Some(reply_to),
                            Default::default(),
                        ),
                    );
                    let reaction = social_post(
                        author_secret,
                        13,
                        Some(reply.event_id()),
                        None,
                        content_kind::SocialPost::new(
                            "👍".to_owned(),
                            Some(reply_to),
                            Default::default(),
                        ),
                    );
                    db.try_process_event_with_content(&reply).await?;
                    db.try_process_event_with_content(&reaction).await?;
                    expected_replies.push(reply.event_id().to_short());
                    expected_reactions.push(reaction.event_id().to_short());
                    expected_receipt_event_ids.push(reply.event_id().to_short());
                    expected_receipt_event_ids.push(reaction.event_id().to_short());
                }

                let target = social_post(
                    author_secret,
                    20,
                    Some(parent.event_id()),
                    Some(parent.event_id()),
                    deleting_content(body, reply_to, self_id),
                );
                let target_id = target.event_id().to_short();
                let deleting = deletion(author_secret, 21, target.event_id(), target.event_id());
                let deleting_id = deleting.event_id.to_short();

                if delete_first {
                    db.try_process_event(&deleting).await?;
                    db.try_process_event_with_content(&target).await?;
                } else {
                    db.try_process_event_with_content(&target).await?;
                    db.try_process_event_with_content(&target).await?;
                    let before = db
                        .read_with(|tx| {
                            let aggregate = tx
                                .open_table(&social_posts::TABLE)?
                                .get(&reply_target_id)?
                                .map(|entry| entry.value())
                                .unwrap_or_default();
                            let time = tx
                                .open_table(&social_posts_by_time::TABLE)?
                                .get(&(target.timestamp(), target_id))?
                                .is_some();
                            let news = tx
                                .open_table(&social_news_rank_by_post_id::TABLE)?
                                .get(&ExternalEventId::new(author, target_id))?
                                .is_some();
                            let mention = tx
                                .open_table(&social_posts_self_mention::TABLE)?
                                .get(&target_id)?
                                .is_some();
                            let receipt_key = tx
                                .open_table(&social_posts_received_at_keys::TABLE)?
                                .get(&target_id)?
                                .map(|entry| entry.value());
                            let receipt = if let Some(key) = receipt_key {
                                tx.open_table(&social_posts_by_received_at::TABLE)?
                                    .get(&key)?
                                    .map(|entry| entry.value())
                            } else {
                                None
                            };
                            Ok((
                                aggregate.reply_count,
                                aggregate.reaction_count,
                                time,
                                news,
                                mention,
                                receipt,
                                receipt_key.is_some(),
                            ))
                        })
                        .await?;
                    let base = u64::from(seed_unrelated);
                    match body {
                        DeletingPostBody::Absent => {
                            assert_eq!(before, (base, base, false, false, false, None, false));
                        }
                        DeletingPostBody::Empty => {
                            assert_eq!(before, (base, base, false, false, false, None, false));
                        }
                        DeletingPostBody::Whitespace => {
                            assert_eq!(before, (base, base, false, false, false, None, false));
                        }
                        DeletingPostBody::Edit => {
                            assert_eq!(
                                before,
                                (base + 1, base, true, true, true, Some(target_id), true,)
                            );
                        }
                    }
                    db.try_process_event(&deleting).await?;
                }

                db.try_process_event_with_content(&target).await?;
                db.try_process_event(&deleting).await?;

                let expected = ProjectionSnapshot {
                    reply_count: u64::from(seed_unrelated),
                    reaction_count: u64::from(seed_unrelated),
                    replies: expected_replies,
                    reactions: expected_reactions,
                    target_in_time: false,
                    target_in_news: false,
                    target_in_mentions: false,
                    receipt_event_ids: {
                        expected_receipt_event_ids.sort_unstable();
                        expected_receipt_event_ids
                    },
                    target_replaces_parent: body.is_edit(),
                    parent_replaced_by_target: body.is_edit(),
                    parent_deleted_by: target_id,
                    target_deleted_by: deleting_id,
                };
                let final_snapshot = snapshot(
                    &db,
                    author,
                    reply_target_id,
                    parent.event_id().to_short(),
                    &target,
                )
                .await?;
                assert_eq!(
                    final_snapshot, expected,
                    "{body:?}, seed_unrelated={seed_unrelated}, delete_first={delete_first}"
                );

                drop(db);
                let reopened = Database::open(&path, self_id).await.boxed()?;
                assert_eq!(
                    snapshot(
                        &reopened,
                        author,
                        reply_target_id,
                        parent.event_id().to_short(),
                        &target,
                    )
                    .await?,
                    expected,
                    "{body:?}, seed_unrelated={seed_unrelated}, delete_first={delete_first}: reopen"
                );
                drop(reopened);

                force_total_replay(&path)?;
                let replayed = Database::open(&path, self_id).await.boxed()?;
                let replayed = snapshot(
                    &replayed,
                    author,
                    reply_target_id,
                    parent.event_id().to_short(),
                    &target,
                )
                .await?;
                assert_eq!(
                    replayed, expected,
                    "{body:?}, seed_unrelated={seed_unrelated}, delete_first={delete_first}: replay"
                );
                final_snapshots.push(replayed);
            }
            assert_eq!(
                final_snapshots[0], final_snapshots[1],
                "{body:?}, seed_unrelated={seed_unrelated}: delivery orders"
            );
        }
    }

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn delete_flag_without_aux_is_projection_inert() -> BoxedErrorResult<()> {
    let self_id = RostraIdSecretKey::from_bytes([85; 32]).id();
    let secret = RostraIdSecretKey::from_bytes([84; 32]);
    let author = secret.id();
    let db = Database::new_in_memory(self_id).await?;
    let parent = social_post(
        secret,
        40,
        None,
        None,
        content_kind::SocialPost::new_text("parent".to_owned(), None, Default::default()),
    );
    let reply_to = ExternalEventId::new(author, parent.event_id());
    let target = delete_flag_without_aux_post(
        secret,
        41,
        content_kind::SocialPost::new_text(
            format!("nonblank; hello <rostra:{self_id}>"),
            Some(reply_to),
            Default::default(),
        )
        .with_news_fields(None, Some("must remain inert".to_owned())),
    );
    let target_id = target.event_id().to_short();

    db.try_process_event_with_content(&parent).await?;
    db.try_process_event_with_content(&target).await?;
    db.read_with(|tx| {
        let aggregate = tx
            .open_table(&social_posts::TABLE)?
            .get(&parent.event_id().to_short())?
            .map(|entry| entry.value())
            .unwrap_or_default();
        assert_eq!(aggregate.reply_count, 0);
        assert!(
            tx.open_table(&social_posts_by_time::TABLE)?
                .get(&(target.timestamp(), target_id))?
                .is_none()
        );
        assert!(
            tx.open_table(&social_news_rank_by_post_id::TABLE)?
                .get(&ExternalEventId::new(author, target_id))?
                .is_none()
        );
        assert!(
            tx.open_table(&social_posts_self_mention::TABLE)?
                .get(&target_id)?
                .is_none()
        );
        Ok(())
    })
    .await?;

    let deleting = deletion(secret, 42, target.event_id(), target.event_id());
    db.try_process_event(&deleting).await?;
    let aggregate = db
        .read_with(|tx| {
            Ok(tx
                .open_table(&social_posts::TABLE)?
                .get(&parent.event_id().to_short())?
                .map(|entry| entry.value())
                .unwrap_or_default())
        })
        .await?;
    assert_eq!(aggregate.reply_count, 0);

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn ordinary_reaction_projection_still_reverts() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([83; 32]);
    let author = secret.id();
    let db = Database::new_in_memory(author).await?;
    let parent = social_post(
        secret,
        30,
        None,
        None,
        content_kind::SocialPost::new_text("parent".to_owned(), None, Default::default()),
    );
    let reaction = social_post(
        secret,
        31,
        Some(parent.event_id()),
        None,
        content_kind::SocialPost::new(
            "👍".to_owned(),
            Some(ExternalEventId::new(author, parent.event_id())),
            Default::default(),
        ),
    );
    let deleting = deletion(secret, 32, reaction.event_id(), reaction.event_id());

    db.try_process_event_with_content(&parent).await?;
    db.try_process_event_with_content(&reaction).await?;
    let reaction_id = reaction.event_id().to_short();
    db.read_with(|tx| {
        let aggregate = tx
            .open_table(&social_posts::TABLE)?
            .get(&parent.event_id().to_short())?
            .expect("reaction aggregate must exist")
            .value();
        assert_eq!(aggregate.reaction_count, 1);
        let receipt_key = tx
            .open_table(&social_posts_received_at_keys::TABLE)?
            .get(&reaction_id)?
            .expect("reaction receipt key must exist")
            .value();
        assert_eq!(
            tx.open_table(&social_posts_by_received_at::TABLE)?
                .get(&receipt_key)?
                .map(|entry| entry.value()),
            Some(reaction_id)
        );
        assert!(
            tx.open_table(&social_posts_reactions::TABLE)?
                .get(&(
                    parent.event_id().to_short(),
                    reaction.timestamp(),
                    reaction_id
                ))?
                .is_some()
        );
        Ok(())
    })
    .await?;

    db.try_process_event(&deleting).await?;
    db.try_process_event(&deleting).await?;
    db.read_with(|tx| {
        let aggregate = tx
            .open_table(&social_posts::TABLE)?
            .get(&parent.event_id().to_short())?
            .expect("reaction aggregate must remain")
            .value();
        assert_eq!(aggregate.reaction_count, 0);
        assert!(
            tx.open_table(&social_posts_received_at_keys::TABLE)?
                .get(&reaction_id)?
                .is_none()
        );
        assert!(
            tx.open_table(&social_posts_by_received_at::TABLE)?
                .range(..)?
                .all(|entry| entry
                    .map(|(_, event_id)| event_id.value() != reaction_id)
                    .unwrap_or(false))
        );
        assert!(
            tx.open_table(&social_posts_reactions::TABLE)?
                .get(&(
                    parent.event_id().to_short(),
                    reaction.timestamp(),
                    reaction_id
                ))?
                .is_none()
        );
        Ok(())
    })
    .await?;

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn inconsistent_eligible_reaction_reversion_fails_and_rolls_back() -> BoxedErrorResult<()> {
    let secret = RostraIdSecretKey::from_bytes([86; 32]);
    let author = secret.id();
    let db = Database::new_in_memory(author).await?;
    let parent = social_post(
        secret,
        50,
        None,
        None,
        content_kind::SocialPost::new_text("parent".to_owned(), None, Default::default()),
    );
    let reaction = social_post(
        secret,
        51,
        Some(parent.event_id()),
        None,
        content_kind::SocialPost::new(
            "👍".to_owned(),
            Some(ExternalEventId::new(author, parent.event_id())),
            Default::default(),
        ),
    );
    let parent_id = parent.event_id().to_short();
    let reaction_id = reaction.event_id().to_short();
    let reaction_key = (parent_id, reaction.timestamp(), reaction_id);
    let deleting = deletion(secret, 52, reaction.event_id(), reaction.event_id());

    db.try_process_event_with_content(&parent).await?;
    db.try_process_event_with_content(&reaction).await?;
    db.write_with(|tx| {
        let mut posts = tx.open_table(&social_posts::TABLE)?;
        let mut aggregate = posts
            .get(&parent_id)?
            .expect("reaction aggregate must exist")
            .value();
        aggregate.reaction_count = 0;
        posts.insert(&parent_id, &aggregate)?;
        Ok(())
    })
    .await?;

    let error = db
        .try_process_event(&deleting)
        .await
        .expect_err("inconsistent eligible reversion must fail");
    assert!(matches!(error, DbError::Overflow));
    assert!(db.get_event(deleting.event_id).await.is_none());
    assert!(db.get_event_content(reaction.event_id()).await.is_some());
    db.read_with(|tx| {
        let state = tx
            .open_table(&events_content_state::TABLE)?
            .get(&reaction_id)?
            .map(|entry| entry.value());
        assert_eq!(state, None);
        let aggregate = tx
            .open_table(&social_posts::TABLE)?
            .get(&parent_id)?
            .expect("corrupt aggregate must remain unchanged")
            .value();
        assert_eq!(aggregate.reaction_count, 0);
        let receipt_key = tx
            .open_table(&social_posts_received_at_keys::TABLE)?
            .get(&reaction_id)?
            .expect("failed reversion must preserve reverse receipt")
            .value();
        assert_eq!(
            tx.open_table(&social_posts_by_received_at::TABLE)?
                .get(&receipt_key)?
                .map(|entry| entry.value()),
            Some(reaction_id),
            "failed reversion must preserve forward receipt"
        );
        assert!(
            tx.open_table(&social_posts_reactions::TABLE)?
                .get(&reaction_key)?
                .is_some()
        );
        Ok(())
    })
    .await?;

    Ok(())
}
