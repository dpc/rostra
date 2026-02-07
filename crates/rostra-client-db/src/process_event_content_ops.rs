use std::cmp;

use rostra_core::Timestamp;
use rostra_core::event::{EventExt as _, EventKind, VerifiedEventContent, content_kind};
use rostra_core::id::ToShort as _;
use rostra_util_error::{BoxedError, FmtCompact as _};
use snafu::{Location, OptionExt as _, ResultExt as _, Snafu};
use tracing::debug;

use crate::event::EventSingletonRecord;
use crate::{
    Database, DbError, IdSocialProfileRecord, IrohNodeRecord, LOG_TARGET, OverflowSnafu,
    SocialPostsReactionsRecord, SocialPostsRepliesRecord, WriteTransactionCtx,
    events_singletons_new, social_posts, social_posts_by_received_at, social_posts_by_time,
    social_posts_reactions, social_posts_replies, social_posts_self_mention,
};

#[derive(Debug, Snafu)]
pub enum ProcessEventError {
    #[snafu(transparent)]
    Db { source: DbError },
    Invalid {
        #[snafu(implicit)]
        location: Location,
        source: BoxedError,
    },
}
pub type ProcessEventResult<T> = std::result::Result<T, ProcessEventError>;

impl Database {
    /// After an event content was inserted process special kinds of event
    /// content, like follows/unfollows.
    ///
    /// The `now` parameter should be `Timestamp::now()` for normal operation,
    /// but can be set to a specific value for testing or migration.
    pub fn process_event_content_inserted_tx(
        &self,
        event_content: &VerifiedEventContent,
        now: Timestamp,
        tx: &WriteTransactionCtx,
    ) -> ProcessEventResult<()> {
        let author = event_content.event.event.author;
        #[allow(clippy::single_match)]
        match event_content.event.event.kind {
            EventKind::FOLLOW | EventKind::UNFOLLOW => {
                let mut ids_followees_t = tx
                    .open_table(&crate::ids_followees::TABLE)
                    .map_err(DbError::from)?;
                let mut ids_followers_t = tx
                    .open_table(&crate::ids_followers::TABLE)
                    .map_err(DbError::from)?;
                let mut id_unfollowed_t = tx
                    .open_table(&crate::ids_unfollowed::TABLE)
                    .map_err(DbError::from)?;

                // Both FOLLOW and UNFOLLOW use the same content type.
                // The actual follow/unfollow distinction is in the content's is_unfollow()
                // method.
                let content = event_content
                    .deserialize_cbor::<content_kind::Follow>()
                    .boxed()
                    .context(InvalidSnafu)?;
                let (followee, updated) = (
                    content.followee,
                    if content.is_unfollow() {
                        Database::insert_unfollow_tx(
                            author,
                            event_content.event.event.timestamp.into(),
                            content.followee,
                            &mut ids_followees_t,
                            &mut ids_followers_t,
                            &mut id_unfollowed_t,
                        )?
                    } else {
                        Database::insert_follow_tx(
                            author,
                            event_content.event.event.timestamp.into(),
                            content,
                            &mut ids_followees_t,
                            &mut ids_followers_t,
                            &mut id_unfollowed_t,
                        )?
                    },
                );

                if updated {
                    if author == self.self_id {
                        let followees_sender = self.self_followees_updated.clone();
                        let self_followees =
                            Database::read_followees_tx(self.self_id, &ids_followees_t)?;
                        tx.on_commit(move || {
                            let _ = followees_sender.send(self_followees);
                        });
                    }

                    if followee == self.self_id {
                        let followers_sender = self.self_followers_updated.clone();
                        let self_followers =
                            Database::read_followers_tx(self.self_id, &ids_followers_t)?;

                        tx.on_commit(move || {
                            let _ = followers_sender.send(self_followers);
                        });
                    }
                }
            }
            _ => match event_content.event.event.kind {
                EventKind::NODE_ANNOUNCEMENT => {
                    let content = event_content
                        .deserialize_cbor::<content_kind::NodeAnnouncement>()
                        .boxed()
                        .context(InvalidSnafu)?;
                    let mut ids_nodes_tbl = tx
                        .open_table(&crate::ids_nodes::TABLE)
                        .map_err(DbError::from)?;

                    let addr = match content {
                        content_kind::NodeAnnouncement::Iroh { addr } => addr,
                    };
                    let key = (event_content.author(), addr);
                    let mut existing = ids_nodes_tbl
                        .get(&key)
                        .map_err(DbError::from)?
                        .map(|g| g.value())
                        .unwrap_or_else(|| IrohNodeRecord {
                            announcement_ts: event_content.timestamp(),
                            stats: Default::default(),
                        });

                    existing.announcement_ts =
                        cmp::max(existing.announcement_ts, event_content.timestamp());

                    ids_nodes_tbl
                        .insert(&key, &existing)
                        .map_err(DbError::from)?;

                    Database::trim_iroh_nodes_to_limit_tx(
                        event_content.author(),
                        &mut ids_nodes_tbl,
                    )?;
                }
                EventKind::SOCIAL_PROFILE_UPDATE => {
                    let content = event_content
                        .deserialize_cbor::<content_kind::SocialProfileUpdate>()
                        .boxed()
                        .context(InvalidSnafu)?;
                    Database::insert_latest_value_tx(
                        event_content.event.event.timestamp.into(),
                        &author,
                        IdSocialProfileRecord {
                            event_id: event_content.event.event_id.to_short(),
                            display_name: content.display_name,
                            bio: content.bio,
                            avatar: content.avatar,
                        },
                        &mut tx
                            .open_table(&crate::social_profiles::TABLE)
                            .map_err(DbError::from)?,
                    )?;
                }
                EventKind::SOCIAL_POST => {
                    let content = event_content
                        .deserialize_cbor::<content_kind::SocialPost>().inspect_err(|err| {
                            debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed SocialComment payload");
                        }).boxed().context(InvalidSnafu)?;

                    let mut social_post_by_time_tbl = tx
                        .open_table(&social_posts_by_time::TABLE)
                        .map_err(DbError::from)?;
                    social_post_by_time_tbl
                        .insert(
                            &(
                                event_content.timestamp(),
                                event_content.event_id().to_short(),
                            ),
                            &(),
                        )
                        .map_err(DbError::from)?;

                    // Also insert into received_at index for notification ordering.
                    // Use the same `now` timestamp that was used for events_received_at.
                    let mut social_post_by_received_at_tbl = tx
                        .open_table(&social_posts_by_received_at::TABLE)
                        .map_err(DbError::from)?;
                    let reception_order = self.next_reception_order();
                    let key = (now, reception_order);
                    // Assert key uniqueness - reception_order is monotonic so this should never
                    // fail
                    let prev = social_post_by_received_at_tbl
                        .insert(&key, &event_content.event_id().to_short())
                        .map_err(DbError::from)?;
                    debug_assert!(
                        prev.is_none(),
                        "social_posts_by_received_at key collision: {key:?}"
                    );

                    tx.on_commit({
                        let event_content = event_content.clone();
                        let content = content.clone();
                        let new_posts_tx = self.new_posts_tx.clone();
                        move || {
                            let _ = new_posts_tx.send((event_content.to_owned(), content));
                        }
                    });

                    // Check for @mentions of self in the post content
                    if author != self.self_id {
                        if let Some(ref djot_content) = content.djot_content {
                            if rostra_djot::mention::contains_mention(djot_content, self.self_id) {
                                let mut self_mention_tbl = tx
                                    .open_table(&social_posts_self_mention::TABLE)
                                    .map_err(DbError::from)?;
                                self_mention_tbl
                                    .insert(&event_content.event_id().to_short(), &())
                                    .map_err(DbError::from)?;
                            }
                        }
                    }

                    if let Some(reply_to) = content.reply_to {
                        let mut social_post_tbl =
                            tx.open_table(&social_posts::TABLE).map_err(DbError::from)?;
                        let mut social_post_replies_tbl = tx
                            .open_table(&social_posts_replies::TABLE)
                            .map_err(DbError::from)?;

                        let mut social_post_reactions_tbl = tx
                            .open_table(&social_posts_reactions::TABLE)
                            .map_err(DbError::from)?;

                        let mut reply_to_social_post_record = social_post_tbl
                            .get(&reply_to.event_id())
                            .map_err(DbError::from)?
                            .map(|g| g.value())
                            .unwrap_or_default();

                        if content.djot_content.is_some() {
                            reply_to_social_post_record.reply_count = reply_to_social_post_record
                                .reply_count
                                .checked_add(1)
                                .context(OverflowSnafu)?;

                            social_post_replies_tbl
                                .insert(
                                    &(
                                        reply_to.event_id(),
                                        event_content.event.event.timestamp.into(),
                                        event_content.event.event_id.to_short(),
                                    ),
                                    &SocialPostsRepliesRecord,
                                )
                                .map_err(DbError::from)?;
                        }

                        if content.reaction.is_some() {
                            reply_to_social_post_record.reaction_count =
                                reply_to_social_post_record
                                    .reaction_count
                                    .checked_add(1)
                                    .context(OverflowSnafu)?;

                            social_post_reactions_tbl
                                .insert(
                                    &(
                                        reply_to.event_id(),
                                        event_content.event.event.timestamp.into(),
                                        event_content.event.event_id.to_short(),
                                    ),
                                    &SocialPostsReactionsRecord,
                                )
                                .map_err(DbError::from)?;
                        }
                        social_post_tbl
                            .insert(&reply_to.event_id(), &reply_to_social_post_record)
                            .map_err(DbError::from)?;
                    }
                }
                _ => {}
            },
        };

        if event_content.event.is_singleton() {
            let mut events_singletons_tbl = tx
                .open_table(&events_singletons_new::TABLE)
                .map_err(DbError::from)?;

            Self::insert_latest_value_tx(
                event_content.timestamp(),
                &(
                    event_content.author(),
                    event_content.kind(),
                    event_content.aux_key(),
                ),
                EventSingletonRecord {
                    event_id: event_content.event_id().to_short(),
                },
                &mut events_singletons_tbl,
            )?;
        }

        Ok(())
    }

