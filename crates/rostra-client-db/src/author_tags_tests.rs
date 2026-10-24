//! Integration coverage for bounded local author-label discovery.

use std::borrow::Cow;
use std::collections::BTreeSet;

use redb::ReadableTable as _;
use rostra_core::event::content_kind::{EventContentKind as _, SocialPost};
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, PersonaTag, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};

use crate::author_tags::{
    AUTHOR_TAG_BYTES_MIN, AUTHOR_TAG_WORK_MIN, AuthorTagBounds, AuthorTagCursor, AuthorTagStop,
};
use crate::{Database, DbError, EventRecord};

fn post(
    secret: RostraIdSecretKey,
    time: i64,
    raw: impl Into<EventContentRaw>,
) -> VerifiedEventContent {
    let content = raw.into();
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(time).unwrap())
        .content(&content)
        .build()
        .signed_by(secret);
    let event = VerifiedEvent::verify_signed(secret.id(), event).unwrap();
    VerifiedEventContent::assume_verified(event, content)
}

fn labels(names: &[&str]) -> BTreeSet<PersonaTag> {
    names
        .iter()
        .map(|name| PersonaTag::new(*name).unwrap())
        .collect()
}

fn text(secret: RostraIdSecretKey, time: i64, names: &[&str]) -> VerifiedEventContent {
    post(
        secret,
        time,
        SocialPost::new_text("body".into(), None, labels(names))
            .serialize_cbor()
            .unwrap(),
    )
}