    pub fn process_event_content_reverted_tx(
        &self,
        event_content: &VerifiedEventContent,
        tx: &WriteTransactionCtx,
    ) -> ProcessEventResult<()> {
        #[allow(clippy::single_match)]
        match event_content.event.event.kind {
            EventKind::SOCIAL_POST => {
                let content = event_content
                    .deserialize_cbor::<content_kind::SocialPost>()
                    .boxed()
                    .context(InvalidSnafu)?;

                let mut social_post_by_time_tbl = tx
                    .open_table(&social_posts_by_time::TABLE)
                    .map_err(DbError::from)?;

                social_post_by_time_tbl
                    .remove(&(
                        event_content.timestamp(),
                        event_content.event_id().to_short(),
                    ))
                    .map_err(DbError::from)?;

                // Remove from self-mention table if present
                let mut self_mention_tbl = tx
                    .open_table(&social_posts_self_mention::TABLE)
                    .map_err(DbError::from)?;
                self_mention_tbl
                    .remove(&event_content.event_id().to_short())
                    .map_err(DbError::from)?;

                if let Some(reply_to) = content.reply_to {
                    let mut social_posts_tbl =
                        tx.open_table(&social_posts::TABLE).map_err(DbError::from)?;
                    let mut social_replies_tbl = tx
                        .open_table(&social_posts_replies::TABLE)
                        .map_err(DbError::from)?;
                    let mut social_reactions_tbl = tx
                        .open_table(&social_posts_reactions::TABLE)
                        .map_err(DbError::from)?;

                    let mut social_post_record = social_posts_tbl
                        .get(&reply_to.event_id())
                        .map_err(DbError::from)?
                        .map(|g| g.value())
                        .unwrap_or_default();

                    if content.djot_content.is_some() {
                        social_replies_tbl
                            .remove(&(
                                reply_to.event_id(),
                                event_content.timestamp(),
                                event_content.event_id().to_short(),
                            ))
                            .map_err(DbError::from)?;

                        social_post_record.reply_count = social_post_record
                            .reply_count
                            .checked_sub(1)
                            .context(OverflowSnafu)?;
                    }

                    if content.reaction.is_some() {
                        social_reactions_tbl
                            .remove(&(
                                reply_to.event_id(),
                                event_content.timestamp(),
                                event_content.event_id().to_short(),
                            ))
                            .map_err(DbError::from)?;

                        social_post_record.reaction_count = social_post_record
                            .reaction_count
                            .checked_sub(1)
                            .context(OverflowSnafu)?;
                    }
                    social_posts_tbl
                        .insert(&reply_to.event_id(), &social_post_record)
                        .map_err(DbError::from)?;
                }
            }
            _ => {}
        }

        Ok(())
    }
}