/// Retain even unsupported historical payloads without exercising ingestion's
/// deliberately different compatibility policy.
async fn retain(
    db: &Database,
    secret: RostraIdSecretKey,
    time: i64,
    raw: Vec<u8>,
) -> anyhow::Result<()> {
    let content = EventContentRaw::new(raw);
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(time)?)
        .content(&content)
        .build()
        .signed_by(secret);
    db.write_with(|tx| {
        let record = EventRecord { signed: event };
        tx.open_table(&crate::events::TABLE)?
            .insert(&event.compute_short_id(), &record)?;
        tx.open_table(&crate::content_store::TABLE)?.insert(
            &record.content_hash(),
            &crate::event::ContentStoreRecord(Cow::Owned(content.clone())),
        )?;
        tx.open_table(&crate::social_posts_by_time::TABLE)?
            .insert(&(record.timestamp(), event.compute_short_id()), &())?;
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn author_tags_pages_split_payload_without_loss_and_dedup_only_locally() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for time in [1, 2] {
        db.try_process_event_with_content(&text(secret, time, &["a", "b", "c", "d", "e"]))
            .await?;
    }
    let bounds = AuthorTagBounds {
        tags: 2,
        ..Default::default()
    };
    let mut cursor = None;
    let mut merged = BTreeSet::new();
    let mut pages = Vec::new();
    for _ in 0..10 {
        let page = db
            .discover_author_tags_page(secret.id(), cursor.as_ref(), bounds)
            .await?;
        assert!(page.tags.len() <= 2);
        merged.extend(page.tags.clone());
        pages.push(page.tags);
        cursor = page
            .next_cursor
            .map(|c| serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap());
        if cursor.is_none() {
            assert_eq!(page.stop, AuthorTagStop::End);
            break;
        }
    }
    assert!(cursor.is_none(), "continuation must make progress");
    assert_eq!(merged, labels(&["a", "b", "c", "d", "e"]));
    assert_eq!(pages[0], labels(&["a", "b"]));
    assert_eq!(pages[1], labels(&["c", "d"]));
    // A fresh row can emit previously observed tags; no global seen-set hidden
    // in the cursor and no truncation that drops the final tag of a payload.
    assert_eq!(pages[2], labels(&["a", "e"]));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn author_tags_bound_unrelated_and_repeated_rows_independently() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let other = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for time in 1..=80 {
        retain(
            &db,
            if time % 2 == 0 { secret } else { other },
            time,
            b"\xa1\x61p\x00".to_vec(),
        )
        .await?;
    }
    for (bounds, stop, count) in [
        (
            AuthorTagBounds {
                raw_rows: 7,
                ..Default::default()
            },
            AuthorTagStop::Rows,
            7,
        ),
        (
            AuthorTagBounds {
                bytes: AUTHOR_TAG_BYTES_MIN,
                ..Default::default()
            },
            AuthorTagStop::Bytes,
            1,
        ),
        (
            AuthorTagBounds {
                work: AUTHOR_TAG_WORK_MIN,
                ..Default::default()
            },
            AuthorTagStop::Work,
            1,
        ),
    ] {
        let page = db
            .discover_author_tags_page(secret.id(), None, bounds)
            .await?;
        assert_eq!(page.stop, stop);
        assert_eq!(page.raw_rows_scanned, count);
        assert_eq!(page.bytes_reserved, count * AUTHOR_TAG_BYTES_MIN);
        assert_eq!(page.work_reserved, count * AUTHOR_TAG_WORK_MIN);
        assert!(page.next_cursor.is_some());
        assert_eq!(page.tags, labels(&["personal"]));
    }
    let unknown = RostraId::from_bytes([17; 32]);
    let page = db
        .discover_author_tags_page(
            unknown,
            None,
            AuthorTagBounds {
                raw_rows: 3,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(page.raw_rows_scanned, 3);
    assert!(page.tags.is_empty());
    assert!(page.next_cursor.is_some());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn author_tags_unsupported_oversize_and_unavailable_are_not_false_empty() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for (time, raw) in [
        (1, b"\xa1\x61p\x01".to_vec()),
        (2, b"\xa0".to_vec()), // genuinely no effective tag
        (3, b"\xa1\x61t\x81\x60".to_vec()),
        (4, b"\xa1\x61t\x9b\xff\xff\xff\xff\xff\xff\xff\xff".to_vec()),
        (5, vec![0; 70_000]),
    ] {
        retain(&db, secret, time, raw).await?;
    }
    let missing = text(secret, 6, &["missing"]);
    db.try_process_event_with_content(&missing).await?;
    db.write_with(|tx| {
        tx.open_table(&crate::content_store::TABLE)?
            .remove(&missing.content_hash())?;
        Ok(())
    })
    .await?;
    let page = db
        .discover_author_tags_page(secret.id(), None, AuthorTagBounds::default())
        .await?;
    assert_eq!(page.stop, AuthorTagStop::End);
    assert_eq!(page.tags, labels(&["professional"]));
    assert_eq!(page.skipped.unsupported, 2);
    assert_eq!(page.skipped.oversized, 1);
    assert_eq!(page.skipped.unavailable, 1);
    assert_eq!(page.raw_rows_scanned, 6);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn author_tags_live_resume_revalidates_deleted_and_unavailable_payloads() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let old = text(secret, 1, &["old"]);
    let split = text(secret, 2, &["a", "b", "c"]);
    db.try_process_event_with_content(&old).await?;
    db.try_process_event_with_content(&split).await?;
    let first = db
        .discover_author_tags_page(
            secret.id(),
            None,
            AuthorTagBounds {
                tags: 1,
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(first.tags, labels(&["a"]));
    // Newer content is not part of a traversal already past that position.
    db.try_process_event_with_content(&text(secret, 3, &["new"]))
        .await?;
    db.write_with(|tx| {
        tx.open_table(&crate::events_content_state::TABLE)?.insert(
            &split.event_id().to_short(),
            &crate::event::EventContentState::Pruned,
        )?;
        Ok(())
    })
    .await?;
    let resumed = db
        .discover_author_tags_page(
            secret.id(),
            first.next_cursor.as_ref(),
            AuthorTagBounds::default(),
        )
        .await?;
    assert_eq!(resumed.tags, labels(&["old"]));
    assert_eq!(resumed.skipped.unavailable, 1);
    assert_eq!(resumed.stop, AuthorTagStop::End);
    db.write_with(|tx| {
        tx.open_table(&crate::social_posts_by_time::TABLE)?
            .remove(&(split.timestamp(), split.event_id().to_short()))?;
        Ok(())
    })
    .await?;
    let resumed = db
        .discover_author_tags_page(
            secret.id(),
            first.next_cursor.as_ref(),
            AuthorTagBounds::default(),
        )
        .await?;
    assert_eq!(resumed.tags, labels(&["old"]));
    assert_eq!(resumed.skipped.unavailable, 0);
    let refreshed = db
        .discover_author_tags_page(secret.id(), None, AuthorTagBounds::default())
        .await?;
    assert_eq!(refreshed.tags, labels(&["new", "old"]));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn author_tags_cursor_scope_reopen_and_actual_rebuild() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("tags.redb");
    let db = Database::open(&path, secret.id()).await?;
    db.try_process_event_with_content(&text(secret, 1, &["a", "b", "c"]))
        .await?;
    let bounds = AuthorTagBounds {
        tags: 1,
        ..Default::default()
    };
    let cursor: AuthorTagCursor = db
        .discover_author_tags_page(secret.id(), None, bounds)
        .await?
        .next_cursor
        .unwrap();
    let other = RostraIdSecretKey::generate().id();
    assert!(matches!(
        db.discover_author_tags_page(other, Some(&cursor), bounds)
            .await,
        Err(DbError::AuthorTagCursorStale)
    ));
    let other_db = Database::new_in_memory(other).await?;
    assert!(matches!(
        other_db
            .discover_author_tags_page(secret.id(), Some(&cursor), bounds)
            .await,
        Err(DbError::AuthorTagCursorStale)
    ));
    db.write_with(|tx| {
        tx.open_table(&crate::social_notification_epoch::TABLE)?
            .insert(&(), &[19; 16])?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.discover_author_tags_page(secret.id(), Some(&cursor), bounds)
            .await,
        Err(DbError::AuthorTagCursorStale)
    ));
    let cursor = db
        .discover_author_tags_page(secret.id(), None, bounds)
        .await?
        .next_cursor;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert!(matches!(
        db.discover_author_tags_page(secret.id(), cursor.as_ref(), bounds)
            .await,
        Err(DbError::AuthorTagCursorStale)
    ));
    let before = db
        .discover_author_tags_page(secret.id(), None, AuthorTagBounds::default())
        .await?;
    let cursor = db
        .discover_author_tags_page(secret.id(), None, bounds)
        .await?
        .next_cursor;
    db.write_with(|tx| Database::prepare_total_migration(tx, 36))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert!(matches!(
        db.discover_author_tags_page(secret.id(), cursor.as_ref(), bounds)
            .await,
        Err(DbError::AuthorTagCursorStale)
    ));
    let after = db
        .discover_author_tags_page(secret.id(), None, AuthorTagBounds::default())
        .await?;
    assert_eq!(before.tags, after.tags);
    assert_eq!(after.tags, labels(&["a", "b", "c"]));
    assert_eq!(after.stop, AuthorTagStop::End);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn author_tags_invalid_bounds_and_corrupt_storage_fail_without_panics() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    for corruption in 0..9 {
        let db = Database::new_in_memory(secret.id()).await?;
        let post = text(secret, 1, &["a", "b"]);
        db.try_process_event_with_content(&post).await?;
        let key = bincode::encode_to_vec(
            (post.timestamp(), post.event_id().to_short()),
            redb_bincode::BINCODE_CONFIG,
        )?;
        let event_key =
            bincode::encode_to_vec(post.event_id().to_short(), redb_bincode::BINCODE_CONFIG)?;
        let content_key =
            bincode::encode_to_vec(post.content_hash(), redb_bincode::BINCODE_CONFIG)?;
        db.write_with(|tx| {
            match corruption {
                0..=2 => {
                    let mut table = tx
                        .as_raw()
                        .open_table(crate::social_posts_by_time::TABLE.as_raw())?;
                    table.remove(key.as_slice())?;
                    let mut bad_key = key.clone();
                    let value: &[u8] = if corruption == 2 { &[0] } else { &[] };
                    if corruption == 0 {
                        bad_key.pop();
                    } else if corruption == 1 {
                        bad_key.push(0);
                    }
                    table.insert(bad_key.as_slice(), value)?;
                }
                3 => {
                    tx.as_raw()
                        .open_table(crate::events::TABLE.as_raw())?
                        .insert(event_key.as_slice(), &[0][..])?;
                }
                4 => {
                    tx.as_raw()
                        .open_table(crate::events_content_state::TABLE.as_raw())?
                        .insert(event_key.as_slice(), &[255][..])?;
                }
                5..=7 => {
                    let mut table = tx
                        .as_raw()
                        .open_table(crate::content_store::TABLE.as_raw())?;
                    let raw = table.get(content_key.as_slice())?.unwrap();
                    let mut bad = raw.value().to_vec();
                    drop(raw);
                    match corruption {
                        5 => bad.push(0),
                        6 => {
                            // Valid wrapper but wrong retained content hash.
                            *bad.last_mut().unwrap() ^= 1;
                        }
                        7 => {
                            bad = vec![255; 9]; // forged enormous byte-string length
                        }
                        _ => unreachable!(),
                    }
                    table.insert(content_key.as_slice(), bad.as_slice())?;
                }
                8 => {
                    let mut table = tx
                        .as_raw()
                        .open_table(crate::social_posts_by_time::TABLE.as_raw())?;
                    table.remove(key.as_slice())?;
                    // Bincode accepts this nonminimal big-endian varint for
                    // timestamp 1, but re-encoding the cursor changes raw order.
                    assert_eq!(key[0], 1);
                    let mut bad_key = vec![251, 0, 1];
                    bad_key.extend_from_slice(&key[1..]);
                    table.insert(bad_key.as_slice(), &[][..])?;
                }
                _ => unreachable!(),
            }
            Ok(())
        })
        .await?;
        assert!(
            db.discover_author_tags_page(
                secret.id(),
                None,
                AuthorTagBounds {
                    raw_rows: 1,
                    tags: 1,
                    ..Default::default()
                },
            )
            .await
            .is_err(),
            "corruption {corruption}"
        );
    }
    for bounds in [
        AuthorTagBounds {
            raw_rows: 0,
            ..Default::default()
        },
        AuthorTagBounds {
            raw_rows: usize::MAX,
            ..Default::default()
        },
        AuthorTagBounds {
            bytes: AUTHOR_TAG_BYTES_MIN - 1,
            ..Default::default()
        },
        AuthorTagBounds {
            work: AUTHOR_TAG_WORK_MIN - 1,
            ..Default::default()
        },
        AuthorTagBounds {
            tags: 33,
            ..Default::default()
        },
    ] {
        assert!(matches!(bounds.validate(), Err(DbError::AuthorTagBounds)));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn author_tags_materialization_and_author_deletion_match_existing_discovery()
-> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let original = text(secret, 1, &["original"]);
    db.try_process_event_with_content(&original).await?;
    let replacement = SocialPost::new_text("edited".into(), None, labels(&["replacement"]));
    let (event, raw) = Event::builder(&replacement)
        .author(secret.id())
        .timestamp(time::OffsetDateTime::from_unix_timestamp(2)?)
        .parent_prev(original.event_id().to_short())
        .delete(original.event_id().to_short())
        .build()?;
    let event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))?;
    let replacement = VerifiedEventContent::assume_verified(event, raw);
    db.try_process_event_with_content(&replacement).await?;
    let page = db
        .discover_author_tags_page(secret.id(), None, AuthorTagBounds::default())
        .await?;
    assert_eq!(page.tags, labels(&["replacement"]));
    assert_eq!(page.tags, db.get_persona_tags_for_id(secret.id()).await);

    let empty = EventContentRaw::new(vec![]);
    let deleting = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(3)?)
        .parent_prev(replacement.event_id().to_short())
        .delete(replacement.event_id().to_short())
        .content(&empty)
        .build()
        .signed_by(secret);
    let deleting = VerifiedEvent::verify_signed(secret.id(), deleting)?;
    db.try_process_event(&deleting).await?;
    let page = db
        .discover_author_tags_page(secret.id(), None, AuthorTagBounds::default())
        .await?;
    assert!(page.tags.is_empty());
    assert_eq!(page.stop, AuthorTagStop::End);
    assert_eq!(page.raw_rows_scanned, 0);
    assert_eq!(page.tags, db.get_persona_tags_for_id(secret.id()).await);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn author_tags_accept_materialized_singleton_posts_even_for_unrelated_authors()
-> anyhow::Result<()> {
    let owner = RostraIdSecretKey::generate();
    let other = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(owner.id()).await?;
    let payload = SocialPost::new_text("singleton post".into(), None, labels(&["singleton"]))
        .serialize_cbor()?;
    let event = Event::builder_raw_content()
        .author(other.id())
        .kind(EventKind::SOCIAL_POST)
        .singleton_aux_key(rostra_core::event::EventAuxKey::default())
        .timestamp(time::OffsetDateTime::from_unix_timestamp(1)?)
        .content(&payload)
        .build()
        .signed_by(other);
    let event = VerifiedEvent::verify_signed(other.id(), event)?;
    let post = VerifiedEventContent::assume_verified(event, payload);
    db.try_process_event_with_content(&post).await?;
    let owned = db
        .discover_author_tags_page(owner.id(), None, AuthorTagBounds::default())
        .await?;
    assert_eq!(owned.raw_rows_scanned, 1);
    assert!(owned.tags.is_empty());
    assert_eq!(owned.stop, AuthorTagStop::End);
    let peer = db
        .discover_author_tags_page(other.id(), None, AuthorTagBounds::default())
        .await?;
    assert_eq!(peer.tags, labels(&["singleton"]));
    assert_eq!(peer.tags, db.get_persona_tags_for_id(other.id()).await);
    Ok(())
}
