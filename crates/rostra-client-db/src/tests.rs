mod event_order;

use rostra_core::event::content_kind::{self, EventContentKind as _};
use rostra_core::event::{
    Event, EventContentRaw, EventExt as _, EventKind, PersonaSelector, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraId, RostraIdSecretKey, ToShort as _};
use rostra_core::{EventId, Timestamp};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;
use tempfile::{TempDir, tempdir};
use tracing::info;

use crate::event::EventContentState;
use crate::event_order::EventOrder;
use crate::{
    Database, content_rc, content_store, events, events_by_time, events_content_missing,
    events_content_state, events_heads, events_missing, ids_full,
};

pub(crate) async fn temp_db_rng() -> BoxedErrorResult<(TempDir, super::Database)> {
    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    temp_db(author).await
}

pub(crate) async fn temp_db(self_id: RostraId) -> BoxedErrorResult<(TempDir, super::Database)> {
    let dir = tempdir()?;
    let db = super::Database::open(dir.path().join("db.redb"), self_id)
        .await
        .boxed()?;

    Ok((dir, db))
}

fn build_test_event(
    id_secret: RostraIdSecretKey,
    parent: impl Into<Option<EventId>>,
) -> VerifiedEvent {
    let parent = parent.into();

    let content = EventContentRaw::new(vec![]);
    let author = id_secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .maybe_parent_prev(parent.map(Into::into))
        .content(&content)
        .build();

    let signed_event = event.signed_by(id_secret);

    VerifiedEvent::verify_signed(author, signed_event).expect("Valid event")
}

fn build_follow_event_content(
    secret: RostraIdSecretKey,
    followee: RostraId,
    timestamp: time::OffsetDateTime,
    parent: Option<EventId>,
) -> VerifiedEventContent {
    let content = content_kind::Follow {
        followee,
        persona: None,
        selector: Some(PersonaSelector::Except { ids: vec![] }),
        persona_tags_selector: None,
    }
    .serialize_cbor()
    .expect("Follow content must serialize");
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::FOLLOW)
        .content(&content)
        .timestamp(timestamp)
        .maybe_parent_prev(parent.map(Into::into))
        .build();
    let signed_event = event.signed_by(secret);
    let verified_event =
        VerifiedEvent::verify_signed(secret.id(), signed_event).expect("Valid event");

    VerifiedEventContent::assume_verified(verified_event, content)
}

fn build_post_event_content(
    secret: RostraIdSecretKey,
    timestamp: time::OffsetDateTime,
    parent: Option<EventId>,
    text: &str,
) -> VerifiedEventContent {
    let content = content_kind::SocialPost::new(text.to_owned(), None, Default::default())
        .serialize_cbor()
        .expect("Social post content must serialize");
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .content(&content)
        .timestamp(timestamp)
        .maybe_parent_prev(parent.map(Into::into))
        .build();
    let signed_event = event.signed_by(secret);
    let verified_event =
        VerifiedEvent::verify_signed(secret.id(), signed_event).expect("Valid event");

    VerifiedEventContent::assume_verified(verified_event, content)
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_store_event() -> BoxedErrorResult<()> {
    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let event_a = build_test_event(id_secret, None);
    let event_a_id = event_a.event_id;
    let event_b = build_test_event(id_secret, event_a.event_id);
    let event_b_id = event_b.event_id;
    let event_c = build_test_event(id_secret, event_b.event_id);
    let event_c_id = event_c.event_id;
    let event_d = build_test_event(id_secret, event_c.event_id);
    let event_d_id = event_d.event_id;

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE).boxed()?;
        let mut events_table = tx.open_table(&events::TABLE).boxed()?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE).boxed()?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE).boxed()?;
        let mut content_store_table = tx.open_table(&content_store::TABLE).boxed()?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE).boxed()?;
        let mut events_content_missing_table =
            tx.open_table(&events_content_missing::TABLE).boxed()?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE).boxed()?;

        for (event, missing_expect, heads_expect) in [
            (event_a, vec![], vec![event_a_id]),
            (event_c, vec![event_b_id], vec![event_a_id, event_c_id]),
            (event_d, vec![event_b_id], vec![event_a_id, event_d_id]),
            (event_b, vec![], vec![event_d_id]),
        ] {
            let mut missing_expect: Vec<rostra_core::ShortEventId> =
                missing_expect.into_iter().map(Into::into).collect();
            let mut heads_expect: Vec<rostra_core::ShortEventId> =
                heads_expect.into_iter().map(Into::into).collect();
            missing_expect.sort_unstable();
            heads_expect.sort_unstable();

            // verify idempotency, just for for the sake of it
            for _ in 0..2 {
                info!(event_id = %event.event_id, "Inserting");
                Database::insert_event_tx(
                    event,
                    &mut ids_full_tbl,
                    &mut events_table,
                    &mut events_missing_table,
                    &mut events_heads_table,
                    &mut events_by_time_table,
                    &mut events_content_state_table,
                    &mut content_store_table,
                    &mut content_rc_table,
                    &mut events_content_missing_table,
                    None,
                )?;

                info!(event_id = %event.event_id, "Checking missing");
                let missing =
                    Database::get_missing_events_for_id_tx(author, &events_missing_table)?;
                missing
                    .iter()
                    .for_each(|missing| info!(%missing, "Missing"));

                assert_eq!(missing, missing_expect);
                info!(event_id = %event.event_id, "Checking heads");
                let heads = Database::get_heads_events_tx(author, &events_heads_table)?;
                heads.iter().for_each(|head| info!(%head, "Head"));
                assert_eq!(heads, heads_expect);
            }
        }
        Ok(())
    })
    .await?;

    Ok(())
}

fn build_test_event_2(
    id_secret: RostraIdSecretKey,
    parent: impl Into<Option<EventId>>,
    delete: impl Into<Option<EventId>>,
) -> VerifiedEvent {
    let parent = parent.into();
    let delete = delete.into();

    let content = EventContentRaw::from(vec![]);
    let author = id_secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .maybe_parent_prev(parent.map(Into::into))
        .maybe_delete(delete.map(Into::into))
        .content(&content)
        .build();

    let signed_event = event.signed_by(id_secret);

    VerifiedEvent::verify_signed(author, signed_event).expect("Valid event")
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_store_deleted_event() -> BoxedErrorResult<()> {
    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;

    let event_a = build_test_event_2(id_secret, None, None);
    let event_a_id = event_a.event_id;
    let event_b = build_test_event_2(id_secret, event_a.event_id, event_a_id);
    let event_b_id = event_b.event_id;
    let event_c = build_test_event_2(id_secret, event_b.event_id, event_a_id);
    let event_c_id = event_c.event_id;
    let event_d = build_test_event_2(id_secret, event_c.event_id, event_b_id);
    let event_d_id = event_d.event_id;
    let event_a_deleted_by = [
        (event_b.timestamp(), event_b_id.to_short()),
        (event_c.timestamp(), event_c_id.to_short()),
    ]
    .into_iter()
    .max()
    .expect("two direct deleters")
    .1;

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE).boxed()?;
        let mut events_table = tx.open_table(&events::TABLE).boxed()?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE).boxed()?;
        let mut content_store_table = tx.open_table(&content_store::TABLE).boxed()?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE).boxed()?;
        let mut events_content_missing_table =
            tx.open_table(&events_content_missing::TABLE).boxed()?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE).boxed()?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE).boxed()?;

        // All events have content_len=0. With the new behavior, deletion
        // of content_len=0 parents DOES set EventContentState::Deleted
        // (the parent_has_content guard was removed). When an event arrives
        // and was pre-marked as deleted in events_missing (by a delete event
        // that arrived first), the Deleted state IS set unconditionally.
        //
        // Insertion order: a, c, d, b.
        // - event_c deletes event_a → event_a gets Deleted { deleted_by: c }
        // - event_d deletes event_b (not yet present) → marks event_b as
        //   deleted-when-missing
        // - When event_b arrives:
        //   - event_b gets Deleted { deleted_by: d } (from missing marker)
        //   - event_b also deletes event_a; canonical timestamp/ID precedence selects
        //     between b and c
        for (event, expected_states) in [
            (event_a, [Some(None), None, None, None]),
            (
                event_c,
                [Some(Some(event_c_id.into())), None, Some(None), None],
            ),
            (
                event_d,
                [Some(Some(event_c_id.into())), None, Some(None), Some(None)],
            ),
            (
                event_b,
                [
                    Some(Some(event_a_deleted_by)),
                    // event_b: arrived with pending delete from event_d
                    Some(Some(event_d_id.into())),
                    Some(None),
                    Some(None),
                ],
            ),
        ] {
            // verify idempotency, just for for the sake of it
            info!(event_id = %event.event_id, "# Inserting");
            for _ in 0..2 {
                Database::insert_event_tx(
                    event,
                    &mut ids_full_tbl,
                    &mut events_table,
                    &mut events_missing_table,
                    &mut events_heads_table,
                    &mut events_by_time_table,
                    &mut events_content_state_table,
                    &mut content_store_table,
                    &mut content_rc_table,
                    &mut events_content_missing_table,
                    None,
                )?;

                for (event_id, expected_state) in [event_a_id, event_b_id, event_c_id, event_d_id]
                    .into_iter()
                    .zip(expected_states)
                {
                    info!(event_id = %event_id, "Checking");
                    let state = Database::get_event_tx(event_id, &events_table)?.map(|_record| {
                        let content_state = Database::get_event_content_state_tx(
                            event_id,
                            &events_content_state_table,
                        )
                        .expect("no db errors");
                        info!(event_id = %event_id, ?content_state, "State");

                        match content_state {
                            Some(EventContentState::Deleted { deleted_by }) => Some(deleted_by),
                            Some(_) => None,
                            None => None,
                        }
                    });

                    assert_eq!(state, expected_state);
                }
            }
        }
        Ok(())
    })
    .await?;

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_cross_author_parent_never_resolves_or_deletes() -> BoxedErrorResult<()> {
    let child_secret = RostraIdSecretKey::generate();
    let child_author = child_secret.id();
    let target_secret = RostraIdSecretKey::generate();
    let target_author = target_secret.id();

    let target_content = EventContentRaw::new(vec![1, 2, 3]);
    let target = Event::builder_raw_content()
        .author(target_author)
        .kind(EventKind::SOCIAL_POST)
        .content(&target_content)
        .build()
        .signed_by(target_secret);
    let target =
        VerifiedEvent::verify_signed(target_author, target).expect("target event is valid");
    let target_id = target.event_id.to_short();
    let target_content_hash = target.content_hash();

    let child_content = EventContentRaw::new(vec![]);
    let ordinary_child = Event::builder_raw_content()
        .author(child_author)
        .kind(EventKind::SOCIAL_POST)
        .parent_prev(target_id)
        .content(&child_content)
        .build()
        .signed_by(child_secret);
    let ordinary_child = VerifiedEvent::verify_signed(child_author, ordinary_child)
        .expect("ordinary child is valid");
    let deleting_child = Event::builder_raw_content()
        .author(child_author)
        .kind(EventKind::SOCIAL_POST)
        .delete(target_id)
        .content(&child_content)
        .build()
        .signed_by(child_secret);
    let deleting_child = VerifiedEvent::verify_signed(child_author, deleting_child)
        .expect("deleting child is valid");

    for (name, child, target_first) in [
        ("target then ordinary child", ordinary_child, true),
        ("ordinary child then target", ordinary_child, false),
        ("target then deleting child", deleting_child, true),
        ("deleting child then target", deleting_child, false),
    ] {
        let (_dir, db) = temp_db(child_author).await?;

        if target_first {
            db.process_event(&target).await;
            db.process_event(&child).await;
        } else {
            db.process_event(&child).await;
            db.process_event(&target).await;
        }

        db.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let events_missing_table = tx.open_table(&events_missing::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_rc_table = tx.open_table(&content_rc::TABLE)?;

            let stored_target = Database::get_event_tx(target_id, &events_table)?
                .unwrap_or_else(|| panic!("{name}: target must remain stored"));
            assert_eq!(
                stored_target.author(),
                target_author,
                "{name}: target author changed"
            );

            let target_state =
                Database::get_event_content_state_tx(target_id, &events_content_state_table)?;
            assert!(
                matches!(target_state, Some(EventContentState::Missing { .. })),
                "{name}: cross-author child changed target state to {target_state:?}"
            );
            assert_eq!(
                Database::get_content_rc_tx(target_content_hash, &content_rc_table)?,
                1,
                "{name}: cross-author child changed target content reference count"
            );

            let missing_parent = events_missing_table
                .get(&(child_author, target_id))?
                .map(|record| record.value())
                .unwrap_or_else(|| panic!("{name}: parent must remain missing for child author"));
            assert_eq!(
                missing_parent.deleted_by,
                child
                    .is_delete_parent_aux_content_set()
                    .then_some(child.event_id.to_short()),
                "{name}: missing-parent deletion intent changed"
            );
            assert!(
                events_missing_table
                    .get(&(target_author, target_id))?
                    .is_none(),
                "{name}: target must not be missing from its own graph"
            );

            Ok(())
        })
        .await?;
    }

    Ok(())
}

/// Test content reference counting by ContentHash.
///
/// The new content deduplication system tracks RC by content hash, not event
/// ID. Multiple events with the same content share a single content_store
/// entry.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_content_reference_counting() -> BoxedErrorResult<()> {
    use rostra_core::event::EventContentRaw;

    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;

    // Create a fake content hash to test RC tracking (use EventContentRaw to
    // compute hash)
    let test_content = EventContentRaw::new(vec![1u8; 32]);
    let test_content_hash = test_content.compute_content_hash();

    db.write_with(|tx| {
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;

        // Test initial state - no reference count should exist
        let initial_count = Database::get_content_rc_tx(test_content_hash, &content_rc_table)?;
        assert_eq!(initial_count, 0, "Initial count should be 0");

        // Insert first content reference
        Database::increment_content_rc_tx(test_content_hash, &mut content_rc_table)?;
        let count_after_first = Database::get_content_rc_tx(test_content_hash, &content_rc_table)?;
        assert_eq!(
            count_after_first, 1,
            "Count should be 1 after first increment"
        );

        // Insert second content reference (simulating another event with same content)
        Database::increment_content_rc_tx(test_content_hash, &mut content_rc_table)?;
        let count_after_second = Database::get_content_rc_tx(test_content_hash, &content_rc_table)?;
        assert_eq!(
            count_after_second, 2,
            "Count should be 2 after second increment"
        );

        // Insert third content reference
        Database::increment_content_rc_tx(test_content_hash, &mut content_rc_table)?;
        let count_after_third = Database::get_content_rc_tx(test_content_hash, &content_rc_table)?;
        assert_eq!(
            count_after_third, 3,
            "Count should be 3 after third increment"
        );

        // Remove first reference - count should go to 2
        let remaining =
            Database::decrement_content_rc_tx(test_content_hash, &mut content_rc_table)?;
        assert_eq!(remaining, 2, "Remaining count should be 2");

        let count_after_first_decrement =
            Database::get_content_rc_tx(test_content_hash, &content_rc_table)?;
        assert_eq!(
            count_after_first_decrement, 2,
            "Count should be 2 after first decrement"
        );

        // Remove second reference - count should go to 1
        let remaining =
            Database::decrement_content_rc_tx(test_content_hash, &mut content_rc_table)?;
        assert_eq!(remaining, 1, "Remaining count should be 1");

        // Remove third reference - count should go to 0 and entry removed
        let remaining =
            Database::decrement_content_rc_tx(test_content_hash, &mut content_rc_table)?;
        assert_eq!(remaining, 0, "Remaining count should be 0");

        // RC entry should be removed when count reaches 0
        let final_count = Database::get_content_rc_tx(test_content_hash, &content_rc_table)?;
        assert_eq!(final_count, 0, "Count should be 0 after all decrements");

        // Verify the entry was actually removed from the table
        let rc_entry_exists = content_rc_table.get(&test_content_hash)?.is_some();
        assert!(
            !rc_entry_exists,
            "RC entry should be removed when count reaches 0"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Event with content_len=0 arrives (no content to track).
///
/// Flow (content_len=0 events skip Missing state and payload tracking):
/// 1. Event is inserted — no Missing state, no RC increment, not in
///    events_content_missing
/// 2. Content is stored manually (empty content)
/// 3. Content availability check behaves correctly
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_event_arrives_before_content() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

    use rostra_core::Timestamp;
    use rostra_core::id::ToShort;

    use crate::event::ContentStoreRecord;

    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;

    let event = build_test_event(id_secret, None);
    let event_id = event.event_id.to_short();
    let content_hash = event.content_hash();

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;

        // Step 1: Insert event - content not in store yet
        Database::insert_event_tx(
            event,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        // Verify: Event should NOT be in events_content_missing (content_len=0)
        assert!(
            events_content_missing_table
                .get(&(Timestamp::ZERO, event_id))?
                .is_none(),
            "content_len=0 event should NOT be in events_content_missing"
        );

        // Verify: No content state entry (content_len=0 skips Missing state)
        assert!(
            Database::get_event_content_state_tx(event_id, &events_content_state_table)?.is_none(),
            "content_len=0 event should have no content state entry"
        );

        // Verify: RC should be 1 (incremented for content_len=0 non-deleted events)
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 — incremented for content_len=0");

        // Step 2: Store content in content_store (simulating content arrival)
        let test_content = EventContentRaw::new(vec![]);
        content_store_table.insert(&content_hash, &ContentStoreRecord(Cow::Owned(test_content)))?;

        // Verify: Content is now available
        assert!(
            Database::is_content_available_for_event_tx(
                event_id,
                content_hash,
                &events_content_state_table,
                &content_store_table
            )?,
            "Content should be available now"
        );

        // Verify: RC still 1 (content_len=0 events now get RC tracking)
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 for content_len=0");

        // Verify: Still no content state entry
        assert!(
            Database::get_event_content_state_tx(event_id, &events_content_state_table)?.is_none(),
            "content_len=0 event should still have no content state entry"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Content exists when event arrives (immediate availability).
///
/// Flow (content_len=0 events skip Missing state and payload tracking):
/// 1. Content is pre-stored in content_store (from another event)
/// 2. Event is inserted — no RC increment, no Missing state (content_len=0)
/// 3. Event is NOT added to events_content_missing
/// 4. Content is immediately available (no state entry blocks it)
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_content_exists_when_event_arrives() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

    use rostra_core::Timestamp;
    use rostra_core::id::ToShort;

    use crate::event::ContentStoreRecord;

    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;

    let event = build_test_event(id_secret, None);
    let event_id = event.event_id.to_short();
    let content_hash = event.content_hash();

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;

        // Step 1: Pre-store content in content_store
        let test_content = EventContentRaw::new(vec![]);
        content_store_table.insert(&content_hash, &ContentStoreRecord(Cow::Owned(test_content)))?;

        // Step 2: Insert event - content already exists
        Database::insert_event_tx(
            event,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        // Verify: Event should NOT be in events_content_missing
        assert!(
            events_content_missing_table
                .get(&(Timestamp::ZERO, event_id))?
                .is_none(),
            "Event should NOT be in events_content_missing"
        );

        // Verify: No content state entry (content_len=0 skips Missing state)
        let state = Database::get_event_content_state_tx(event_id, &events_content_state_table)?;
        assert!(
            state.is_none(),
            "content_len=0 event should have no content state entry"
        );

        // Verify: RC should be 1 (incremented for content_len=0 non-deleted events)
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 — incremented for content_len=0");

        // Verify: Content is available (no state entry blocks it, and
        // content is in the store)
        assert!(
            Database::is_content_available_for_event_tx(
                event_id,
                content_hash,
                &events_content_state_table,
                &content_store_table
            )?,
            "Content should be available"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Multiple events with same content hash share storage.
///
/// Flow (content_len=0 events skip Missing state and RC tracking):
/// 1. Event A arrives — no Missing state, no RC increment, not in missing
/// 2. Content arrives (stored manually)
/// 3. Event B arrives — no RC increment either
/// 4. Prune A -> Pruned state set (but RC stays 0)
/// 5. Prune B -> Pruned state set
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_multiple_events_share_content() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

    use rostra_core::Timestamp;
    use rostra_core::id::ToShort;

    use crate::event::ContentStoreRecord;

    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;

    // Create two events with the same content hash (empty content)
    let event_a = build_test_event(id_secret, None);
    let event_a_id = event_a.event_id.to_short();
    let event_b = build_test_event(id_secret, event_a.event_id);
    let event_b_id = event_b.event_id.to_short();
    let content_hash = event_a.content_hash();

    assert_eq!(
        content_hash,
        event_b.content_hash(),
        "Both events should have same content hash"
    );

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;

        // Step 1: Event A arrives - no content in store
        Database::insert_event_tx(
            event_a,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        // content_len=0: RC incremented, not in missing, no content state
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            1,
            "RC=1 after A arrives (content_len=0 now gets RC increment)"
        );
        assert!(
            events_content_missing_table
                .get(&(Timestamp::ZERO, event_a_id))?
                .is_none(),
            "A should NOT be in missing (content_len=0)"
        );

        // Step 2: Content arrives - store it if not already present
        // (but A already stored empty content, so this is a no-op check)
        let test_content = EventContentRaw::new(vec![]);
        if content_store_table.get(&content_hash)?.is_none() {
            content_store_table
                .insert(&content_hash, &ContentStoreRecord(Cow::Owned(test_content)))?;
        }

        // RC unchanged (still 1)
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            1,
            "RC=1 - content arrival doesn't change RC"
        );

        // Step 3: Event B arrives - content already exists (and content_len=0)
        Database::insert_event_tx(
            event_b,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        // content_len=0: RC increment for B too (now 2)
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            2,
            "RC=2 after B arrives (both A and B increment RC)"
        );
        assert!(
            events_content_missing_table
                .get(&(Timestamp::ZERO, event_b_id))?
                .is_none(),
            "B should NOT be in missing (content_len=0)"
        );
        // No content state entry for content_len=0 events
        assert!(
            Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?
                .is_none(),
            "B should have no content state entry (content_len=0)"
        );

        // Note: RC tracking now applies to content_len=0 events too.

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Multiple content_len=0 events with same content hash.
///
/// Flow (content_len=0 events skip Missing state and RC tracking):
/// 1. Event A and B arrive — neither in missing, RC=0
/// 2. Content is stored manually
/// 3. Both events can access content (no state entry blocks them)
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_multiple_events_waiting_for_content() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

    use rostra_core::Timestamp;
    use rostra_core::id::ToShort;

    use crate::event::ContentStoreRecord;

    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;

    let event_a = build_test_event(id_secret, None);
    let event_a_id = event_a.event_id.to_short();
    let event_b = build_test_event(id_secret, event_a.event_id);
    let event_b_id = event_b.event_id.to_short();
    let content_hash = event_a.content_hash();

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;

        // Step 1: Both events arrive - no content in store
        Database::insert_event_tx(
            event_a,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;
        Database::insert_event_tx(
            event_b,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        // content_len=0: neither in missing, RC=2 (both events increment)
        assert!(
            events_content_missing_table
                .get(&(Timestamp::ZERO, event_a_id))?
                .is_none()
        );
        assert!(
            events_content_missing_table
                .get(&(Timestamp::ZERO, event_b_id))?
                .is_none()
        );
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            2,
            "RC=2 — both content_len=0 events increment RC"
        );

        // Step 2: Content is already in store (A and B both stored it),
        // so this is just a check
        let test_content = EventContentRaw::new(vec![]);
        if content_store_table.get(&content_hash)?.is_none() {
            content_store_table
                .insert(&content_hash, &ContentStoreRecord(Cow::Owned(test_content)))?;
        }

        // RC unchanged (still 2)
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            2
        );

        // Step 3: Both events can now access content (no state entry blocks
        // them, and content is in the store)
        assert!(
            Database::is_content_available_for_event_tx(
                event_a_id,
                content_hash,
                &events_content_state_table,
                &content_store_table
            )?,
            "Content should be available for A"
        );
        assert!(
            Database::is_content_available_for_event_tx(
                event_b_id,
                content_hash,
                &events_content_state_table,
                &content_store_table
            )?,
            "Content should be available for B"
        );

        // No content state entries for content_len=0 events
        assert!(
            Database::get_event_content_state_tx(event_a_id, &events_content_state_table)?
                .is_none(),
            "A should have no content state entry"
        );
        assert!(
            Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?
                .is_none(),
            "B should have no content state entry"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Delete event arrives before its target (out-of-order).
///
/// Verifies that when a delete event arrives before its target:
/// - The target is marked as "to be deleted" in events_missing
/// - Non-delete events with parent_aux don't mark their parents as deleted
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_delete_event_arrives_before_target() -> BoxedErrorResult<()> {
    use rostra_core::id::ToShort as _;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Create fake event IDs for events that don't exist yet
    // We'll use these as parent references
    let fake_event_a = {
        let content = EventContentRaw::new(vec![1, 2, 3]);
        let event = Event::builder_raw_content()
            .author(author)
            .kind(EventKind::SOCIAL_POST)
            .content(&content)
            .build();
        event.signed_by(id_secret)
    };
    let fake_event_a_id = fake_event_a.compute_id();

    let fake_event_d = {
        let content = EventContentRaw::new(vec![4, 5, 6]);
        let event = Event::builder_raw_content()
            .author(author)
            .kind(EventKind::SOCIAL_POST)
            .content(&content)
            .build();
        event.signed_by(id_secret)
    };
    let fake_event_d_id = fake_event_d.compute_id();

    // Event B: DELETE event targeting A (A doesn't exist yet)
    let event_b = {
        let content = EventContentRaw::new(vec![10, 11, 12]);
        let event = Event::builder_raw_content()
            .author(author)
            .kind(EventKind::SOCIAL_POST)
            .delete(fake_event_a_id.to_short()) // This sets delete flag AND parent_aux
            .content(&content)
            .build();
        let signed = event.signed_by(id_secret);
        VerifiedEvent::verify_signed(author, signed).expect("Valid event")
    };
    let event_b_id = event_b.event_id.to_short();

    // Event C: Non-delete event with parent_aux = D (D doesn't exist yet)
    let event_c = {
        let content = EventContentRaw::new(vec![13, 14, 15]);
        let event = Event::builder_raw_content()
            .author(author)
            .kind(EventKind::SOCIAL_POST)
            .parent_aux(fake_event_d_id.to_short()) // Just parent_aux, no delete flag
            .content(&content)
            .build();
        let signed = event.signed_by(id_secret);
        VerifiedEvent::verify_signed(author, signed).expect("Valid event")
    };

    // Event E: DELETE event but referencing F via parent_prev (not parent_aux)
    // Note: delete() sets parent_aux, so we need to manually construct this
    // Actually, looking at the builder, delete() sets BOTH the flag AND parent_aux
    // So we can't have a delete event with missing parent_prev but existing
    // parent_aux Let's test with: delete event B targeting A, and verify A is
    // marked deleted And: event C with parent_aux D (non-delete), verify D is
    // NOT marked deleted

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;

        // Insert delete event B (targeting missing A)
        Database::insert_event_tx(
            event_b,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        // Verify: A should be marked as missing with deleted_by = B
        let missing_a = events_missing_table
            .get(&(author, fake_event_a_id.to_short()))?
            .map(|g| g.value());
        assert!(
            missing_a.is_some(),
            "A should be in events_missing (referenced by B)"
        );
        assert_eq!(
            missing_a.unwrap().deleted_by,
            Some(event_b_id),
            "A should be marked as deleted_by = B (delete event targeting missing parent_aux)"
        );

        // Insert non-delete event C (with parent_aux = missing D)
        Database::insert_event_tx(
            event_c,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        // Verify: D should be marked as missing but WITHOUT deleted_by
        let missing_d = events_missing_table
            .get(&(author, fake_event_d_id.to_short()))?
            .map(|g| g.value());
        assert!(
            missing_d.is_some(),
            "D should be in events_missing (referenced by C)"
        );
        assert_eq!(
            missing_d.unwrap().deleted_by,
            None,
            "D should NOT be marked as deleted (C is not a delete event)"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Follow/unfollow timestamp ordering - newer timestamps replace older.
///
/// Verifies that:
/// - A follow with newer timestamp replaces older follow record
/// - A follow with an older event order is rejected
/// - Same logic applies to unfollows
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_follow_unfollow_timestamp_ordering() -> BoxedErrorResult<()> {
    use rostra_core::Timestamp;
    use rostra_core::event::content_kind;

    use crate::{ids_followees, ids_followers, ids_unfollowed};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let followee = RostraIdSecretKey::generate().id();
    let (_dir, db) = temp_db(author).await?;

    db.write_with(|tx| {
        let mut followees_table = tx.open_table(&ids_followees::TABLE)?;
        let mut followers_table = tx.open_table(&ids_followers::TABLE)?;
        let mut unfollowed_table = tx.open_table(&ids_unfollowed::TABLE)?;

        let ts_100 = Timestamp::from(100);
        let ts_200 = Timestamp::from(200);
        let ts_150 = Timestamp::from(150);

        // Initial follow at timestamp 100
        let follow_content = content_kind::Follow {
            followee,
            persona: None,
            selector: None,
            persona_tags_selector: None,
        };
        let result = Database::insert_follow_tx(
            author,
            EventOrder::new(ts_100, rostra_core::ShortEventId::ZERO),
            follow_content.clone(),
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(result, "Initial follow should succeed");

        // Verify the record exists with ts=100
        let record = followees_table.get(&(author, followee))?.unwrap().value();
        assert_eq!(record.latest_ts, ts_100);

        // Try to follow with older timestamp - should be rejected
        let result = Database::insert_follow_tx(
            author,
            EventOrder::new(Timestamp::from(50), rostra_core::ShortEventId::ZERO),
            follow_content.clone(),
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(!result, "Follow with older timestamp should be rejected");

        // The same timestamp and event ID is idempotently rejected.
        let result = Database::insert_follow_tx(
            author,
            EventOrder::new(ts_100, rostra_core::ShortEventId::ZERO),
            follow_content.clone(),
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(!result, "Duplicate follow should be rejected");

        // Follow with newer timestamp - should succeed
        let result = Database::insert_follow_tx(
            author,
            EventOrder::new(ts_200, rostra_core::ShortEventId::ZERO),
            follow_content,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(result, "Follow with newer timestamp should succeed");

        // Verify the record was updated
        let record = followees_table.get(&(author, followee))?.unwrap().value();
        assert_eq!(record.latest_ts, ts_200);

        // Now test unfollow timestamp ordering
        // Unfollow with older timestamp than current follow - should be rejected
        let result = Database::insert_unfollow_tx(
            author,
            EventOrder::new(ts_150, rostra_core::ShortEventId::ZERO),
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(
            !result,
            "Unfollow with timestamp older than follow should be rejected"
        );

        // Unfollow with newer timestamp - should succeed
        let ts_300 = Timestamp::from(300);
        let result = Database::insert_unfollow_tx(
            author,
            EventOrder::new(ts_300, rostra_core::ShortEventId::ZERO),
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(result, "Unfollow with newer timestamp should succeed");

        // Now there's an unfollowed record at ts_300
        // Try to follow with timestamp older than unfollowed - should be rejected
        let follow_content2 = content_kind::Follow {
            followee,
            persona: None,
            selector: None,
            persona_tags_selector: None,
        };
        let result = Database::insert_follow_tx(
            author,
            EventOrder::new(ts_200, rostra_core::ShortEventId::ZERO),
            follow_content2.clone(),
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(
            !result,
            "Follow with timestamp older than unfollow should be rejected"
        );

        // Follow with newer timestamp than unfollow - should succeed
        let ts_400 = Timestamp::from(400);
        let result = Database::insert_follow_tx(
            author,
            EventOrder::new(ts_400, rostra_core::ShortEventId::ZERO),
            follow_content2,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(
            result,
            "Follow with timestamp newer than unfollow should succeed"
        );

        // An unfollow equal to the old unfollow is still older than the current
        // follow and exercises the second stored-order guard.
        let result = Database::insert_unfollow_tx(
            author,
            EventOrder::new(ts_300, rostra_core::ShortEventId::ZERO),
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(
            !result,
            "Unfollow with timestamp older than current state should be rejected"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: get_random_self_event returns events correctly.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_get_random_self_event() -> BoxedErrorResult<()> {
    use rostra_core::id::ToShort as _;

    use crate::events_self;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Create some test events
    let event_a = build_test_event(id_secret, None);
    let event_b = build_test_event(id_secret, event_a.event_id);
    let event_a_short = event_a.event_id.to_short();
    let event_b_short = event_b.event_id.to_short();

    db.write_with(|tx| {
        let mut events_self_table = tx.open_table(&events_self::TABLE)?;

        // Empty table should return None
        let result = Database::get_random_self_event(&events_self_table)?;
        assert!(result.is_none(), "Empty table should return None");

        // Insert one event
        events_self_table.insert(&event_a_short, &())?;

        // Should return the only event
        let result = Database::get_random_self_event(&events_self_table)?;
        assert_eq!(result, Some(event_a_short), "Should return the only event");

        // Insert another event
        events_self_table.insert(&event_b_short, &())?;

        // Should return one of the two events (we can't predict which due to
        // randomness)
        let result = Database::get_random_self_event(&events_self_table)?;
        assert!(
            result == Some(event_a_short) || result == Some(event_b_short),
            "Should return one of the events"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: get_random_self_event exercises both search directions and fallback
/// paths.
///
/// By running many iterations with a single event, we exercise both primary
/// search directions and their fallbacks, since the random pivot determines
/// which branch is taken.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_get_random_self_event_fallback_paths() -> BoxedErrorResult<()> {
    use rostra_core::ShortEventId;
    use rostra_core::id::ToShort as _;

    use crate::events_self;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let event = build_test_event(id_secret, None);
    let event_short = event.event_id.to_short();

    db.write_with(|tx| {
        let mut events_self_table = tx.open_table(&events_self::TABLE)?;

        // Insert the single event
        events_self_table.insert(&event_short, &())?;

        // Run many iterations to ensure both random branches and fallback paths are
        // exercised. With a single event and random pivot, sometimes the
        // primary direction won't find it and the fallback will be used.
        for _ in 0..100 {
            let result = Database::get_random_self_event(&events_self_table)?;
            assert_eq!(
                result,
                Some(event_short),
                "Should always find the single event via primary or fallback path"
            );
        }

        // Test with extreme event IDs to ensure both primary paths work
        events_self_table.remove(&event_short)?;

        // Event near the start of the ID space (will be found by before_pivot primary)
        // Using from_bytes with a very low value (just above ZERO to avoid edge case)
        let low_event_id =
            ShortEventId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        events_self_table.insert(&low_event_id, &())?;

        for _ in 0..50 {
            let result = Database::get_random_self_event(&events_self_table)?;
            assert_eq!(
                result,
                Some(low_event_id),
                "Should find low event ID via primary or fallback"
            );
        }

        events_self_table.remove(&low_event_id)?;

        // Event near the end of the ID space (will be found by after_pivot primary)
        let high_event_id = ShortEventId::from_bytes([
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            0xFF, 0xFE,
        ]);
        events_self_table.insert(&high_event_id, &())?;

        for _ in 0..50 {
            let result = Database::get_random_self_event(&events_self_table)?;
            assert_eq!(
                result,
                Some(high_event_id),
                "Should find high event ID via primary or fallback"
            );
        }

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Duplicate unfollows with older timestamps are rejected.
///
/// Verifies that when an unfollow record already exists, attempting to unfollow
/// again with the same or older timestamp is rejected.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_duplicate_unfollow_rejected() -> BoxedErrorResult<()> {
    use rostra_core::Timestamp;

    use crate::{ids_followees, ids_followers, ids_unfollowed};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let followee = RostraIdSecretKey::generate().id();
    let (_dir, db) = temp_db(author).await?;

    db.write_with(|tx| {
        let mut followees_table = tx.open_table(&ids_followees::TABLE)?;
        let mut followers_table = tx.open_table(&ids_followers::TABLE)?;
        let mut unfollowed_table = tx.open_table(&ids_unfollowed::TABLE)?;

        let ts_100 = Timestamp::from(100);
        let ts_200 = Timestamp::from(200);

        // Initial unfollow at timestamp 100 (no prior follow)
        let result = Database::insert_unfollow_tx(
            author,
            EventOrder::new(ts_100, rostra_core::ShortEventId::ZERO),
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(result, "Initial unfollow should succeed");

        // Verify unfollow record exists
        let record = unfollowed_table.get(&(author, followee))?.unwrap().value();
        assert_eq!(record.ts, ts_100);

        // The same timestamp and event ID is idempotently rejected.
        let result = Database::insert_unfollow_tx(
            author,
            EventOrder::new(ts_100, rostra_core::ShortEventId::ZERO),
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(!result, "Duplicate unfollow should be rejected");

        // Try to unfollow with older timestamp - should be rejected
        let result = Database::insert_unfollow_tx(
            author,
            EventOrder::new(Timestamp::from(50), rostra_core::ShortEventId::ZERO),
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(!result, "Unfollow with older timestamp should be rejected");

        // Unfollow with newer timestamp - should succeed and update record
        let result = Database::insert_unfollow_tx(
            author,
            EventOrder::new(ts_200, rostra_core::ShortEventId::ZERO),
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(result, "Unfollow with newer timestamp should succeed");

        // Verify record was updated
        let record = unfollowed_table.get(&(author, followee))?.unwrap().value();
        assert_eq!(record.ts, ts_200);

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: insert_latest_value_tx respects timestamp ordering.
///
/// Verifies that older or duplicate event orders are rejected while newer
/// event orders update the stored value.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_insert_latest_value_timestamp_ordering() -> BoxedErrorResult<()> {
    use rostra_core::Timestamp;
    use rostra_core::id::ToShort as _;

    use crate::{IdSocialProfileRecord, social_profiles};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Create a fake event id for the profile record
    let event = build_test_event(id_secret, None);
    let event_short = event.event_id.to_short();

    db.write_with(|tx| {
        let mut profiles_table = tx.open_table(&social_profiles::TABLE)?;

        let ts_100 = Timestamp::from(100);
        let ts_200 = Timestamp::from(200);

        let profile_alice = IdSocialProfileRecord {
            event_id: event_short,
            display_name: "Alice".to_string(),
            bio: "".to_string(),
            avatar: None,
        };

        // Initial insert at timestamp 100
        let result = Database::insert_latest_value_tx(
            ts_100,
            &author,
            profile_alice.clone(),
            &mut profiles_table,
        )?;
        assert!(result, "Initial insert should succeed");

        // Verify the value was stored
        let record = profiles_table.get(&author)?.unwrap().value();
        assert_eq!(record.ts, ts_100);
        assert_eq!(record.inner.display_name, "Alice");

        let profile_bob = IdSocialProfileRecord {
            event_id: event_short,
            display_name: "Bob".to_string(),
            bio: "".to_string(),
            avatar: None,
        };

        // The same timestamp and event ID is idempotently rejected.
        let result = Database::insert_latest_value_tx(
            ts_100,
            &author,
            profile_bob.clone(),
            &mut profiles_table,
        )?;
        assert!(!result, "Duplicate value should be rejected");

        // Verify value unchanged
        let record = profiles_table.get(&author)?.unwrap().value();
        assert_eq!(record.inner.display_name, "Alice");

        let profile_charlie = IdSocialProfileRecord {
            event_id: event_short,
            display_name: "Charlie".to_string(),
            bio: "".to_string(),
            avatar: None,
        };

        // Try to insert with older timestamp - should be rejected
        let result = Database::insert_latest_value_tx(
            Timestamp::from(50),
            &author,
            profile_charlie,
            &mut profiles_table,
        )?;
        assert!(!result, "Insert with older timestamp should be rejected");

        // Verify value unchanged
        let record = profiles_table.get(&author)?.unwrap().value();
        assert_eq!(record.inner.display_name, "Alice");

        // Insert with newer timestamp - should succeed
        let result =
            Database::insert_latest_value_tx(ts_200, &author, profile_bob, &mut profiles_table)?;
        assert!(result, "Insert with newer timestamp should succeed");

        // Verify value was updated
        let record = profiles_table.get(&author)?.unwrap().value();
        assert_eq!(record.ts, ts_200);
        assert_eq!(record.inner.display_name, "Bob");

        Ok(())
    })
    .await?;

    Ok(())
}

// ============================================================================
// Data Usage Tracking Tests
// ============================================================================

/// Helper: build an event with garbage content that will fail validation.
fn build_test_event_with_invalid_content(
    id_secret: RostraIdSecretKey,
    parent: impl Into<Option<EventId>>,
    content_size: usize,
) -> (VerifiedEvent, EventContentRaw) {
    let parent = parent.into();
    let content = EventContentRaw::new(vec![0u8; content_size]);
    let author = id_secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .maybe_parent_prev(parent.map(Into::into))
        .content(&content)
        .build();
    let signed = event.signed_by(id_secret);
    let verified = VerifiedEvent::verify_signed(author, signed).expect("Valid event");
    (verified, content)
}

/// Helper: build an event with valid SocialPost content.
fn build_test_event_with_valid_content(
    id_secret: RostraIdSecretKey,
    parent: impl Into<Option<EventId>>,
    text: &str,
) -> (VerifiedEvent, EventContentRaw) {
    use rostra_core::event::content_kind;
    use rostra_core::event::content_kind::EventContentKind as _;

    let parent = parent.into();
    let post = content_kind::SocialPost::new(
        text.to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let content = post.serialize_cbor().expect("valid cbor");
    let author = id_secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .maybe_parent_prev(parent.map(Into::into))
        .content(&content)
        .build();
    let signed = event.signed_by(id_secret);
    let verified = VerifiedEvent::verify_signed(author, signed).expect("Valid event");
    (verified, content)
}

/// Helper: build a delete event targeting another event.
fn build_delete_event(
    id_secret: RostraIdSecretKey,
    parent: EventId,
    delete: EventId,
) -> VerifiedEvent {
    let content = EventContentRaw::new(vec![]);
    let author = id_secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .parent_prev(parent.into())
        .delete(delete.into())
        .content(&content)
        .build();
    let signed = event.signed_by(id_secret);
    VerifiedEvent::verify_signed(author, signed).expect("Valid event")
}

/// Build a deletion event with explicit content and signed timestamp.
fn build_delete_event_at(
    id_secret: RostraIdSecretKey,
    parent: EventId,
    delete: EventId,
    content: &EventContentRaw,
    timestamp: i64,
) -> VerifiedEvent {
    let author = id_secret.id();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .parent_prev(parent.into())
        .delete(delete.into())
        .content(content)
        .timestamp(
            time::OffsetDateTime::from_unix_timestamp(timestamp).expect("valid test timestamp"),
        )
        .build();
    let signed = event.signed_by(id_secret);
    VerifiedEvent::verify_signed(author, signed).expect("Valid event")
}

/// Return the canonical direct deleter recorded for an event.
async fn get_deleted_by(
    db: &Database,
    event_id: impl Into<rostra_core::ShortEventId>,
) -> BoxedErrorResult<rostra_core::ShortEventId> {
    let event_id = event_id.into();
    Ok(db
        .read_with(|tx| {
            let states = tx.open_table(&events_content_state::TABLE)?;
            Ok(
                match Database::get_event_content_state_tx(event_id, &states)? {
                    Some(EventContentState::Deleted { deleted_by }) => Some(deleted_by),
                    _ => None,
                },
            )
        })
        .await?
        .expect("event content must be deleted"))
}

/// Return an event content hash's current reference count.
async fn get_test_content_rc(
    db: &Database,
    content_hash: rostra_core::ContentHash,
) -> BoxedErrorResult<u64> {
    Ok(db
        .read_with(|tx| {
            let content_rc_table = tx.open_table(&content_rc::TABLE)?;
            Database::get_content_rc_tx(content_hash, &content_rc_table)
        })
        .await?)
}

/// Return the stored reply count for a social post.
async fn get_test_reply_count(
    db: &Database,
    event_id: impl Into<rostra_core::ShortEventId>,
) -> BoxedErrorResult<u64> {
    let event_id = event_id.into();
    Ok(db
        .read_with(|tx| {
            let social_posts = tx.open_table(&crate::social_posts::TABLE)?;
            Ok(social_posts
                .get(&event_id)?
                .map(|record| record.value().reply_count)
                .unwrap_or_default())
        })
        .await?)
}

const THREE_EVENT_PERMUTATIONS: [[usize; 3]; 6] = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
];

/// Ordinary children cannot erase a staged deletion in any delivery order.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_deletion_is_monotone_across_delivery_permutations() -> BoxedErrorResult<()> {
    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (target, target_content) =
        build_test_event_with_valid_content(id_secret, None, "monotone target");
    let target_id = target.event_id;
    let target_hash = target.content_hash();
    let deleting = build_delete_event(id_secret, target_id, target_id);
    let deleting_id = deleting.event_id.to_short();
    let ordinary = build_test_event(id_secret, target_id);
    let events = [deleting, ordinary, target];

    for permutation in THREE_EVENT_PERMUTATIONS {
        let (_dir, db) = temp_db(author).await?;
        for index in permutation {
            db.process_event(&events[index]).await;
        }

        let verified_content = rostra_core::event::VerifiedEventContent::assume_verified(
            target,
            target_content.clone(),
        );
        db.process_event_content(&verified_content).await;

        assert_eq!(get_deleted_by(&db, target_id).await?, deleting_id);
        assert_eq!(get_test_content_rc(&db, target_hash).await?, 0);
        assert!(!db.is_event_content_missing(target_id.to_short()).await);
        assert!(db.get_event_content(target_id).await.is_none());
    }

    Ok(())
}

/// Distinct-time direct deleters converge on the latest signed candidate.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_direct_deleters_converge_across_delivery_permutations() -> BoxedErrorResult<()> {
    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (target, target_content) =
        build_test_event_with_valid_content(id_secret, None, "canonical target");
    let target_id = target.event_id;
    let target_hash = target.content_hash();
    let empty = EventContentRaw::new(vec![]);
    let delete_1 = build_delete_event_at(id_secret, target_id, target_id, &empty, 1_000);
    let delete_2 = build_delete_event_at(id_secret, delete_1.event_id, target_id, &empty, 1_001);
    let expected = delete_2.event_id.to_short();
    let events = [delete_1, delete_2, target];

    for permutation in THREE_EVENT_PERMUTATIONS {
        let (_dir, db) = temp_db(author).await?;
        for index in permutation {
            db.process_event(&events[index]).await;
        }

        let verified_content = rostra_core::event::VerifiedEventContent::assume_verified(
            target,
            target_content.clone(),
        );
        db.process_event_content(&verified_content).await;

        assert_eq!(get_deleted_by(&db, target_id).await?, expected);
        assert_eq!(get_test_content_rc(&db, target_hash).await?, 0);
        assert!(!db.is_event_content_missing(target_id.to_short()).await);
        assert!(db.get_event_content(target_id).await.is_none());
    }

    Ok(())
}

/// Equal-time direct deleters use event ID as the canonical tie-break.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_equal_timestamp_deleters_use_event_id_tiebreak() -> BoxedErrorResult<()> {
    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (target, _) = build_test_event_with_valid_content(id_secret, None, "equal-time target");
    let target_id = target.event_id;
    let empty = EventContentRaw::new(vec![]);
    let delete_1 = build_delete_event_at(id_secret, target_id, target_id, &empty, 2_000);
    let delete_2 = build_delete_event_at(id_secret, delete_1.event_id, target_id, &empty, 2_000);
    let expected = delete_1
        .event_id
        .to_short()
        .max(delete_2.event_id.to_short());
    let events = [delete_1, delete_2, target];

    for permutation in THREE_EVENT_PERMUTATIONS {
        let (_dir, db) = temp_db(author).await?;
        for index in permutation {
            db.process_event(&events[index]).await;
        }

        assert_eq!(get_deleted_by(&db, target_id).await?, expected);
    }

    Ok(())
}

/// Attribution-only updates do not repeat processed-post projection reversion.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_deleter_attribution_update_does_not_repeat_reversion() -> BoxedErrorResult<()> {
    use rostra_core::ExternalEventId;
    use rostra_core::event::VerifiedEventContent;
    use rostra_core::event::content_kind::{EventContentKind as _, SocialPost};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let parent_content =
        SocialPost::new("parent".to_owned(), None, Default::default()).serialize_cbor()?;
    let parent = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .content(&parent_content)
        .build()
        .signed_by(id_secret);
    let parent = VerifiedEvent::verify_signed(author, parent).expect("valid parent event");

    let reply_content = SocialPost::new(
        "reply".to_owned(),
        Some(ExternalEventId::new(author, parent.event_id)),
        Default::default(),
    )
    .serialize_cbor()?;
    let reply = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .parent_prev(parent.event_id.into())
        .content(&reply_content)
        .build()
        .signed_by(id_secret);
    let reply = VerifiedEvent::verify_signed(author, reply).expect("valid reply event");

    db.process_event_with_content(&VerifiedEventContent::assume_verified(
        parent,
        parent_content,
    ))
    .await;
    db.process_event_with_content(&VerifiedEventContent::assume_verified(reply, reply_content))
        .await;
    assert_eq!(get_test_reply_count(&db, parent.event_id).await?, 1);

    let empty = EventContentRaw::new(vec![]);
    let delete_1 = build_delete_event_at(id_secret, reply.event_id, reply.event_id, &empty, 5_000);
    let delete_2 =
        build_delete_event_at(id_secret, delete_1.event_id, reply.event_id, &empty, 5_001);
    let losing_delete =
        build_delete_event_at(id_secret, delete_2.event_id, reply.event_id, &empty, 4_999);

    db.process_event(&delete_1).await;
    assert_eq!(
        get_deleted_by(&db, reply.event_id).await?,
        delete_1.event_id.to_short()
    );
    assert_eq!(get_test_reply_count(&db, parent.event_id).await?, 0);

    db.process_event(&delete_2).await;
    assert_eq!(
        get_deleted_by(&db, reply.event_id).await?,
        delete_2.event_id.to_short()
    );
    assert_eq!(get_test_reply_count(&db, parent.event_id).await?, 0);

    db.process_event(&losing_delete).await;
    assert_eq!(
        get_deleted_by(&db, reply.event_id).await?,
        delete_2.event_id.to_short()
    );
    assert_eq!(get_test_reply_count(&db, parent.event_id).await?, 0);

    Ok(())
}

/// Deleting a deleting event's content does not cancel its header effect.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_deletion_chain_preserves_header_effects() -> BoxedErrorResult<()> {
    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (target, _) = build_test_event_with_valid_content(id_secret, None, "chain target");
    let target_id = target.event_id;
    let (_, delete_1_content) =
        build_test_event_with_valid_content(id_secret, None, "deleting event content");
    let delete_1 = build_delete_event_at(id_secret, target_id, target_id, &delete_1_content, 3_000);
    let delete_1_id = delete_1.event_id;
    let delete_2 = build_delete_event_at(
        id_secret,
        delete_1_id,
        delete_1_id,
        &EventContentRaw::new(vec![]),
        3_001,
    );
    let delete_2_id = delete_2.event_id;
    let events = [target, delete_1, delete_2];

    for permutation in THREE_EVENT_PERMUTATIONS {
        let (_dir, db) = temp_db(author).await?;
        for index in permutation {
            db.process_event(&events[index]).await;
        }

        assert_eq!(
            get_deleted_by(&db, target_id).await?,
            delete_1_id.to_short()
        );
        assert_eq!(
            get_deleted_by(&db, delete_1_id).await?,
            delete_2_id.to_short()
        );
        assert!(db.get_event(target_id).await.is_some());
        assert!(db.get_event(delete_1_id).await.is_some());
        assert!(db.get_event(delete_2_id).await.is_some());
        assert!(!db.is_event_content_missing(target_id.to_short()).await);
        assert!(!db.is_event_content_missing(delete_1_id.to_short()).await);
    }

    let ordinary = build_test_event(id_secret, delete_1_id);
    let (_dir, db) = temp_db(author).await?;
    for event in [delete_2, ordinary, delete_1, target] {
        db.process_event(&event).await;
    }
    assert_eq!(
        get_deleted_by(&db, target_id).await?,
        delete_1_id.to_short()
    );
    assert_eq!(
        get_deleted_by(&db, delete_1_id).await?,
        delete_2_id.to_short()
    );

    Ok(())
}

/// Test: Inserting events increments metadata size and num (current and total).
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_new_event_metadata() -> BoxedErrorResult<()> {
    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let event_a = build_test_event(id_secret, None);
    let event_b = build_test_event(id_secret, event_a.event_id);

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;
        let mut ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;

        // Initially all zeros
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(usage.current_metadata_size, 0);
        assert_eq!(usage.current_metadata_num, 0);

        // Insert first event
        Database::insert_event_tx(
            event_a,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            Some(&mut ids_data_usage_table),
        )?;

        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(usage.current_metadata_size, Database::EVENT_METADATA_SIZE);
        assert_eq!(usage.total_metadata_size, Database::EVENT_METADATA_SIZE);
        assert_eq!(usage.current_metadata_num, 1);
        assert_eq!(usage.total_metadata_num, 1);

        // Insert second event
        Database::insert_event_tx(
            event_b,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            Some(&mut ids_data_usage_table),
        )?;

        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(
            usage.current_metadata_size,
            Database::EVENT_METADATA_SIZE * 2
        );
        assert_eq!(usage.total_metadata_size, Database::EVENT_METADATA_SIZE * 2);
        assert_eq!(usage.current_metadata_num, 2);
        assert_eq!(usage.total_metadata_num, 2);

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Empty-content events (content_len == 0) are treated as processed
/// immediately — no Missing state, no RC, no payload tracking.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_empty_content_event_skips_missing_state() -> BoxedErrorResult<()> {
    use rostra_core::Timestamp;
    use rostra_core::id::ToShort as _;

    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // build_test_event creates events with EventContentRaw::new(vec![]) →
    // content_len == 0
    let event = build_test_event(id_secret, None);
    let event_id = event.event_id;
    let content_hash = event.content_hash();
    assert_eq!(
        event.content_len(),
        0,
        "Test event should have content_len 0"
    );
    let now = rostra_core::Timestamp::now();

    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;

    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;

        // No entry in events_content_state (treated as processed)
        let state =
            Database::get_event_content_state_tx(event_id.to_short(), &events_content_state_table)?;
        assert!(
            state.is_none(),
            "Empty-content event should have no content state, got {state:?}"
        );

        // RC entry incremented (new behavior: content_len=0 events get RC tracking)
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "Empty-content event should increment RC");

        // Not in events_content_missing
        let missing = events_content_missing_table.get(&(Timestamp::ZERO, event_id.to_short()))?;
        assert!(
            missing.is_none(),
            "Empty-content event should not be in events_content_missing"
        );

        // Data usage: metadata and payload tracked (new behavior: content_len=0 events
        // tracked as payload)
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(usage.current_metadata_num, 1, "Should have 1 event");
        assert_eq!(usage.missing_payload_num, 0, "No missing payloads");
        assert_eq!(usage.missing_payload_size, 0, "No missing payload size");
        assert_eq!(
            usage.total_payload_num, 1,
            "1 total payload (content_len=0 tracked)"
        );
        assert_eq!(
            usage.total_content_size, 0,
            "No total content size (content_len=0)"
        );
        assert_eq!(
            usage.current_payload_num, 1,
            "1 current payload (content_len=0 tracked)"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: New event payload goes to missing + total; current stays 0.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_new_payload_starts_missing() -> BoxedErrorResult<()> {
    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let (event, _content) = build_test_event_with_invalid_content(id_secret, None, 500);
    let content_len = u64::from(event.content_len());

    let now = rostra_core::Timestamp::now();

    // Use process_event_tx which opens all tables internally
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;

    db.read_with(|tx| {
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;

        // Metadata tracked
        assert_eq!(usage.current_metadata_num, 1);
        assert_eq!(usage.total_metadata_num, 1);

        // Payload is in unprocessed + total, not current
        assert_eq!(usage.total_content_size, content_len);
        assert_eq!(usage.total_payload_num, 1);
        assert_eq!(usage.missing_payload_size, content_len);
        assert_eq!(usage.missing_payload_num, 1);
        assert_eq!(usage.current_content_size, 0);
        assert_eq!(usage.current_payload_num, 0);

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Processing content moves payload from unprocessed to current.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_payload_processing() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;

    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let (event, content) = build_test_event_with_valid_content(id_secret, None, "Test post");
    let content_len = u64::from(event.content_len());
    let now = rostra_core::Timestamp::now();

    // Insert event (payload starts as unprocessed)
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;

    // Process the content
    let verified_content = VerifiedEventContent::assume_verified(event, content);
    db.process_event_content(&verified_content).await;

    db.read_with(|tx| {
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;

        // Unprocessed should be 0 now
        assert_eq!(usage.missing_payload_size, 0);
        assert_eq!(usage.missing_payload_num, 0);

        // Current should have the payload
        assert_eq!(usage.current_content_size, content_len);
        assert_eq!(usage.current_payload_num, 1);

        // Total unchanged
        assert_eq!(usage.total_content_size, content_len);
        assert_eq!(usage.total_payload_num, 1);

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Deleting processed content moves from current to deleted.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_payload_deletion() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;

    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let (event, content) = build_test_event_with_valid_content(id_secret, None, "Delete me");
    let event_id = event.event_id;
    let content_len = u64::from(event.content_len());
    let now = rostra_core::Timestamp::now();

    // Insert and process the event
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;
    let verified_content = VerifiedEventContent::assume_verified(event, content);
    db.process_event_content(&verified_content).await;

    // Delete the event via a delete event (the delete event itself has
    // content_len=0, so it is NOT tracked as a payload)
    let delete_event = build_delete_event(id_secret, event_id, event_id);
    db.write_with(|tx| {
        db.process_event_tx(&delete_event, now, tx)?;
        Ok(())
    })
    .await?;

    db.read_with(|tx| {
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;

        // Current should have only the delete event (content_len=0)
        assert_eq!(usage.current_content_size, 0);
        assert_eq!(usage.current_payload_num, 1, "Delete event payload tracked");

        // Deleted should have the original payload
        assert_eq!(usage.deleted_payload_size, content_len);
        assert_eq!(usage.deleted_payload_num, 1);

        // Total includes both: original post + delete event (content_len=0 now tracked)
        assert_eq!(usage.total_content_size, content_len);
        assert_eq!(usage.total_payload_num, 2, "Original + delete event");

        // No missing payloads
        assert_eq!(usage.missing_payload_size, 0);
        assert_eq!(usage.missing_payload_num, 0);

        // 2 events: original + delete
        assert_eq!(usage.current_metadata_num, 2);

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Deleting missing (unprocessed) content moves from missing to deleted.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_missing_payload_deletion() -> BoxedErrorResult<()> {
    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let (event, _content) = build_test_event_with_invalid_content(id_secret, None, 500);
    let event_id = event.event_id;
    let content_len = u64::from(event.content_len());
    let now = rostra_core::Timestamp::now();

    // Insert event (content stays missing — we don't call
    // process_event_content)
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;

    // Delete the event
    let delete_event = build_delete_event(id_secret, event_id, event_id);
    db.write_with(|tx| {
        db.process_event_tx(&delete_event, now, tx)?;
        Ok(())
    })
    .await?;

    db.read_with(|tx| {
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;

        // Original payload moved from missing to deleted.
        // Delete event has content_len=0 but is now tracked as a payload.
        assert_eq!(usage.missing_payload_size, 0);
        assert_eq!(usage.missing_payload_num, 0);

        // Deleted should have the original payload
        assert_eq!(usage.deleted_payload_size, content_len);
        assert_eq!(usage.deleted_payload_num, 1);

        // Current should have the delete event (content_len=0)
        assert_eq!(usage.current_content_size, 0);
        assert_eq!(usage.current_payload_num, 1, "Delete event payload tracked");

        // Total includes both: original post + delete event (content_len=0 now tracked)
        assert_eq!(usage.total_content_size, content_len);
        assert_eq!(usage.total_payload_num, 2, "Original + delete event");

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Pruning processed content moves from current to pruned.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_payload_pruning() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;
    use rostra_core::id::ToShort as _;

    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let (event, content) = build_test_event_with_valid_content(id_secret, None, "Prune me");
    let event_id = event.event_id;
    let content_hash = event.content_hash();
    let content_len = u64::from(event.content_len());
    let now = rostra_core::Timestamp::now();

    // Insert and process the event
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;
    let verified_content = VerifiedEventContent::assume_verified(event, content);
    db.process_event_content(&verified_content).await;

    // Prune the content
    db.write_with(|tx| {
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;
        let mut ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;

        Database::prune_event_content_tx(
            event_id.to_short(),
            content_hash,
            &mut events_content_state_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            Some((author, content_len as u32, &mut ids_data_usage_table)),
        )?;
        Ok(())
    })
    .await?;

    db.read_with(|tx| {
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;

        // Current should be 0 (moved to pruned)
        assert_eq!(usage.current_content_size, 0);
        assert_eq!(usage.current_payload_num, 0);

        // Pruned should have the payload
        assert_eq!(usage.pruned_payload_size, content_len);
        assert_eq!(usage.pruned_payload_num, 1);

        // Total unchanged
        assert_eq!(usage.total_content_size, content_len);
        assert_eq!(usage.total_payload_num, 1);

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Pruning unprocessed content moves from unprocessed to pruned.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_unprocessed_payload_pruning() -> BoxedErrorResult<()> {
    use rostra_core::id::ToShort as _;

    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let (event, _content) = build_test_event_with_invalid_content(id_secret, None, 500);
    let event_id = event.event_id;
    let content_hash = event.content_hash();
    let content_len = u64::from(event.content_len());
    let now = rostra_core::Timestamp::now();

    // Insert event (content stays unprocessed)
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;

    // Prune the content
    db.write_with(|tx| {
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;
        let mut ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;

        Database::prune_event_content_tx(
            event_id.to_short(),
            content_hash,
            &mut events_content_state_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            Some((author, content_len as u32, &mut ids_data_usage_table)),
        )?;
        Ok(())
    })
    .await?;

    db.read_with(|tx| {
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;

        // Unprocessed should be 0 (moved to pruned)
        assert_eq!(usage.missing_payload_size, 0);
        assert_eq!(usage.missing_payload_num, 0);

        // Pruned should have the payload
        assert_eq!(usage.pruned_payload_size, content_len);
        assert_eq!(usage.pruned_payload_num, 1);

        // Current should still be 0
        assert_eq!(usage.current_content_size, 0);
        assert_eq!(usage.current_payload_num, 0);

        // Total unchanged
        assert_eq!(usage.total_content_size, content_len);
        assert_eq!(usage.total_payload_num, 1);

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Processing invalid content moves from unprocessed to invalid and
/// decrements RC.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_payload_invalid() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;

    use crate::{content_rc, ids_data_usage};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let (event, content) = build_test_event_with_invalid_content(id_secret, None, 500);
    let content_hash = event.content_hash();
    let content_len = u64::from(event.content_len());
    let now = rostra_core::Timestamp::now();

    // Insert event (payload starts as unprocessed)
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;

    // Process the content — it's invalid (all zeros, not valid CBOR)
    let verified_content = VerifiedEventContent::assume_verified(event, content);
    db.process_event_content(&verified_content).await;

    db.read_with(|tx| {
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;

        // Unprocessed should be 0 (moved to invalid)
        assert_eq!(usage.missing_payload_size, 0);
        assert_eq!(usage.missing_payload_num, 0);

        // Invalid should have the payload
        assert_eq!(usage.invalid_payload_size, content_len);
        assert_eq!(usage.invalid_payload_num, 1);

        // Current should be 0 (invalid content not stored)
        assert_eq!(usage.current_content_size, 0);
        assert_eq!(usage.current_payload_num, 0);

        // Total unchanged
        assert_eq!(usage.total_content_size, content_len);
        assert_eq!(usage.total_payload_num, 1);

        // RC entry removed (decremented to 0 on invalid)
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let rc = content_rc_table.get(&content_hash)?;
        assert!(
            rc.is_none(),
            "RC entry should be removed when count reaches 0"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Deleting invalid content moves from invalid to deleted without
/// changing RC (already decremented).
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_invalid_payload_deletion() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;

    use crate::{content_rc, ids_data_usage};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let (event, content) = build_test_event_with_invalid_content(id_secret, None, 500);
    let event_id = event.event_id;
    let content_hash = event.content_hash();
    let content_len = u64::from(event.content_len());
    let now = rostra_core::Timestamp::now();

    // Insert and process (invalid content)
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;
    let verified_content = VerifiedEventContent::assume_verified(event, content);
    db.process_event_content(&verified_content).await;

    // Delete the event
    let delete_event = build_delete_event(id_secret, event_id, event_id);
    db.write_with(|tx| {
        db.process_event_tx(&delete_event, now, tx)?;
        Ok(())
    })
    .await?;

    db.read_with(|tx| {
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;

        // Invalid should be 0 (moved to deleted)
        assert_eq!(usage.invalid_payload_size, 0);
        assert_eq!(usage.invalid_payload_num, 0);

        // Deleted should have the original payload
        assert_eq!(usage.deleted_payload_size, content_len);
        assert_eq!(usage.deleted_payload_num, 1);

        // Current should have the delete event (content_len=0)
        assert_eq!(usage.current_content_size, 0);
        assert_eq!(usage.current_payload_num, 1, "Delete event payload tracked");

        // Total includes both: original post + delete event (content_len=0 now tracked)
        assert_eq!(usage.total_content_size, content_len);
        assert_eq!(usage.total_payload_num, 2, "Original + delete event");

        // RC entry still removed (no double-decrement)
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let rc = content_rc_table.get(&content_hash)?;
        assert!(rc.is_none(), "RC entry should remain removed after delete");

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Follow, unfollow, and re-follow flow with event processing.
///
/// Verifies the complete lifecycle:
/// 1. User A follows User B - check followees/followers tables
/// 2. User A unfollows User B - check tables are updated
/// 3. User A re-follows User B - check tables are restored
///
/// This test processes events through the full event content processing path.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_follow_unfollow_refollow_flow() -> BoxedErrorResult<()> {
    use rostra_core::event::content_kind::{EventContentKind as _, Follow};
    use rostra_core::event::{
        Event, EventKind, PersonaSelector, VerifiedEvent, VerifiedEventContent,
    };

    use crate::{ids_followees, ids_followers, ids_unfollowed};

    let user_a_secret = RostraIdSecretKey::generate();
    let user_a = user_a_secret.id();
    let user_b_secret = RostraIdSecretKey::generate();
    let user_b = user_b_secret.id();

    let (_dir, db) = temp_db(user_a).await?;

    // Helper to create a follow event with explicit timestamp
    let make_follow_event = |secret: RostraIdSecretKey,
                             followee: rostra_core::id::RostraId,
                             selector: Option<PersonaSelector>,
                             timestamp: time::OffsetDateTime|
     -> (VerifiedEvent, rostra_core::event::EventContentRaw) {
        let follow = Follow {
            followee,
            persona: None,
            selector,
            persona_tags_selector: None,
        };
        let content = follow.serialize_cbor().expect("valid");
        let event = Event::builder_raw_content()
            .author(secret.id())
            .kind(EventKind::FOLLOW)
            .content(&content)
            .timestamp(timestamp)
            .build();
        let signed = event.signed_by(secret);
        let verified = VerifiedEvent::verify_signed(secret.id(), signed).expect("Valid event");
        (verified, content)
    };

    // Use explicit timestamps to ensure proper ordering (1-second resolution)
    let base_time = time::OffsetDateTime::now_utc();
    let follow_time = base_time;
    let unfollow_time = base_time + time::Duration::seconds(1);
    let refollow_time = base_time + time::Duration::seconds(2);

    // Step 1: User A follows User B (Follow All except none = follow all personas)
    let (follow_event_1, follow_content_1) = make_follow_event(
        user_a_secret,
        user_b,
        Some(PersonaSelector::Except { ids: vec![] }),
        follow_time,
    );

    // Insert the event first (without content in store - content arrives later)
    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;

        // Insert the event (content not in store yet)
        Database::insert_event_tx(
            follow_event_1,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        Ok(())
    })
    .await?;

    // Process the follow event content
    let verified_content_1 =
        VerifiedEventContent::assume_verified(follow_event_1, follow_content_1);
    db.process_event_content(&verified_content_1).await;

    // Verify: User A should be following User B
    db.write_with(|tx| {
        let followees_table = tx.open_table(&ids_followees::TABLE)?;
        let followers_table = tx.open_table(&ids_followers::TABLE)?;
        let unfollowed_table = tx.open_table(&ids_unfollowed::TABLE)?;

        // Check followees: (user_a, user_b) should exist
        assert!(
            followees_table.get(&(user_a, user_b))?.is_some(),
            "User A should be following User B after follow"
        );

        // Check followers: (user_b, user_a) should exist
        assert!(
            followers_table.get(&(user_b, user_a))?.is_some(),
            "User B should have User A as follower"
        );

        // No unfollowed record should exist
        assert!(
            unfollowed_table.get(&(user_a, user_b))?.is_none(),
            "No unfollow record should exist"
        );

        Ok(())
    })
    .await?;

    // Step 2: User A unfollows User B (Follow with no selector = unfollow)
    let (unfollow_event, unfollow_content) =
        make_follow_event(user_a_secret, user_b, None, unfollow_time);

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;

        Database::insert_event_tx(
            unfollow_event,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        Ok(())
    })
    .await?;

    // Process the unfollow event content
    let verified_content_2 =
        VerifiedEventContent::assume_verified(unfollow_event, unfollow_content);
    db.process_event_content(&verified_content_2).await;

    // Verify: User A should no longer be following User B
    db.write_with(|tx| {
        let followees_table = tx.open_table(&ids_followees::TABLE)?;
        let followers_table = tx.open_table(&ids_followers::TABLE)?;
        let unfollowed_table = tx.open_table(&ids_unfollowed::TABLE)?;

        // Followee record should be removed
        assert!(
            followees_table.get(&(user_a, user_b))?.is_none(),
            "User A should not be following User B after unfollow"
        );

        // Follower record should be removed
        assert!(
            followers_table.get(&(user_b, user_a))?.is_none(),
            "User B should not have User A as follower after unfollow"
        );

        // Unfollowed record should exist
        assert!(
            unfollowed_table.get(&(user_a, user_b))?.is_some(),
            "Unfollow record should exist"
        );

        Ok(())
    })
    .await?;

    // Step 3: User A re-follows User B (same selector as initial follow - tests
    // deduplication) This tests that even with content deduplication (same
    // content hash as initial follow), the event-specific processing (follow
    // table updates) still runs correctly.
    let (refollow_event, refollow_content) = make_follow_event(
        user_a_secret,
        user_b,
        Some(PersonaSelector::Except { ids: vec![] }),
        refollow_time,
    );

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;

        Database::insert_event_tx(
            refollow_event,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &mut content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        Ok(())
    })
    .await?;

    // Process the re-follow event content
    let verified_content_3 =
        VerifiedEventContent::assume_verified(refollow_event, refollow_content);
    db.process_event_content(&verified_content_3).await;

    // Verify: User A should be following User B again
    db.write_with(|tx| {
        let followees_table = tx.open_table(&ids_followees::TABLE)?;
        let followers_table = tx.open_table(&ids_followers::TABLE)?;
        let unfollowed_table = tx.open_table(&ids_unfollowed::TABLE)?;

        // Check followees: (user_a, user_b) should exist again
        assert!(
            followees_table.get(&(user_a, user_b))?.is_some(),
            "User A should be following User B after re-follow"
        );

        // Check followers: (user_b, user_a) should exist again
        assert!(
            followers_table.get(&(user_b, user_a))?.is_some(),
            "User B should have User A as follower after re-follow"
        );

        // Unfollowed record should be removed (follow with newer timestamp removes it)
        assert!(
            unfollowed_table.get(&(user_a, user_b))?.is_none(),
            "Unfollow record should be removed after re-follow"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

// ============================================================================
// Property-based testing for RC counting correctness
// ============================================================================

mod proptest_rc {
    use std::borrow::Cow;
    use std::collections::{HashMap, HashSet};

    use proptest::prelude::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent};
    use rostra_core::id::{RostraIdSecretKey, ToShort as _};
    use rostra_core::{ContentHash, ShortEventId};
    use tracing::debug;

    use crate::event::ContentStoreRecord;
    use crate::{
        Database, EventContentState, content_rc, content_store, events, events_by_time,
        events_content_missing, events_content_state, events_heads, events_missing, ids_full,
    };

    /// Represents a content payload for testing.
    #[derive(Debug, Clone)]
    struct TestContent {
        raw: EventContentRaw,
        hash: ContentHash,
    }

    impl TestContent {
        fn new(data: Vec<u8>) -> Self {
            let raw = EventContentRaw::new(data);
            let hash = raw.compute_content_hash();
            Self { raw, hash }
        }
    }

    /// Represents an event specification in the test DAG.
    #[derive(Debug, Clone)]
    struct TestEventSpec {
        /// Which of the 3 authors (0, 1, 2)
        author_idx: usize,
        /// Which content payload (0-9)
        content_idx: usize,
        /// Index of parent_prev in the generated events (None for first event
        /// of author)
        parent_prev_idx: Option<usize>,
        /// Index of parent_aux (for merging branches)
        parent_aux_idx: Option<usize>,
        /// Index of event whose content this event deletes (mutually exclusive
        /// with parent_aux)
        delete_idx: Option<usize>,
    }

    /// Calculates expected RC counts by examining event states.
    ///
    /// In the new model, an event contributes +1 to RC for its content_hash
    /// unless it is deleted or pruned. RC is managed at event insertion time.
    fn calculate_expected_rc(
        event_hashes: &[(ShortEventId, ContentHash)],
        events_content_state_table: &impl events_content_state::ReadableTable,
    ) -> crate::DbResult<HashMap<ContentHash, u64>> {
        let mut expected_rc: HashMap<ContentHash, u64> = HashMap::new();

        for (event_id, content_hash) in event_hashes {
            // Skip zero hash (events with no content)
            if *content_hash == ContentHash::ZERO {
                continue;
            }

            let state =
                Database::get_event_content_state_tx(*event_id, events_content_state_table)?;

            // Count events that are NOT deleted/pruned/invalid (new model: RC managed at
            // insertion). Events with no state or Unprocessed state contribute to RC.
            // Deleted, Pruned, and Invalid events don't contribute.
            let has_rc = match state {
                None => true,
                Some(EventContentState::Missing { .. }) => true,
                Some(
                    EventContentState::Deleted { .. }
                    | EventContentState::Pruned
                    | EventContentState::Invalid,
                ) => false,
            };

            if has_rc {
                *expected_rc.entry(*content_hash).or_insert(0) += 1;
            }
        }

        Ok(expected_rc)
    }

    /// Verifies that actual RC counts match expected RC counts.
    ///
    /// Returns an error message if there's a mismatch, None if everything
    /// matches.
    pub fn verify_rc_consistency(
        event_hashes: &[(ShortEventId, ContentHash)],
        events_content_state_table: &impl events_content_state::ReadableTable,
        content_rc_table: &impl content_rc::ReadableTable,
    ) -> crate::DbResult<Option<String>> {
        let expected_rc = calculate_expected_rc(event_hashes, events_content_state_table)?;

        // Collect all unique content hashes (excluding zero)
        let all_hashes: HashSet<ContentHash> = event_hashes
            .iter()
            .map(|(_, h)| *h)
            .filter(|h| *h != ContentHash::ZERO)
            .collect();

        let mut errors = Vec::new();

        for hash in all_hashes {
            let expected = expected_rc.get(&hash).copied().unwrap_or(0);
            let actual = Database::get_content_rc_tx(hash, content_rc_table)?;

            if expected != actual {
                errors.push(format!(
                    "ContentHash {hash:?}: expected RC={expected}, actual RC={actual}"
                ));
            }
        }

        if errors.is_empty() {
            Ok(None)
        } else {
            Ok(Some(errors.join("\n")))
        }
    }

    /// Generates a valid DAG of events for testing.
    ///
    /// Rules:
    /// - 3 authors, each with their own chain of events
    /// - Each author's events form a linked list via parent_prev
    /// - parent_aux can reference any earlier event by the same author
    /// - delete_idx can be set to delete an earlier event's content (mutually
    ///   exclusive with parent_aux)
    fn generate_event_dag(
        num_events: usize,
        rng_seed: u64,
    ) -> (Vec<TestEventSpec>, Vec<(usize, bool)>) {
        let mut rng = StdRng::seed_from_u64(rng_seed);
        let mut events: Vec<TestEventSpec> = Vec::new();
        let mut last_event_by_author: [Option<usize>; 3] = [None, None, None];

        for i in 0..num_events {
            let author_idx = rng.random_range(0..3);
            let content_idx = rng.random_range(0..10);

            // parent_prev is the last event from this author
            let parent_prev_idx = last_event_by_author[author_idx];

            // Decide between parent_aux and delete (mutually exclusive)
            let same_author_event_indices = events
                .iter()
                .enumerate()
                .filter_map(|(idx, event)| (event.author_idx == author_idx).then_some(idx))
                .collect::<Vec<_>>();
            let (parent_aux_idx, delete_idx) = if !same_author_event_indices.is_empty() {
                let choice = rng.random_range(0..10);
                let earlier_idx =
                    same_author_event_indices[rng.random_range(0..same_author_event_indices.len())];
                if choice < 2 {
                    // 20% chance: delete an earlier event
                    (None, Some(earlier_idx))
                } else if choice < 5 {
                    // 30% chance: have a parent_aux
                    (Some(earlier_idx), None)
                } else {
                    // 50% chance: neither
                    (None, None)
                }
            } else {
                (None, None)
            };

            events.push(TestEventSpec {
                author_idx,
                content_idx,
                parent_prev_idx,
                parent_aux_idx,
                delete_idx,
            });

            last_event_by_author[author_idx] = Some(i);
        }

        // Generate delivery order: pairs of (event_idx, is_content_delivery)
        // Each event needs to be inserted, and content needs to be delivered
        let mut delivery_order: Vec<(usize, bool)> = Vec::new();
        for i in 0..num_events {
            delivery_order.push((i, false)); // insert event
            delivery_order.push((i, true)); // deliver content
        }

        // Shuffle the delivery order
        for i in (1..delivery_order.len()).rev() {
            let j = rng.random_range(0..=i);
            delivery_order.swap(i, j);
        }

        (events, delivery_order)
    }

    /// Property test: RC counting is correct for arbitrary event/content
    /// delivery orders.
    ///
    /// This test:
    /// 1. Generates 10 unique content payloads
    /// 2. Generates a DAG of events referencing these payloads
    /// 3. Delivers events and content in random order
    /// 4. Verifies RC counts match expected values
    #[test]
    fn proptest_rc_counting() {
        // Use proptest runner
        proptest!(ProptestConfig::with_cases(100), |(
            seed in 0u64..10000,
            num_events in 1usize..=50,
            content_seeds in prop::array::uniform10(any::<[u8; 8]>()),
        )| {
            // Run the async test
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                run_rc_property_test(seed, num_events, content_seeds).await
            }).map_err(|e| TestCaseError::fail(e.to_string()))?;
        });
    }

    async fn run_rc_property_test(
        seed: u64,
        num_events: usize,
        content_seeds: [[u8; 8]; 10],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use crate::tests::temp_db;

        // Generate 10 unique content payloads
        let contents: Vec<TestContent> = content_seeds
            .iter()
            .enumerate()
            .map(|(i, seed)| {
                let mut data = seed.to_vec();
                data.push(i as u8); // Ensure uniqueness
                TestContent::new(data)
            })
            .collect();

        // Create 3 authors
        let authors: Vec<RostraIdSecretKey> =
            (0..3).map(|_| RostraIdSecretKey::generate()).collect();

        // Use first author's ID for the database
        let (_dir, db) = temp_db(authors[0].id()).await?;

        // Generate event DAG and delivery order
        let (event_specs, delivery_order) = generate_event_dag(num_events, seed);

        // Build actual VerifiedEvents
        let mut verified_events: Vec<Option<VerifiedEvent>> = vec![None; num_events];
        let mut event_hashes: Vec<(ShortEventId, ContentHash)> = Vec::new();

        // We need to build events in order so parent references are valid
        let mut event_ids: Vec<Option<rostra_core::EventId>> = vec![None; num_events];

        for (i, spec) in event_specs.iter().enumerate() {
            let author_secret = authors[spec.author_idx];
            let author = author_secret.id();
            let content = &contents[spec.content_idx];

            let parent_prev = spec.parent_prev_idx.and_then(|idx| event_ids[idx]);
            let parent_aux = spec.parent_aux_idx.and_then(|idx| event_ids[idx]);
            let delete = spec.delete_idx.and_then(|idx| event_ids[idx]);

            let event = Event::builder_raw_content()
                .author(author)
                .kind(EventKind::SOCIAL_POST)
                .maybe_parent_prev(parent_prev.map(Into::into))
                .maybe_parent_aux(parent_aux.map(Into::into))
                .maybe_delete(delete.map(Into::into))
                .content(&content.raw)
                .build();

            let signed_event = event.signed_by(author_secret);
            let verified = VerifiedEvent::verify_signed(author, signed_event).expect("Valid event");

            event_ids[i] = Some(verified.event_id);
            event_hashes.push((verified.event_id.to_short(), content.hash));
            verified_events[i] = Some(verified);
        }

        // Track which events have been inserted and which have content delivered
        let mut events_inserted: HashSet<usize> = HashSet::new();
        let mut content_delivered: HashSet<usize> = HashSet::new();

        // Execute delivery order
        let consistency_result = db
            .write_with(|tx| {
                let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
                let mut events_table = tx.open_table(&events::TABLE)?;
                let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
                let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
                let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
                let mut content_store_table = tx.open_table(&content_store::TABLE)?;
                let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
                let mut events_content_missing_table =
                    tx.open_table(&events_content_missing::TABLE)?;
                let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;

                for (event_idx, is_content_delivery) in &delivery_order {
                    let event_idx = *event_idx;

                    if *is_content_delivery {
                        // Content delivery
                        if content_delivered.contains(&event_idx) {
                            continue; // Already delivered
                        }

                        let spec = &event_specs[event_idx];
                        let content = &contents[spec.content_idx];

                        // Store content in content_store if not already there
                        if content_store_table.get(&content.hash)?.is_none() {
                            content_store_table.insert(
                                &content.hash,
                                &ContentStoreRecord(Cow::Owned(content.raw.clone())),
                            )?;
                        }

                        // In the new model, RC is managed at event insertion time.
                        // Content arrival just stores the content - no claiming step needed.

                        content_delivered.insert(event_idx);
                    } else {
                        // Event insertion
                        if events_inserted.contains(&event_idx) {
                            continue; // Already inserted
                        }

                        let event = verified_events[event_idx].unwrap();

                        Database::insert_event_tx(
                            event,
                            &mut ids_full_tbl,
                            &mut events_table,
                            &mut events_missing_table,
                            &mut events_heads_table,
                            &mut events_by_time_table,
                            &mut events_content_state_table,
                            &mut content_store_table,
                            &mut content_rc_table,
                            &mut events_content_missing_table,
                            None,
                        )?;

                        events_inserted.insert(event_idx);
                    }
                }

                // Verify RC consistency
                let consistency_result = verify_rc_consistency(
                    &event_hashes,
                    &events_content_state_table,
                    &content_rc_table,
                )?;

                debug!("RC consistency verified for {} events", num_events);

                Ok(consistency_result)
            })
            .await?;

        // Assert consistency at the outer layer
        if let Some(errors) = consistency_result {
            return Err(format!("RC consistency check failed:\n{errors}").into());
        }

        Ok(())
    }
}

// ============================================================================
// Property-based testing for follow/unfollow correctness
// ============================================================================

mod proptest_follow {
    use proptest::prelude::*;
    use rostra_core::event::content_kind::{EventContentKind as _, Follow, PersonaSelector};
    use rostra_core::event::{Event, EventKind, VerifiedEvent, VerifiedEventContent};
    use rostra_core::id::RostraIdSecretKey;
    use tracing::debug;

    use crate::{
        Database, content_rc, content_store, events, events_by_time, events_content_missing,
        events_content_state, events_heads, events_missing, ids_followees, ids_followers, ids_full,
        ids_unfollowed,
    };

    /// Represents a follow or unfollow operation
    #[derive(Debug, Clone, Copy)]
    enum FollowOp {
        /// Follow with a specific "variant" to create different content hashes
        Follow {
            variant: u8,
        },
        Unfollow,
    }

    /// Represents when to deliver event vs content
    #[derive(Debug, Clone, Copy)]
    enum DeliveryStep {
        /// Insert event at index
        InsertEvent(usize),
        /// Process content for event at index
        ProcessContent(usize),
    }

    /// Strategy to generate a sequence of follow/unfollow operations
    fn follow_ops_strategy() -> impl Strategy<Value = Vec<FollowOp>> {
        // Generate 10-50 operations
        prop::collection::vec(
            prop_oneof![
                // Follow with variant 0-3 to create different content hashes
                (0u8..4).prop_map(|variant| FollowOp::Follow { variant }),
                Just(FollowOp::Unfollow),
            ],
            10..=50,
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(50))]

        /// Test that follow/unfollow operations work correctly regardless of delivery order.
        ///
        /// This test:
        /// 1. Generates a sequence of follow/unfollow operations with increasing timestamps
        /// 2. Generates a random delivery order for events and content
        /// 3. Verifies the final following status matches the latest operation by timestamp
        #[test]
        fn test_follow_unfollow_delivery_order(
            ops in follow_ops_strategy(),
            seed: u64,
        ) {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();

            rt.block_on(async {
                run_follow_unfollow_test(ops, seed).await
            }).expect("Test failed");
        }
    }

    async fn run_follow_unfollow_test(
        ops: Vec<FollowOp>,
        seed: u64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use rand::SeedableRng;
        use rand::seq::SliceRandom;

        if ops.is_empty() {
            return Ok(());
        }

        let user_a_secret = RostraIdSecretKey::generate();
        let user_a = user_a_secret.id();
        let user_b_secret = RostraIdSecretKey::generate();
        let user_b = user_b_secret.id();

        let (_dir, db) = super::temp_db(user_a).await?;

        // Create events for each operation with increasing timestamps
        let base_time = time::OffsetDateTime::now_utc();
        let mut events_and_content: Vec<(VerifiedEvent, rostra_core::event::EventContentRaw)> =
            Vec::new();

        for (i, op) in ops.iter().enumerate() {
            let timestamp = base_time + time::Duration::seconds(i as i64);
            let selector = match op {
                FollowOp::Follow { variant } => {
                    // Use variant to create slightly different content
                    // by including different persona IDs in the selector
                    let ids: Vec<_> = (0..*variant).map(rostra_core::event::PersonaId).collect();
                    Some(PersonaSelector::Except { ids })
                }
                FollowOp::Unfollow => None,
            };

            let follow = Follow {
                followee: user_b,
                persona: None,
                selector,
                persona_tags_selector: None,
            };
            let content = follow.serialize_cbor().expect("valid");
            let event = Event::builder_raw_content()
                .author(user_a)
                .kind(EventKind::FOLLOW)
                .content(&content)
                .timestamp(timestamp)
                .build();
            let signed = event.signed_by(user_a_secret);
            let verified = VerifiedEvent::verify_signed(user_a, signed).expect("Valid event");
            events_and_content.push((verified, content));
        }

        // Generate delivery order using seed
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let mut delivery_order: Vec<DeliveryStep> = (0..ops.len())
            .flat_map(|i| {
                vec![
                    DeliveryStep::InsertEvent(i),
                    DeliveryStep::ProcessContent(i),
                ]
            })
            .collect();
        delivery_order.shuffle(&mut rng);

        debug!(
            "Testing {} ops with delivery order: {:?}",
            ops.len(),
            delivery_order
        );

        // Track what has been done
        let mut events_inserted = std::collections::HashSet::new();
        let mut content_processed = std::collections::HashSet::new();

        // Execute delivery order
        for step in &delivery_order {
            match step {
                DeliveryStep::InsertEvent(idx) => {
                    if events_inserted.contains(idx) {
                        continue;
                    }

                    let (event, _content) = &events_and_content[*idx];

                    db.write_with(|tx| {
                        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;
                        let mut events_table = tx.open_table(&events::TABLE)?;
                        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
                        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;
                        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
                        let mut events_content_state_table =
                            tx.open_table(&events_content_state::TABLE)?;
                        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
                        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
                        let mut events_content_missing_table =
                            tx.open_table(&events_content_missing::TABLE)?;

                        Database::insert_event_tx(
                            *event,
                            &mut ids_full_tbl,
                            &mut events_table,
                            &mut events_missing_table,
                            &mut events_heads_table,
                            &mut events_by_time_table,
                            &mut events_content_state_table,
                            &mut content_store_table,
                            &mut content_rc_table,
                            &mut events_content_missing_table,
                            None,
                        )?;

                        Ok(())
                    })
                    .await?;

                    events_inserted.insert(*idx);
                }
                DeliveryStep::ProcessContent(idx) => {
                    if content_processed.contains(idx) {
                        continue;
                    }
                    // Content can only be processed if event was inserted
                    if !events_inserted.contains(idx) {
                        continue;
                    }

                    let (event, content) = &events_and_content[*idx];
                    let verified_content =
                        VerifiedEventContent::assume_verified(*event, content.clone());
                    db.process_event_content(&verified_content).await;

                    content_processed.insert(*idx);
                }
            }
        }

        // Process any remaining content that wasn't processed due to ordering
        for (idx, (event, content)) in events_and_content.iter().enumerate().take(ops.len()) {
            if events_inserted.contains(&idx) && !content_processed.contains(&idx) {
                let verified_content =
                    VerifiedEventContent::assume_verified(*event, content.clone());
                db.process_event_content(&verified_content).await;
                content_processed.insert(idx);
            }
        }

        // Determine expected final state: the operation with the highest timestamp wins
        // Since timestamps are ordered by index, the last operation determines the
        // state
        let last_op = ops.last().unwrap();
        let expected_following = matches!(last_op, FollowOp::Follow { .. });

        // Verify final state
        db.write_with(|tx| {
            let followees_table = tx.open_table(&ids_followees::TABLE)?;
            let followers_table = tx.open_table(&ids_followers::TABLE)?;
            let unfollowed_table = tx.open_table(&ids_unfollowed::TABLE)?;

            let is_following = followees_table.get(&(user_a, user_b))?.is_some();
            let has_follower = followers_table.get(&(user_b, user_a))?.is_some();
            let is_unfollowed = unfollowed_table.get(&(user_a, user_b))?.is_some();

            if expected_following {
                assert!(
                    is_following,
                    "Expected user_a to be following user_b (ops: {ops:?})"
                );
                assert!(
                    has_follower,
                    "Expected user_b to have user_a as follower (ops: {ops:?})"
                );
                assert!(
                    !is_unfollowed,
                    "Expected no unfollow record when following (ops: {ops:?})"
                );
            } else {
                assert!(
                    !is_following,
                    "Expected user_a to NOT be following user_b (ops: {ops:?})"
                );
                assert!(
                    !has_follower,
                    "Expected user_b to NOT have user_a as follower (ops: {ops:?})"
                );
                assert!(
                    is_unfollowed,
                    "Expected unfollow record when not following (ops: {ops:?})"
                );
            }

            Ok(())
        })
        .await?;

        Ok(())
    }
}

/// Test social posts pagination by received_at timestamp.
///
/// This test verifies that:
/// 1. Social posts are correctly inserted into social_posts_by_received_at
///    table
/// 2. Pagination functions return posts in the expected order
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_social_posts_by_received_at_pagination() -> BoxedErrorResult<()> {
    use rostra_core::event::{VerifiedEventContent, content_kind};
    use rostra_core::{ExternalEventId, Timestamp};

    let user_a_secret = RostraIdSecretKey::generate();
    let user_a = user_a_secret.id();

    let user_b_secret = RostraIdSecretKey::generate();
    let user_b = user_b_secret.id();

    // Database owned by user_a
    let (_dir, db) = temp_db(user_a).await?;

    // Helper to build a social post event
    let build_social_post_event = |id_secret: RostraIdSecretKey,
                                   parent: Option<EventId>,
                                   djot_content: &str,
                                   reply_to: Option<ExternalEventId>|
     -> (VerifiedEvent, EventContentRaw) {
        use rostra_core::event::content_kind::EventContentKind as _;
        let content = content_kind::SocialPost::new(
            djot_content.to_string(),
            reply_to,
            Default::default(), // persona_tags
        );
        let content_raw = content.serialize_cbor().unwrap();
        let author = id_secret.id();
        let event = Event::builder_raw_content()
            .author(author)
            .kind(EventKind::SOCIAL_POST)
            .maybe_parent_prev(parent.map(Into::into))
            .content(&content_raw)
            .build();

        let signed_event = event.signed_by(id_secret);
        let verified = VerifiedEvent::verify_signed(author, signed_event).expect("Valid event");
        (verified, content_raw)
    };

    // User B creates a post
    let (post_b1, post_b1_content) =
        build_social_post_event(user_b_secret, None, "Post by B", None);
    let post_b1_id = post_b1.event_id;

    // User A responds to user B's post
    let reply_to_b1 = ExternalEventId::new(user_b, post_b1_id);
    let (reply_a1, reply_a1_content) =
        build_social_post_event(user_a_secret, None, "Reply from A to B", Some(reply_to_b1));
    let reply_a1_id = reply_a1.event_id;

    // User B creates another post
    let (post_b2, post_b2_content) =
        build_social_post_event(user_b_secret, Some(post_b1_id), "Second post by B", None);
    let post_b2_id = post_b2.event_id;

    // Process all events and content with explicit timestamps
    // Insert in order: post_b1 (ts=100), reply_a1 (ts=200), post_b2 (ts=300)
    let events_with_ts = [
        (&post_b1, &post_b1_content, Timestamp::from(100u64)),
        (&reply_a1, &reply_a1_content, Timestamp::from(200u64)),
        (&post_b2, &post_b2_content, Timestamp::from(300u64)),
    ];

    for (event, content_raw, received_ts) in events_with_ts {
        db.write_with(|tx| {
            db.process_event_tx(event, received_ts, tx)?;
            let verified_content =
                VerifiedEventContent::assume_verified(*event, content_raw.clone());
            db.process_event_content_tx(&verified_content, received_ts, tx)?;
            Ok(())
        })
        .await?;
    }

    // Test paginate_social_posts_by_received_at_rev - should return posts in
    // reverse received order
    let (posts_rev, _cursor) = db
        .paginate_social_posts_by_received_at_rev(None, 10, |_| true)
        .await;

    assert_eq!(posts_rev.len(), 3, "Should have 3 posts");
    // Most recently received should be first (post_b2)
    assert_eq!(
        posts_rev[0].event_id,
        post_b2_id.into(),
        "First post should be post_b2 (most recent)"
    );
    assert_eq!(
        posts_rev[1].event_id,
        reply_a1_id.into(),
        "Second post should be reply_a1"
    );
    assert_eq!(
        posts_rev[2].event_id,
        post_b1_id.into(),
        "Third post should be post_b1 (oldest)"
    );

    // Test paginate_social_posts_by_received_at (forward) - should return posts in
    // received order
    let (posts_fwd, _cursor) = db
        .paginate_social_posts_by_received_at(None, 10, |_| true)
        .await;

    assert_eq!(posts_fwd.len(), 3, "Should have 3 posts");
    // Oldest received should be first (post_b1)
    assert_eq!(
        posts_fwd[0].event_id,
        post_b1_id.into(),
        "First post should be post_b1 (oldest)"
    );
    assert_eq!(
        posts_fwd[1].event_id,
        reply_a1_id.into(),
        "Second post should be reply_a1"
    );
    assert_eq!(
        posts_fwd[2].event_id,
        post_b2_id.into(),
        "Third post should be post_b2 (most recent)"
    );

    // Test with filter - only posts replying to user_a
    let (notifications, _cursor) = db
        .paginate_social_posts_by_received_at_rev(None, 10, move |post| {
            post.author != user_a && post.reply_to.map(|ext_id| ext_id.rostra_id()) == Some(user_a)
        })
        .await;

    // No posts should match this filter since no one replied to user_a
    assert_eq!(
        notifications.len(),
        0,
        "No notifications for user_a (no one replied to them)"
    );

    Ok(())
}

/// Test: Total migration correctly rebuilds derived state.
///
/// Verifies that:
/// 1. After forcing an old db version, reopening triggers total migration
/// 2. DB version is updated to current
/// 3. Followees/followers are correctly re-derived
/// 4. Social posts are in the correct index tables
/// 5. Exact winner event IDs are recovered after derived rows are removed
/// 6. Stable database initialization metadata is preserved
/// 7. Present same-author parents replay before their children
/// 8. Reception indexes and their durable sequence rebuild from retained
///    sources
/// 9. The missing-content queue rebuilds from source state instead of retaining
///    inconsistent legacy rows
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_total_migration() -> BoxedErrorResult<()> {
    use rostra_core::Timestamp;
    use rostra_core::event::content_kind::PersonaSelector;
    use rostra_core::event::{VerifiedEventContent, content_kind};

    use crate::{
        db_init_time, db_version, events_received_at, ids_followees, ids_followers,
        reception_order_next, social_posts_by_received_at, social_posts_by_time,
    };

    let user_a_secret = RostraIdSecretKey::from_bytes([1; 32]);
    let user_a = user_a_secret.id();

    let user_b_secret = RostraIdSecretKey::from_bytes([2; 32]);
    let user_b = user_b_secret.id();

    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("db.redb");
    let expected_follow_event_id;
    let expected_post_event_id;
    let expected_missing_event_id;
    let db_init_time_before;

    // Phase 1: Create database with data
    {
        let db = Database::open(&db_path, user_a).await.boxed()?;
        db_init_time_before = db
            .read_with(|tx| {
                Ok(tx
                    .open_table(&db_init_time::TABLE)?
                    .get(&())?
                    .map(|g| g.value())
                    .expect("database initialization time"))
            })
            .await?;

        // Create a follow event (user_a follows user_b)
        // Note: selector must be Some to be a follow, None means unfollow
        let follow_content = content_kind::Follow {
            followee: user_b,
            persona: None,
            selector: Some(PersonaSelector::default()), // Follow all personas
            persona_tags_selector: None,
        };
        let follow_content_raw = {
            use rostra_core::event::content_kind::EventContentKind as _;
            follow_content.serialize_cbor().unwrap()
        };
        let follow_event = {
            let event = Event::builder_raw_content()
                .author(user_a)
                .kind(EventKind::FOLLOW)
                .content(&follow_content_raw)
                .timestamp(time::OffsetDateTime::from_unix_timestamp(100).expect("valid timestamp"))
                .build();
            let signed = event.signed_by(user_a_secret);
            VerifiedEvent::verify_signed(user_a, signed).expect("Valid event")
        };
        expected_follow_event_id = follow_event.event_id.to_short();

        // Use a deleting child whose ID sorts before the target. Raw-ID replay
        // would stage deletion before the follow and produce a different derived
        // result; dependency-ordered replay must process the target first.
        let deleting_follow_event = {
            let event = Event::builder_raw_content()
                .author(user_a)
                .kind(EventKind::NULL)
                .delete(expected_follow_event_id)
                .timestamp(time::OffsetDateTime::UNIX_EPOCH)
                .build();
            let signed = event.signed_by(user_a_secret);
            VerifiedEvent::verify_signed(user_a, signed).expect("valid deleting event")
        };
        assert!(
            deleting_follow_event.event_id.to_short() < expected_follow_event_id,
            "fixed deleting child must sort before its target"
        );

        // Create a follow event (user_b follows user_a) - to test "who follows me"
        let reverse_follow_content = content_kind::Follow {
            followee: user_a,
            persona: None,
            selector: Some(PersonaSelector::default()),
            persona_tags_selector: None,
        };
        let reverse_follow_content_raw = {
            use rostra_core::event::content_kind::EventContentKind as _;
            reverse_follow_content.serialize_cbor().unwrap()
        };
        let reverse_follow_event = {
            let event = Event::builder_raw_content()
                .author(user_b)
                .kind(EventKind::FOLLOW)
                .content(&reverse_follow_content_raw)
                .build();
            let signed = event.signed_by(user_b_secret);
            VerifiedEvent::verify_signed(user_b, signed).expect("Valid event")
        };

        // Create a social post
        let post_content = content_kind::SocialPost::new(
            "Hello world!".to_string(),
            None,               // reply_to
            Default::default(), // persona_tags
        );
        let post_content_raw = {
            use rostra_core::event::content_kind::EventContentKind as _;
            post_content.serialize_cbor().unwrap()
        };
        let post_event = {
            let event = Event::builder_raw_content()
                .author(user_a)
                .kind(EventKind::SOCIAL_POST)
                .content(&post_content_raw)
                .build();
            let signed = event.signed_by(user_a_secret);
            VerifiedEvent::verify_signed(user_a, signed).expect("Valid event")
        };
        let post_event_id = post_event.event_id;
        expected_post_event_id = post_event_id.to_short();
        let (missing_event, _missing_content) =
            build_test_event_with_valid_content(user_b_secret, None, "still missing after replay");
        expected_missing_event_id = missing_event.event_id.to_short();

        // Process events
        let now = Timestamp::now();
        db.write_with(|tx| {
            db.process_event_tx(&follow_event, now, tx)?;
            let verified_follow =
                VerifiedEventContent::assume_verified(follow_event, follow_content_raw);
            db.process_event_content_tx(&verified_follow, now, tx)?;

            db.process_event_tx(&reverse_follow_event, now, tx)?;
            let verified_reverse_follow = VerifiedEventContent::assume_verified(
                reverse_follow_event,
                reverse_follow_content_raw,
            );
            db.process_event_content_tx(&verified_reverse_follow, now, tx)?;

            db.process_event_tx(&post_event, now, tx)?;
            let verified_post = VerifiedEventContent::assume_verified(post_event, post_content_raw);
            db.process_event_content_tx(&verified_post, now, tx)?;
            db.process_event_tx(&deleting_follow_event, now, tx)?;
            db.process_event_tx(&missing_event, now, tx)?;
            Ok(())
        })
        .await?;

        // Seed queue corruption representative of pre-R-06 failed-fetch races.
        // Total replay must discard these derived rows and recreate one exact
        // row for the still-Missing event.
        db.write_with(|tx| {
            let mut queue = tx.open_table(&events_content_missing::TABLE)?;
            queue.insert(&(Timestamp::from(123), expected_missing_event_id), &())?;
            queue.insert(&(Timestamp::from(1), expected_post_event_id), &())?;
            Ok(())
        })
        .await?;

        // Verify data exists before migration - detailed checks
        db.read_with(|tx| {
            let followees = tx.open_table(&ids_followees::TABLE)?;

            // Debug: list all followees entries
            info!("Followees table contents before migration:");
            for entry in followees.range(..)? {
                let (key, value) = entry?;
                info!("  {:?} -> {:?}", key.value(), value.value());
            }

            // Check followee record exists and has correct values
            let followee_record = followees
                .get(&(user_a, user_b))?
                .map(|g| g.value())
                .expect("Follow should exist before migration");
            info!(
                "Followee record before migration: latest_ts={:?}",
                followee_record.latest_ts,
            );

            // Check follower record
            let followers = tx.open_table(&ids_followers::TABLE)?;
            info!("Followers table contents before migration:");
            for entry in followers.range(..)? {
                let (key, _value) = entry?;
                info!("  {:?}", key.value());
            }
            assert!(
                followers.get(&(user_b, user_a))?.is_some(),
                "Follower record should exist before migration"
            );

            let posts_by_time = tx.open_table(&social_posts_by_time::TABLE)?;
            let post_exists = posts_by_time.range(..)?.any(|r| {
                r.map(|(k, _)| k.value().1 == post_event_id.into())
                    .unwrap_or(false)
            });
            assert!(
                post_exists,
                "Post should exist in time index before migration"
            );

            Ok(())
        })
        .await?;

        // Also verify via Database methods before migration
        let followees_before = db.get_followees(user_a).await;
        info!(
            "get_followees(user_a) before migration: {:?}",
            followees_before
        );
        assert_eq!(
            followees_before.len(),
            1,
            "Should have 1 followee before migration"
        );
        assert_eq!(followees_before[0].0, user_b, "Followee should be user_b");

        let followers_before = db.get_followers(user_b).await;
        info!(
            "get_followers(user_b) before migration: {:?}",
            followers_before
        );
        assert_eq!(
            followers_before.len(),
            1,
            "user_b should have 1 follower before migration"
        );
        assert_eq!(followers_before[0], user_a, "Follower should be user_a");

        // Check who follows user_a (self) - this is what the UI shows
        let self_followers_before = db.get_self_followers().await;
        info!(
            "get_self_followers() before migration: {:?}",
            self_followers_before
        );
        assert_eq!(
            self_followers_before.len(),
            1,
            "user_a should have 1 follower before migration"
        );
        assert_eq!(
            self_followers_before[0], user_b,
            "user_a's follower should be user_b"
        );

        let (posts_before, _) = db.paginate_social_posts_rev(None, 10, |_| true).await;
        info!(
            "paginate_social_posts_rev before migration: {} posts",
            posts_before.len()
        );
        assert_eq!(posts_before.len(), 1, "Should have 1 post before migration");

        // Remove the latest-event rows so reopening can restore them only by
        // replaying source events with the current reducer and schema.
        db.write_with(|tx| {
            tx.open_table(&ids_followees::TABLE)?
                .remove(&(user_a, user_b))?;
            tx.open_table(&ids_followers::TABLE)?
                .remove(&(user_b, user_a))?;
            Ok(())
        })
        .await?;

        // Database is dropped here
    }

    // Phase 2: Manually downgrade db version to trigger migration
    {
        let raw_db = redb_bincode::Database::from(redb::Database::open(&db_path).boxed()?);
        let write_txn = raw_db.begin_write().boxed()?;
        {
            let mut table = write_txn.open_table(&db_version::TABLE).boxed()?;
            // Exercise the established total-replay path without changing the
            // production schema counter; the final stacked-series migration
            // owns that single bump.
            let old_version: u64 = 23;
            table.insert(&(), &old_version).boxed()?;
        }
        write_txn.commit().boxed()?;
    }

    // Phase 3: Reopen database - should trigger migration
    let db = Database::open(&db_path, user_a).await.boxed()?;

    // Phase 4: Verify migration worked - detailed checks
    db.read_with(|tx| {
        // Check db version was updated
        let db_ver_table = tx.open_table(&db_version::TABLE)?;
        let current_ver = db_ver_table.first()?.map(|g| g.1.value());
        info!("DB version after migration: {:?}", current_ver);
        assert_eq!(current_ver, Some(24), "DB version should be updated");

        // Check followees table in detail
        let followees = tx.open_table(&ids_followees::TABLE)?;
        info!("Followees table contents after migration:");
        for entry in followees.range(..)? {
            let (key, value) = entry?;
            info!("  {:?} -> {:?}", key.value(), value.value());
        }

        let followee_record = followees
            .get(&(user_a, user_b))?
            .map(|g| g.value())
            .expect("Follow should exist after migration");
        info!(
            "Followee record after migration: latest_ts={:?}",
            followee_record.latest_ts,
        );
        assert_eq!(followee_record.latest_event_id, expected_follow_event_id);

        let db_init_time_after = tx
            .open_table(&db_init_time::TABLE)?
            .get(&())?
            .map(|g| g.value());
        assert_eq!(db_init_time_after, Some(db_init_time_before));

        // Check followers table in detail
        let followers = tx.open_table(&ids_followers::TABLE)?;
        info!("Followers table contents after migration:");
        for entry in followers.range(..)? {
            let (key, _value) = entry?;
            info!("  {:?}", key.value());
        }
        assert!(
            followers.get(&(user_b, user_a))?.is_some(),
            "Follower record should exist after migration"
        );

        // Check social posts
        let posts_by_time = tx.open_table(&social_posts_by_time::TABLE)?;
        let post_count = posts_by_time.range(..)?.count();
        info!("Posts in time index after migration: {}", post_count);
        assert!(
            post_count > 0,
            "Posts should exist in time index after migration"
        );

        let event_receipts = tx
            .open_table(&events_received_at::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value()))
            .collect::<Result<Vec<_>, _>>()?;
        let social_receipts = tx
            .open_table(&social_posts_by_received_at::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(_, event_id)| event_id.value()))
            .collect::<Result<Vec<_>, _>>()?;
        let next_reception_order = tx
            .open_table(&reception_order_next::TABLE)?
            .get(&())?
            .map(|value| value.value());
        assert_eq!(event_receipts.len(), 5);
        assert_eq!(social_receipts, vec![expected_post_event_id]);
        assert_eq!(
            next_reception_order,
            Some((event_receipts.len() + social_receipts.len()) as u64),
            "total replay must rebuild the sequence from its receipt allocations"
        );

        let missing_queue = tx
            .open_table(&events_content_missing::TABLE)?
            .range(..)?
            .map(|entry| entry.map(|(key, _)| key.value()))
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            missing_queue,
            vec![(Timestamp::ZERO, expected_missing_event_id)],
            "total replay must derive one exact row for each queued Missing event"
        );
        assert!(matches!(
            tx.open_table(&events_content_state::TABLE)?
                .get(&expected_missing_event_id)?
                .map(|state| state.value()),
            Some(EventContentState::Missing {
                next_fetch_attempt: Timestamp::ZERO,
                ..
            })
        ));

        Ok(())
    })
    .await?;

    // Phase 5: Verify via Database methods after migration
    info!("=== Verifying Database methods after migration ===");

    let followees_after = db.get_followees(user_a).await;
    info!(
        "get_followees(user_a) after migration: {:?}",
        followees_after
    );
    assert_eq!(
        followees_after.len(),
        1,
        "Should have 1 followee after migration"
    );
    assert_eq!(
        followees_after[0].0, user_b,
        "Followee should be user_b after migration"
    );

    let followers_after = db.get_followers(user_b).await;
    info!(
        "get_followers(user_b) after migration: {:?}",
        followers_after
    );
    assert_eq!(
        followers_after.len(),
        1,
        "user_b should have 1 follower after migration"
    );
    assert_eq!(
        followers_after[0], user_a,
        "Follower should be user_a after migration"
    );

    // Also check self methods since db.self_id == user_a
    let self_followees = db.get_self_followees().await;
    info!("get_self_followees() after migration: {:?}", self_followees);
    assert_eq!(
        self_followees.len(),
        1,
        "Self should have 1 followee after migration"
    );

    // Check who follows user_a (self) - this is what the UI shows
    let self_followers_after = db.get_self_followers().await;
    info!(
        "get_self_followers() after migration: {:?}",
        self_followers_after
    );
    assert_eq!(
        self_followers_after.len(),
        1,
        "user_a should have 1 follower after migration"
    );
    assert_eq!(
        self_followers_after[0], user_b,
        "user_a's follower should be user_b after migration"
    );

    let migrated_followees = db.self_followees_subscribe();
    let migrated_followers = db.self_followers_subscribe();
    let migrated_wot = db.self_wot_subscribe();
    let migrated_head = db.self_head_subscribe();
    assert!(migrated_followees.borrow().contains_key(&user_b));
    assert!(migrated_followers.borrow().contains_key(&user_b));
    assert!(migrated_wot.borrow().followees.contains_key(&user_b));
    let authoritative_head = db.get_self_current_head().await;
    assert_eq!(*migrated_head.borrow(), authoritative_head);

    let (posts_after, _) = db.paginate_social_posts_rev(None, 10, |_| true).await;
    info!(
        "paginate_social_posts_rev after migration: {} posts",
        posts_after.len()
    );
    assert_eq!(posts_after.len(), 1, "Should have 1 post after migration");
    assert_eq!(
        posts_after[0].content.djot_content,
        Some("Hello world!".to_string()),
        "Post content should match after migration"
    );
    let (received_posts_after, _) = db
        .paginate_social_posts_by_received_at(None, 10, |_| true)
        .await;
    assert_eq!(
        received_posts_after
            .iter()
            .map(|post| post.event_id)
            .collect::<Vec<_>>(),
        vec![expected_post_event_id],
        "total replay must retain reception-index membership"
    );

    info!("=== All migration verifications passed ===");

    Ok(())
}

/// Test self-mention detection in social posts.
///
/// This test verifies that:
/// 1. Posts mentioning the local user are recorded in social_posts_self_mention
/// 2. Posts without mentions are not recorded
/// 3. Self-posts (by the local user) are not recorded even if they mention self
/// 4. The is_self_mention and get_self_mentions methods work correctly
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_self_mention_detection() -> BoxedErrorResult<()> {
    use rostra_core::ExternalEventId;
    use rostra_core::event::{VerifiedEventContent, content_kind};

    let user_a_secret = RostraIdSecretKey::generate();
    let user_a = user_a_secret.id();

    let user_b_secret = RostraIdSecretKey::generate();
    let _user_b = user_b_secret.id();

    // Database owned by user_a (user_a is "self")
    let (_dir, db) = temp_db(user_a).await?;

    // Helper to build a social post event
    let build_social_post_event = |id_secret: RostraIdSecretKey,
                                   parent: Option<EventId>,
                                   djot_content: &str,
                                   reply_to: Option<ExternalEventId>|
     -> (VerifiedEvent, EventContentRaw) {
        use rostra_core::event::content_kind::EventContentKind as _;
        let content = content_kind::SocialPost::new(
            djot_content.to_string(),
            reply_to,
            Default::default(), // persona_tags
        );
        let content_raw = content.serialize_cbor().unwrap();
        let author = id_secret.id();
        let event = Event::builder_raw_content()
            .author(author)
            .kind(EventKind::SOCIAL_POST)
            .maybe_parent_prev(parent.map(Into::into))
            .content(&content_raw)
            .build();

        let signed_event = event.signed_by(id_secret);
        let verified = VerifiedEvent::verify_signed(author, signed_event).expect("Valid event");
        (verified, content_raw)
    };

    // Post 1: User B posts mentioning user A
    let mention_content = format!("Hello <rostra:{user_a}>!");
    let (post_mention, post_mention_content) =
        build_social_post_event(user_b_secret, None, &mention_content, None);
    let post_mention_id = post_mention.event_id;

    // Post 2: User B posts without mentioning anyone
    let (post_no_mention, post_no_mention_content) = build_social_post_event(
        user_b_secret,
        Some(post_mention_id),
        "Just a regular post",
        None,
    );
    let post_no_mention_id = post_no_mention.event_id;

    // Post 3: User A posts (self-post, should not trigger notification)
    let (post_self, post_self_content) =
        build_social_post_event(user_a_secret, None, "My own post", None);
    let post_self_id = post_self.event_id;

    // Post 4: User A posts mentioning themselves (self-mention, should not trigger)
    let self_mention_content = format!("I am <rostra:{user_a}>!");
    let (post_self_mention, post_self_mention_content) = build_social_post_event(
        user_a_secret,
        Some(post_self_id),
        &self_mention_content,
        None,
    );
    let post_self_mention_id = post_self_mention.event_id;

    // Post 5: User B replies to user A's post (reply notification, not mention)
    let reply_to_a = ExternalEventId::new(user_a, post_self_id);
    let (post_reply, post_reply_content) = build_social_post_event(
        user_b_secret,
        Some(post_no_mention_id),
        "Reply to A",
        Some(reply_to_a),
    );
    let post_reply_id = post_reply.event_id;

    // Post 6: User B replies AND mentions user A
    let reply_mention_content = format!("Hey <rostra:{user_a}>, replying to you!");
    let (post_reply_mention, post_reply_mention_content) = build_social_post_event(
        user_b_secret,
        Some(post_reply_id),
        &reply_mention_content,
        Some(reply_to_a),
    );
    let post_reply_mention_id = post_reply_mention.event_id;

    // Process all events
    let events_with_content = [
        (&post_mention, &post_mention_content),
        (&post_no_mention, &post_no_mention_content),
        (&post_self, &post_self_content),
        (&post_self_mention, &post_self_mention_content),
        (&post_reply, &post_reply_content),
        (&post_reply_mention, &post_reply_mention_content),
    ];

    let now = rostra_core::Timestamp::now();
    for (event, content_raw) in events_with_content {
        db.write_with(|tx| {
            db.process_event_tx(event, now, tx)?;
            let verified_content =
                VerifiedEventContent::assume_verified(*event, content_raw.clone());
            db.process_event_content_tx(&verified_content, now, tx)?;
            Ok(())
        })
        .await?;
    }

    // Test is_self_mention
    assert!(
        db.is_self_mention(post_mention_id.into()).await,
        "Post with mention should be recorded as self-mention"
    );
    assert!(
        !db.is_self_mention(post_no_mention_id.into()).await,
        "Post without mention should NOT be recorded as self-mention"
    );
    assert!(
        !db.is_self_mention(post_self_id.into()).await,
        "Self-post should NOT be recorded as self-mention"
    );
    assert!(
        !db.is_self_mention(post_self_mention_id.into()).await,
        "Self-post mentioning self should NOT be recorded as self-mention"
    );
    assert!(
        !db.is_self_mention(post_reply_id.into()).await,
        "Reply without mention should NOT be recorded as self-mention"
    );
    assert!(
        db.is_self_mention(post_reply_mention_id.into()).await,
        "Reply with mention should be recorded as self-mention"
    );

    // Test get_self_mentions
    let self_mentions = db.get_self_mentions().await;
    assert_eq!(
        self_mentions.len(),
        2,
        "Should have exactly 2 self-mentions (post_mention and post_reply_mention)"
    );
    assert!(
        self_mentions.contains(&post_mention_id.into()),
        "Self-mentions should contain post_mention"
    );
    assert!(
        self_mentions.contains(&post_reply_mention_id.into()),
        "Self-mentions should contain post_reply_mention"
    );
    assert!(
        !self_mentions.contains(&post_no_mention_id.into()),
        "Self-mentions should NOT contain post_no_mention"
    );

    info!("=== Self-mention detection test passed ===");

    Ok(())
}

/// Test that content processing is idempotent - processing the same content
/// multiple times should not cause duplicate side effects.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_content_processing_idempotency() -> BoxedErrorResult<()> {
    use rostra_core::ExternalEventId;
    use rostra_core::event::content_kind;
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::id::ToShort as _;

    let _ = tracing_subscriber::fmt::try_init();

    let id_secret_a = RostraIdSecretKey::generate();
    let user_a = id_secret_a.id();

    let id_secret_b = RostraIdSecretKey::generate();
    let user_b = id_secret_b.id();

    let (_tmp, db) = temp_db(user_a).await?;

    // Create a post from user A
    let post_content = content_kind::SocialPost::new(
        "Test post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let post_raw = post_content.serialize_cbor().unwrap();
    let post_event = {
        let event = Event::builder_raw_content()
            .author(user_a)
            .kind(EventKind::SOCIAL_POST)
            .content(&post_raw)
            .build();
        let signed = event.signed_by(id_secret_a);
        VerifiedEvent::verify_signed(user_a, signed).expect("Valid event")
    };
    let post_event_id = post_event.event_id;
    let post_id = post_event_id.to_short();

    // Create a reply from user B
    let reply_content = content_kind::SocialPost::new(
        "Reply".to_string(),
        Some(ExternalEventId::new(user_a, post_event_id)),
        Default::default(), // persona_tags
    );
    let reply_raw = reply_content.serialize_cbor().unwrap();
    let reply_event = {
        let event = Event::builder_raw_content()
            .author(user_b)
            .kind(EventKind::SOCIAL_POST)
            .content(&reply_raw)
            .build();
        let signed = event.signed_by(id_secret_b);
        VerifiedEvent::verify_signed(user_b, signed).expect("Valid event")
    };

    let reply_event_id = reply_event.event_id;
    let reply_id = reply_event_id.to_short();

    // Step 1: Process post event (without content)
    let now = rostra_core::Timestamp::now();
    db.write_with(|tx| {
        db.process_event_tx(&post_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Check: post should be marked as Unprocessed
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Missing { .. })),
            "Post should be Unprocessed before content arrives"
        );
        Ok(())
    })
    .await?;

    // Step 2: Process post content
    let verified_post =
        rostra_core::event::VerifiedEventContent::assume_verified(post_event, post_raw.clone());
    db.write_with(|tx| {
        db.process_event_content_tx(&verified_post, now, tx)?;
        Ok(())
    })
    .await?;

    // Check: post should have NO state (Unprocessed removed after processing)
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            state.is_none(),
            "Post should have no content state after processing"
        );
        Ok(())
    })
    .await?;

    // Step 3: Process reply event (without content)
    db.write_with(|tx| {
        db.process_event_tx(&reply_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Check: reply should be marked as Unprocessed
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let state = Database::get_event_content_state_tx(reply_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Missing { .. })),
            "Reply should be Unprocessed before content arrives"
        );
        Ok(())
    })
    .await?;

    // Step 4: Process reply content - this should increment reply_count on post
    let verified_reply =
        rostra_core::event::VerifiedEventContent::assume_verified(reply_event, reply_raw.clone());
    db.write_with(|tx| {
        db.process_event_content_tx(&verified_reply, now, tx)?;
        Ok(())
    })
    .await?;

    // Check: reply should have NO state, post should have reply_count = 1
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let social_posts_table = tx.open_table(&crate::social_posts::TABLE)?;

        let reply_state =
            Database::get_event_content_state_tx(reply_id, &events_content_state_table)?;
        assert!(
            reply_state.is_none(),
            "Reply should have no content state after processing"
        );

        let post_record = social_posts_table.get(&post_id)?.map(|g| g.value());
        assert_eq!(
            post_record.map(|r| r.reply_count).unwrap_or(0),
            1,
            "Post should have reply_count = 1"
        );

        Ok(())
    })
    .await?;

    // Step 5: Try to process reply content AGAIN - should be idempotent
    db.write_with(|tx| {
        db.process_event_content_tx(&verified_reply, now, tx)?;
        Ok(())
    })
    .await?;

    // Check: reply_count should still be 1 (not incremented again)
    db.read_with(|tx| {
        let social_posts_table = tx.open_table(&crate::social_posts::TABLE)?;

        let post_record = social_posts_table.get(&post_id)?.map(|g| g.value());
        assert_eq!(
            post_record.map(|r| r.reply_count).unwrap_or(0),
            1,
            "Post should still have reply_count = 1 after reprocessing"
        );

        Ok(())
    })
    .await?;

    info!("=== Content processing idempotency test passed ===");

    Ok(())
}

/// Test that deleting an event while it's Unprocessed works correctly.
///
/// This verifies:
/// 1. Delete changes state from Unprocessed to Deleted
/// 2. RC is decremented when Unprocessed event is deleted
/// 3. Content processing is skipped for deleted events
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_delete_while_unprocessed() -> BoxedErrorResult<()> {
    use rostra_core::event::content_kind;
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post
    let post_content = content_kind::SocialPost::new(
        "Test post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let post_raw = post_content.serialize_cbor().unwrap();
    let post_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .content(&post_raw)
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };
    let post_event_id = post_event.event_id;
    let post_id = post_event_id.to_short();
    let content_hash = post_event.content_hash();

    // Create a delete event targeting the post
    let delete_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .parent_prev(post_event_id.into())
            .delete(post_event_id.into())
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };

    let now = rostra_core::Timestamp::now();

    // Step 1: Insert post event (without processing content)
    db.write_with(|tx| {
        db.process_event_tx(&post_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify: Post is Unprocessed, RC = 1
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;

        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Missing { .. })),
            "Post should be Unprocessed"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 after post insertion");

        Ok(())
    })
    .await?;

    // Step 2: Insert delete event
    db.write_with(|tx| {
        db.process_event_tx(&delete_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify: Post is now Deleted, RC = 0
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;

        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Deleted { .. })),
            "Post should be Deleted after delete event, got {state:?}"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should be 0 after deletion");

        Ok(())
    })
    .await?;

    // Step 3: Try to process content for the deleted post - should be skipped
    let verified_post =
        rostra_core::event::VerifiedEventContent::assume_verified(post_event, post_raw.clone());
    db.write_with(|tx| {
        db.process_event_content_tx(&verified_post, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify: State still Deleted, no side effects applied
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let social_posts_table = tx.open_table(&crate::social_posts::TABLE)?;

        // State should still be Deleted
        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Deleted { .. })),
            "Post should still be Deleted after attempted content processing"
        );

        // No social post record should exist (content processing was skipped)
        let post_record = social_posts_table.get(&post_id)?;
        assert!(
            post_record.is_none(),
            "No social post record should exist for deleted post"
        );

        Ok(())
    })
    .await?;

    info!("=== Delete while Unprocessed test passed ===");

    Ok(())
}

/// Test that two delete events targeting the same event don't double-decrement
/// RC.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_two_deletes_same_target() -> BoxedErrorResult<()> {
    use rostra_core::event::content_kind;
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post
    let post_content = content_kind::SocialPost::new(
        "Test post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let post_raw = post_content.serialize_cbor().unwrap();
    let post_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .content(&post_raw)
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };
    let post_event_id = post_event.event_id;
    let post_id = post_event_id.to_short();
    let content_hash = post_event.content_hash();

    // Create first delete event
    let delete1_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .parent_prev(post_event_id.into())
            .delete(post_event_id.into())
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };
    let delete1_id = delete1_event.event_id;

    // Create second delete event (different event, same target)
    let delete2_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .parent_prev(delete1_id.into())
            .delete(post_event_id.into())
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };
    let expected_deleted_by = [
        (delete1_event.timestamp(), delete1_event.event_id.to_short()),
        (delete2_event.timestamp(), delete2_event.event_id.to_short()),
    ]
    .into_iter()
    .max()
    .expect("two deletion candidates")
    .1;

    let now = rostra_core::Timestamp::now();

    // Insert post: RC = 1
    db.write_with(|tx| {
        db.process_event_tx(&post_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Insert first delete: RC = 0
    db.write_with(|tx| {
        db.process_event_tx(&delete1_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify RC is 0
    db.read_with(|tx| {
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should be 0 after first delete");
        Ok(())
    })
    .await?;
    let usage_after_first_delete = db.get_data_usage(user).await;

    // Insert second delete: RC should still be 0 (no double decrement)
    db.write_with(|tx| {
        db.process_event_tx(&delete2_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify RC is still 0 (not negative or wrapped)
    db.read_with(|tx| {
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should still be 0 after second delete");

        // State should still be Deleted
        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert_eq!(
            state,
            Some(EventContentState::Deleted {
                deleted_by: expected_deleted_by
            }),
            "Post should retain canonical direct attribution"
        );

        Ok(())
    })
    .await?;
    let usage_after_second_delete = db.get_data_usage(user).await;
    assert_eq!(
        usage_after_second_delete.deleted_payload_num, usage_after_first_delete.deleted_payload_num,
        "attribution-only updates must not repeat payload deletion accounting"
    );
    assert_eq!(
        usage_after_second_delete.deleted_payload_size,
        usage_after_first_delete.deleted_payload_size,
        "attribution-only updates must not repeat payload deletion accounting"
    );

    info!("=== Two deletes same target test passed ===");

    Ok(())
}

/// Test pruning then deleting the same event.
///
/// Verifies:
/// - Prune sets state to Pruned and decrements RC
/// - Delete changes state to Deleted but doesn't decrement RC again
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_prune_then_delete() -> BoxedErrorResult<()> {
    use rostra_core::event::content_kind;
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post
    let post_content = content_kind::SocialPost::new(
        "Test post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let post_raw = post_content.serialize_cbor().unwrap();
    let post_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .content(&post_raw)
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };
    let post_event_id = post_event.event_id;
    let post_id = post_event_id.to_short();
    let content_hash = post_event.content_hash();

    // Create delete event
    let delete_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .parent_prev(post_event_id.into())
            .delete(post_event_id.into())
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };

    let now = rostra_core::Timestamp::now();

    // Insert post: RC = 1, Unprocessed
    db.write_with(|tx| {
        db.process_event_tx(&post_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Prune the post: RC = 0, Pruned
    db.write_with(|tx| {
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;

        Database::prune_event_content_tx(
            post_id,
            content_hash,
            &mut events_content_state_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;
        Ok(())
    })
    .await?;

    // Verify: Pruned, RC = 0
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;

        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Pruned)),
            "Post should be Pruned, got {state:?}"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should be 0 after prune");

        Ok(())
    })
    .await?;

    // Now insert delete event: state should become Deleted, RC stays 0
    db.write_with(|tx| {
        db.process_event_tx(&delete_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify: Deleted (author intent recorded), RC still 0
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;

        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Deleted { .. })),
            "Post should be Deleted after delete event, got {state:?}"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should still be 0 (no double decrement)");

        Ok(())
    })
    .await?;

    info!("=== Prune then delete test passed ===");

    Ok(())
}

/// Test deleting then attempting to prune the same event.
///
/// Verifies:
/// - Delete sets state to Deleted and decrements RC
/// - Prune attempt returns false (already deleted)
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_delete_then_prune() -> BoxedErrorResult<()> {
    use rostra_core::event::content_kind;
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post
    let post_content = content_kind::SocialPost::new(
        "Test post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let post_raw = post_content.serialize_cbor().unwrap();
    let post_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .content(&post_raw)
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };
    let post_event_id = post_event.event_id;
    let post_id = post_event_id.to_short();
    let content_hash = post_event.content_hash();

    // Create delete event
    let delete_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .parent_prev(post_event_id.into())
            .delete(post_event_id.into())
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };

    let now = rostra_core::Timestamp::now();

    // Insert post and delete
    db.write_with(|tx| {
        db.process_event_tx(&post_event, now, tx)?;
        db.process_event_tx(&delete_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify: Deleted, RC = 0
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;

        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Deleted { .. })),
            "Post should be Deleted"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should be 0 after delete");

        Ok(())
    })
    .await?;

    // Attempt to prune: should return false
    let prune_result = db
        .write_with(|tx| {
            let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
            let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;

            let result = Database::prune_event_content_tx(
                post_id,
                content_hash,
                &mut events_content_state_table,
                &mut content_rc_table,
                &mut events_content_missing_table,
                None,
            )?;
            Ok(result)
        })
        .await?;

    assert!(!prune_result, "Prune should return false for deleted event");

    // Verify: still Deleted, RC still 0
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;

        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Deleted { .. })),
            "Post should still be Deleted"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should still be 0");

        Ok(())
    })
    .await?;

    info!("=== Delete then prune test passed ===");

    Ok(())
}

/// Test: Prune after Invalid returns false (already handled).
///
/// Verifies:
/// - Invalid sets state to Invalid and decrements RC
/// - Prune attempt returns false (already invalid, RC already decremented)
/// - State stays Invalid, RC stays 0
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_invalid_then_prune() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    let (event, content) = build_test_event_with_invalid_content(user_secret, None, 500);
    let event_id = event.event_id;
    let post_id = event_id.to_short();
    let content_hash = event.content_hash();
    let now = rostra_core::Timestamp::now();

    // Insert and process (content is invalid — all zeros)
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;
    let verified_content = VerifiedEventContent::assume_verified(event, content);
    db.process_event_content(&verified_content).await;

    // Verify: Invalid, RC = 0
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;

        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Invalid)),
            "Post should be Invalid, got {state:?}"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should be 0 after invalid");

        Ok(())
    })
    .await?;

    // Attempt to prune: should return false
    let prune_result = db
        .write_with(|tx| {
            let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
            let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;

            let result = Database::prune_event_content_tx(
                post_id,
                content_hash,
                &mut events_content_state_table,
                &mut content_rc_table,
                &mut events_content_missing_table,
                None,
            )?;
            Ok(result)
        })
        .await?;

    assert!(!prune_result, "Prune should return false for invalid event");

    // Verify: still Invalid, RC still 0
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;

        let state = Database::get_event_content_state_tx(post_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Invalid)),
            "Post should still be Invalid, got {state:?}"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should still be 0");

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Delete event arrives before target, then target arrives.
///
/// Verifies Flow 3 from the lifecycle docs:
/// - Delete event marks target as deleted_by in events_missing
/// - When target finally arrives, it's immediately marked Deleted
/// - RC is NOT incremented for the target (is_deleted = true)
/// - No payload is tracked as unprocessed
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_delete_before_target_rc_not_incremented() -> BoxedErrorResult<()> {
    use rostra_core::id::ToShort as _;

    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Create the target event (but don't insert it yet)
    let (target_event, _target_content) =
        build_test_event_with_valid_content(id_secret, None, "Will be deleted");
    let target_id = target_event.event_id;
    let target_content_hash = target_event.content_hash();

    // Create the delete event targeting the not-yet-inserted target
    let delete_event = build_delete_event(id_secret, target_id, target_id);

    let now = rostra_core::Timestamp::now();

    // Step 1: Insert delete event first (target doesn't exist yet)
    db.write_with(|tx| {
        db.process_event_tx(&delete_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Step 2: Now insert the target event
    db.write_with(|tx| {
        db.process_event_tx(&target_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify: target is Deleted, RC was never incremented
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;

        // State should be Deleted
        let state = Database::get_event_content_state_tx(
            target_id.to_short(),
            &events_content_state_table,
        )?;
        assert!(
            matches!(state, Some(EventContentState::Deleted { .. })),
            "Target should be Deleted, got {state:?}"
        );

        // RC should be 0 (never incremented for deleted-before-arrival events)
        let rc = Database::get_content_rc_tx(target_content_hash, &content_rc_table)?;
        assert_eq!(
            rc, 0,
            "RC should be 0 — never incremented for pre-deleted target"
        );

        // Data usage: target's payload should NOT be tracked (deleted before arrival).
        // The delete event has content_len=0 but IS now tracked as a payload.
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(
            usage.missing_payload_num, 0,
            "No payloads should be missing (target is deleted)"
        );
        assert_eq!(
            usage.current_payload_num, 1,
            "Delete event (content_len=0) should be tracked"
        );
        assert_eq!(
            usage.total_payload_num, 1,
            "Only delete event tracked (target deleted before arrival)"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Deleting already-processed content (no entry in events_content_state).
///
/// Verifies the old_state=None path in insert_event_tx deletion:
/// - Event is inserted and content is processed (Unprocessed marker removed)
/// - Delete event arrives → current → deleted transition in data usage
/// - RC decremented (was not previously decremented)
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_delete_processed_content() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;
    use rostra_core::id::ToShort as _;

    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    let (event, content) =
        build_test_event_with_valid_content(id_secret, None, "Process then delete");
    let event_id = event.event_id;
    let content_hash = event.content_hash();
    let content_len = u64::from(event.content_len());
    let now = rostra_core::Timestamp::now();

    // Insert and process the event fully
    db.write_with(|tx| {
        db.process_event_tx(&event, now, tx)?;
        Ok(())
    })
    .await?;
    let verified_content = VerifiedEventContent::assume_verified(event, content);
    db.process_event_content(&verified_content).await;

    // Verify: no entry in events_content_state (processed), RC = 1
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;

        let state =
            Database::get_event_content_state_tx(event_id.to_short(), &events_content_state_table)?;
        assert!(
            state.is_none(),
            "Processed event should have no entry, got {state:?}"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 after processing");

        Ok(())
    })
    .await?;

    // Delete the event
    let delete_event = build_delete_event(id_secret, event_id, event_id);
    db.write_with(|tx| {
        db.process_event_tx(&delete_event, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify: Deleted state, RC = 0, data usage moved from current to deleted
    db.read_with(|tx| {
        let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;

        let state =
            Database::get_event_content_state_tx(event_id.to_short(), &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Deleted { .. })),
            "Post should be Deleted, got {state:?}"
        );

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should be 0 after delete");

        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(usage.current_content_size, 0, "Current size should be 0");
        assert_eq!(
            usage.current_payload_num, 1,
            "Current count should be 1 (delete event)"
        );
        assert_eq!(
            usage.deleted_payload_size, content_len,
            "Deleted should have the payload"
        );
        assert_eq!(usage.deleted_payload_num, 1, "Deleted count should be 1");

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Two events share same invalid content hash — RC tracks correctly.
///
/// Verifies:
/// - Both events increment RC to 2
/// - First event processed as Invalid → RC decremented to 1
/// - Second event processed as Invalid → RC decremented to 0
/// - Data usage shows 2 invalid payloads
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_two_events_same_invalid_content() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;

    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Build two events with the same invalid content
    let (event_a, content_a) = build_test_event_with_invalid_content(id_secret, None, 300);
    let event_a_id = event_a.event_id;
    let content_hash = event_a.content_hash();

    let (event_b, content_b) = build_test_event_with_invalid_content(id_secret, event_a_id, 300);
    let content_hash_b = event_b.content_hash();

    // Same content bytes → same hash
    assert_eq!(
        content_hash, content_hash_b,
        "Same content should produce same hash"
    );

    let now = rostra_core::Timestamp::now();

    // Insert both events
    db.write_with(|tx| {
        db.process_event_tx(&event_a, now, tx)?;
        db.process_event_tx(&event_b, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify RC = 2
    db.read_with(|tx| {
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 2, "RC should be 2 after inserting both events");
        Ok(())
    })
    .await?;

    // Process event A (invalid) → RC = 1
    let verified_a = VerifiedEventContent::assume_verified(event_a, content_a);
    db.process_event_content(&verified_a).await;

    db.read_with(|tx| {
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 after first invalid processing");
        Ok(())
    })
    .await?;

    // Process event B (also invalid) → RC = 0
    let verified_b = VerifiedEventContent::assume_verified(event_b, content_b);
    db.process_event_content(&verified_b).await;

    db.read_with(|tx| {
        let content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;

        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should be 0 after both invalid");

        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(
            usage.invalid_payload_num, 2,
            "Should have 2 invalid payloads"
        );
        assert_eq!(usage.missing_payload_num, 0, "No unprocessed payloads left");
        assert_eq!(usage.current_payload_num, 0, "No current payloads");

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test processing content for an event that was never inserted.
///
/// Verifies that this is handled gracefully (skipped, not crash) in release
/// mode. In debug mode, this will panic via debug_assert - that's intentional.
#[cfg(not(debug_assertions))]
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_process_content_for_nonexistent_event() -> BoxedErrorResult<()> {
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::event::{VerifiedEventContent, content_kind};
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post event but DON'T insert it
    let post_content = content_kind::SocialPost::new(
        "Test post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let post_raw = post_content.serialize_cbor().unwrap();
    let post_event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .content(&post_raw)
            .build();
        let signed = event.signed_by(user_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };

    let now = rostra_core::Timestamp::now();

    // Try to process content for the non-existent event
    // This should not panic or error - it should be silently skipped
    let verified_post = VerifiedEventContent::assume_verified(post_event, post_raw);
    db.write_with(|tx| {
        db.process_event_content_tx(&verified_post, now, tx)?;
        Ok(())
    })
    .await?;

    // Verify: no side effects (no social post record created)
    db.read_with(|tx| {
        let social_posts_table = tx.open_table(&crate::social_posts::TABLE)?;
        let events_table = tx.open_table(&events::TABLE)?;

        // Event should not exist
        assert!(
            events_table
                .get(&verified_post.event_id().to_short())?
                .is_none(),
            "Event should not exist"
        );

        // No social post record
        let post_record = social_posts_table.get(&verified_post.event_id().to_short())?;
        assert!(post_record.is_none(), "No social post record should exist");

        Ok(())
    })
    .await?;

    info!("=== Process content for nonexistent event test passed ===");

    Ok(())
}

/// Test that `wants_content` returns correct values based on content state
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_wants_content_basic() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;

    use crate::ProcessEventState;

    info!("=== Testing wants_content behavior ===");

    let id_secret = RostraIdSecretKey::generate();
    let user = id_secret.id();
    let (_dir, db) = temp_db(user).await?;

    // Create a test event with valid content
    let (event, content) = build_test_event_with_valid_content(id_secret, None, "wants test");
    let event_id = event.event_id.to_short();

    // Step 1: Process event (without content yet)
    let (_, process_state) = db.process_event(&event).await;
    info!(?process_state, "Event processed");

    // For a new event, wants_content should return true (ProcessEventState::New ->
    // Wants)
    assert!(
        db.wants_content(event_id, ProcessEventState::New).await,
        "wants_content should return true for ProcessEventState::New"
    );

    // For existing event without content, wants_content should return true
    // (ProcessEventState::Existing -> MaybeWants, then checks DB and finds no
    // content)
    assert!(
        db.wants_content(event_id, ProcessEventState::Existing)
            .await,
        "wants_content should return true when content is NOT in store"
    );

    // Step 2: Process event content (store it)
    let verified_content = VerifiedEventContent::assume_verified(event, content);
    db.process_event_content(&verified_content).await;

    // After storing content, wants_content with Existing should return false
    // (content IS in store now)
    assert!(
        !db.wants_content(event_id, ProcessEventState::Existing)
            .await,
        "wants_content should return false when content IS in store"
    );

    // ProcessEventState::Deleted should always return false
    assert!(
        !db.wants_content(event_id, ProcessEventState::Deleted).await,
        "wants_content should return false for ProcessEventState::Deleted"
    );

    // ProcessEventState::Pruned should always return false
    assert!(
        !db.wants_content(event_id, ProcessEventState::Pruned).await,
        "wants_content should return false for ProcessEventState::Pruned"
    );

    info!("=== wants_content basic test passed ===");

    Ok(())
}

/// Test that `wants_content` correctly identifies missing content for repeated
/// checks This tests the bug fix where content that exists was incorrectly
/// marked as "wanted"
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_wants_content_no_repeated_downloads() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;

    use crate::ProcessEventState;

    info!("=== Testing wants_content doesn't cause repeated downloads ===");

    let id_secret = RostraIdSecretKey::generate();
    let user = id_secret.id();
    let (_dir, db) = temp_db(user).await?;

    // Create a test event with valid content
    let (event, content) = build_test_event_with_valid_content(id_secret, None, "no repeat");
    let event_id = event.event_id.to_short();

    // Process event and content
    db.process_event(&event).await;
    let verified_content = VerifiedEventContent::assume_verified(event, content);
    db.process_event_content(&verified_content).await;

    // Simulate multiple checks (like what happens in the head checker loop)
    // All should return false since we already have the content
    for i in 0..5 {
        let wants = db
            .wants_content(event_id, ProcessEventState::Existing)
            .await;
        assert!(
            !wants,
            "Iteration {i}: wants_content should return false for existing content"
        );
    }

    info!("=== wants_content repeated check test passed ===");

    Ok(())
}

/// Test that `wants_content` returns true for events where content is genuinely
/// missing
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_wants_content_for_missing_content() -> BoxedErrorResult<()> {
    use crate::ProcessEventState;

    info!("=== Testing wants_content for genuinely missing content ===");

    let id_secret = RostraIdSecretKey::generate();
    let user = id_secret.id();
    let (_dir, db) = temp_db(user).await?;

    // Create a test event
    let content = EventContentRaw::new(vec![9, 10, 11, 12]);
    let event = {
        let event = Event::builder_raw_content()
            .author(user)
            .kind(EventKind::SOCIAL_POST)
            .content(&content)
            .build();
        let signed = event.signed_by(id_secret);
        VerifiedEvent::verify_signed(user, signed).expect("Valid event")
    };
    let event_id = event.event_id.to_short();

    // Process event only (not content) - simulating receiving event header but not
    // content
    db.process_event(&event).await;

    // Multiple checks should all return true since content is still missing
    for i in 0..5 {
        let wants = db
            .wants_content(event_id, ProcessEventState::Existing)
            .await;
        assert!(
            wants,
            "Iteration {i}: wants_content should return true for missing content"
        );
    }

    info!("=== wants_content missing content test passed ===");

    Ok(())
}

async fn read_missing_content_queue(
    db: &Database,
) -> BoxedErrorResult<Vec<(Timestamp, rostra_core::ShortEventId)>> {
    Ok(db
        .read_with(|tx| {
            tx.open_table(&events_content_missing::TABLE)?
                .range(..)?
                .map(|entry| entry.map(|(key, _)| key.value()).map_err(Into::into))
                .collect()
        })
        .await?)
}

/// Stale failed-fetch completions cannot advance retry state or leave queue
/// rows after content processing or deletion.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_failed_fetch_completions_are_compare_and_set() -> BoxedErrorResult<()> {
    let id_secret = RostraIdSecretKey::from_bytes([31; 32]);
    let author = id_secret.id();
    let (event, content) =
        build_test_event_with_valid_content(id_secret, None, "fetch completion CAS");
    let event_id = event.event_id.to_short();
    let event_content = VerifiedEventContent::assume_verified(event, content);
    let deletion = build_delete_event(id_secret, event.event_id, event.event_id);
    let first_attempt = Timestamp::from(10);
    let first_retry = Timestamp::from(100);
    let stale_attempt = Timestamp::from(20);
    let stale_retry = Timestamp::from(200);

    for (scenario, process_content, stale_before_terminal) in [
        ("F1,F2,P", true, true),
        ("F1,P,F2", true, false),
        ("F1,F2,D", false, true),
        ("F1,D,F2", false, false),
    ] {
        let (_dir, db) = temp_db(author).await?;
        db.process_event(&event).await;

        db.record_failed_content_fetch(event_id, Timestamp::ZERO, first_attempt, first_retry)
            .await;

        if stale_before_terminal {
            db.record_failed_content_fetch(event_id, Timestamp::ZERO, stale_attempt, stale_retry)
                .await;
            assert_eq!(
                read_missing_content_queue(&db).await?,
                vec![(first_retry, event_id)],
                "{scenario}: stale or non-forward completion must not replace the current schedule"
            );
            db.read_with(|tx| {
                assert!(
                    matches!(
                        tx.open_table(&events_content_state::TABLE)?
                            .get(&event_id)?
                            .map(|state| state.value()),
                        Some(EventContentState::Missing {
                            last_fetch_attempt: Some(last_fetch_attempt),
                            fetch_attempt_count: 1,
                            next_fetch_attempt,
                        }) if last_fetch_attempt == first_attempt
                            && next_fetch_attempt == first_retry
                    ),
                    "{scenario}: stale or non-forward completion changed retry metadata"
                );
                Ok(())
            })
            .await?;
        }

        if process_content {
            db.process_event_content(&event_content).await;
        } else {
            db.process_event(&deletion).await;
        }

        if !stale_before_terminal {
            db.record_failed_content_fetch(event_id, Timestamp::ZERO, stale_attempt, stale_retry)
                .await;
        }

        assert!(
            read_missing_content_queue(&db).await?.is_empty(),
            "{scenario}: terminal content state must have no fetch rows"
        );

        db.read_with(|tx| {
            let state = tx
                .open_table(&events_content_state::TABLE)?
                .get(&event_id)?
                .map(|state| state.value());
            if process_content {
                assert_eq!(state, None, "{scenario}");
            } else {
                assert!(
                    matches!(state, Some(EventContentState::Deleted { .. })),
                    "{scenario}"
                );
            }
            Ok(())
        })
        .await?;
    }

    Ok(())
}

/// Equal or backward replacement schedules cannot reuse a current timestamp as
/// a compare-and-set token.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_failed_fetch_rejects_non_forward_schedule() -> BoxedErrorResult<()> {
    let id_secret = RostraIdSecretKey::from_bytes([33; 32]);
    let author = id_secret.id();
    let (event, _content) =
        build_test_event_with_valid_content(id_secret, None, "non-forward fetch schedule");
    let event_id = event.event_id.to_short();
    let (_dir, db) = temp_db(author).await?;
    let attempted_at = Timestamp::from(10);
    let current_schedule = Timestamp::from(100);

    db.process_event(&event).await;
    db.record_failed_content_fetch(event_id, Timestamp::ZERO, attempted_at, current_schedule)
        .await;
    for invalid_schedule in [current_schedule, Timestamp::from(99)] {
        db.record_failed_content_fetch(
            event_id,
            current_schedule,
            Timestamp::from(20),
            invalid_schedule,
        )
        .await;
    }

    assert_eq!(
        read_missing_content_queue(&db).await?,
        vec![(current_schedule, event_id)]
    );
    db.read_with(|tx| {
        assert!(matches!(
            tx.open_table(&events_content_state::TABLE)?
                .get(&event_id)?
                .map(|state| state.value()),
            Some(EventContentState::Missing {
                last_fetch_attempt: Some(last_fetch_attempt),
                fetch_attempt_count: 1,
                next_fetch_attempt,
            }) if last_fetch_attempt == attempted_at
                && next_fetch_attempt == current_schedule
        ));
        Ok(())
    })
    .await?;

    Ok(())
}

/// Queue peeking transactionally repairs stale front rows and reaches later
/// valid work without making the fetcher wait or retry terminal content.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_missing_content_peek_repairs_stale_front_rows() -> BoxedErrorResult<()> {
    let id_secret = RostraIdSecretKey::from_bytes([32; 32]);
    let author = id_secret.id();
    let dir = tempdir()?;
    let db_path = dir.path().join("db.redb");
    let db = Database::open(&db_path, author).await.boxed()?;
    let processed = build_post_event_content(
        id_secret,
        time::OffsetDateTime::UNIX_EPOCH,
        None,
        "processed",
    );
    let missing = build_post_event_content(
        id_secret,
        time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        Some(processed.event_id()),
        "valid queued work",
    );
    let processed_id = processed.event_id().to_short();
    let missing_id = missing.event_id().to_short();
    let valid_schedule = Timestamp::from(100);

    db.process_event_with_content(&processed).await;
    db.process_event(&missing.event).await;
    db.record_failed_content_fetch(
        missing_id,
        Timestamp::ZERO,
        Timestamp::from(50),
        valid_schedule,
    )
    .await;

    db.write_with(|tx| {
        let mut queue = tx.open_table(&events_content_missing::TABLE)?;
        queue.insert(&(Timestamp::from(1), processed_id), &())?;
        queue.insert(&(Timestamp::from(2), missing_id), &())?;
        queue.insert(&(Timestamp::from(3), rostra_core::ShortEventId::ZERO), &())?;
        queue.insert(&(Timestamp::from(101), processed_id), &())?;
        queue.insert(&(Timestamp::from(102), missing_id), &())?;
        Ok(())
    })
    .await?;

    let next = db
        .peek_next_missing_content()
        .await
        .expect("valid work behind stale rows");
    assert_eq!(next.event_id, missing_id);
    assert_eq!(next.scheduled_time, valid_schedule);
    assert_eq!(next.fetch_attempt_count, 1);
    assert_eq!(
        read_missing_content_queue(&db).await?,
        vec![
            (valid_schedule, missing_id),
            (Timestamp::from(101), processed_id),
            (Timestamp::from(102), missing_id),
        ]
    );
    let (paginated, cursor) = db.paginate_missing_events_contents(None, 10).await;
    assert_eq!(paginated, vec![(author, missing_id)]);
    assert_eq!(cursor, None);

    drop(db);
    let reopened = Database::open(&db_path, author).await.boxed()?;
    assert_eq!(
        read_missing_content_queue(&reopened).await?,
        vec![
            (valid_schedule, missing_id),
            (Timestamp::from(101), processed_id),
            (Timestamp::from(102), missing_id),
        ],
        "front-row repair must survive restart without exposing stale tail rows"
    );
    let (paginated, cursor) = reopened.paginate_missing_events_contents(None, 10).await;
    assert_eq!(paginated, vec![(author, missing_id)]);
    assert_eq!(cursor, None);

    reopened.process_event_content(&missing).await;
    reopened
        .write_with(|tx| {
            tx.open_table(&events_content_missing::TABLE)?
                .insert(&(Timestamp::from(1), processed_id), &())?;
            Ok(())
        })
        .await?;
    assert!(
        reopened.peek_next_missing_content().await.is_none(),
        "a stale terminal row must be removed instead of returned for refetch"
    );
    assert!(read_missing_content_queue(&reopened).await?.is_empty());

    Ok(())
}

// ============================================================================
// Broadcast Channel Tests
// ============================================================================

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_current_state_watches_retain_state_and_match_restart() -> BoxedErrorResult<()> {
    let self_secret = RostraIdSecretKey::generate();
    let self_id = self_secret.id();
    let direct_followee_secret = RostraIdSecretKey::generate();
    let direct_followee = direct_followee_secret.id();
    let extended_followee = RostraIdSecretKey::generate().id();
    let follower_secret = RostraIdSecretKey::generate();
    let follower = follower_secret.id();
    let dir = tempdir()?;
    let db_path = dir.path().join("db.redb");
    let db = Database::open(&db_path, self_id).await.boxed()?;
    let base_time = time::OffsetDateTime::UNIX_EPOCH;

    let self_follow = build_follow_event_content(self_secret, direct_followee, base_time, None);
    let self_post_a = build_post_event_content(
        self_secret,
        base_time + time::Duration::seconds(1),
        Some(self_follow.event_id()),
        "retained head A",
    );
    let self_post_b = build_post_event_content(
        self_secret,
        base_time + time::Duration::seconds(2),
        Some(self_follow.event_id()),
        "retained head B",
    );
    let follower_event = build_follow_event_content(follower_secret, self_id, base_time, None);
    let extended_event =
        build_follow_event_content(direct_followee_secret, extended_followee, base_time, None);
    let expected_head = std::cmp::min(
        self_post_a.event_id().to_short(),
        self_post_b.event_id().to_short(),
    );

    db.write_with(|tx| {
        for event_content in [
            &self_follow,
            &extended_event,
            &follower_event,
            &self_post_a,
            &self_post_b,
        ] {
            db.process_event_tx(&event_content.event, Timestamp::ZERO, tx)?;
            db.process_event_content_tx(event_content, Timestamp::ZERO, tx)?;
        }
        Ok(())
    })
    .await?;

    let followees_rx = db.self_followees_subscribe();
    let followers_rx = db.self_followers_subscribe();
    let wot_rx = db.self_wot_subscribe();
    let head_rx = db.self_head_subscribe();

    assert!(followees_rx.borrow().contains_key(&direct_followee));
    assert!(followers_rx.borrow().contains_key(&follower));
    assert!(wot_rx.borrow().followees.contains_key(&direct_followee));
    assert!(wot_rx.borrow().extended.contains(&extended_followee));
    assert_eq!(*head_rx.borrow(), Some(expected_head));

    let continuous_head = *head_rx.borrow();

    drop((followees_rx, followers_rx, wot_rx, head_rx));
    drop(db);

    let reopened = Database::open(&db_path, self_id).await.boxed()?;
    let reopened_followees = reopened.self_followees_subscribe();
    let reopened_followers = reopened.self_followers_subscribe();
    let reopened_wot = reopened.self_wot_subscribe();
    let reopened_head = reopened.self_head_subscribe();

    assert_eq!(reopened_followees.borrow().len(), 1);
    assert!(reopened_followees.borrow().contains_key(&direct_followee));
    assert_eq!(reopened_followers.borrow().len(), 1);
    assert!(reopened_followers.borrow().contains_key(&follower));
    assert_eq!(reopened_wot.borrow().followees.len(), 1);
    assert!(
        reopened_wot
            .borrow()
            .followees
            .contains_key(&direct_followee)
    );
    assert_eq!(reopened_wot.borrow().extended.len(), 1);
    assert!(reopened_wot.borrow().extended.contains(&extended_followee));
    assert_eq!(*reopened_head.borrow(), continuous_head);

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_current_state_watches_cannot_regress_between_commits() -> BoxedErrorResult<()> {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    let self_secret = RostraIdSecretKey::generate();
    let self_id = self_secret.id();
    let direct_a_secret = RostraIdSecretKey::generate();
    let direct_a = direct_a_secret.id();
    let direct_b = RostraIdSecretKey::generate().id();
    let extended = RostraIdSecretKey::generate().id();
    let follower_a_secret = RostraIdSecretKey::generate();
    let follower_a = follower_a_secret.id();
    let follower_b_secret = RostraIdSecretKey::generate();
    let follower_b = follower_b_secret.id();
    let (_dir, db) = temp_db(self_id).await?;
    let db = Arc::new(db);
    let followees_rx = db.self_followees_subscribe();
    let followers_rx = db.self_followers_subscribe();
    let wot_rx = db.self_wot_subscribe();
    let head_rx = db.self_head_subscribe();
    let base_time = time::OffsetDateTime::UNIX_EPOCH;

    let older_self_follow = build_follow_event_content(self_secret, direct_a, base_time, None);
    let older_self_post = build_post_event_content(
        self_secret,
        base_time + time::Duration::seconds(1),
        Some(older_self_follow.event_id()),
        "older head",
    );
    let older_follower = build_follow_event_content(follower_a_secret, self_id, base_time, None);

    let newer_self_follow = build_follow_event_content(
        self_secret,
        direct_b,
        base_time + time::Duration::seconds(2),
        Some(older_self_post.event_id()),
    );
    let newer_self_post = build_post_event_content(
        self_secret,
        base_time + time::Duration::seconds(3),
        Some(newer_self_follow.event_id()),
        "newer head",
    );
    let newer_follower = build_follow_event_content(
        follower_b_secret,
        self_id,
        base_time + time::Duration::seconds(1),
        None,
    );
    let newer_extended = build_follow_event_content(
        direct_a_secret,
        extended,
        base_time + time::Duration::seconds(1),
        None,
    );
    let expected_head = newer_self_post.event_id().to_short();

    let (older_hook_entered_tx, older_hook_entered_rx) = mpsc::channel();
    let (release_older_hook_tx, release_older_hook_rx) = mpsc::channel();
    let older_db = db.clone();
    let older_task = tokio::spawn(async move {
        older_db
            .write_with(|tx| {
                tx.on_commit(move || {
                    let _ = older_hook_entered_tx.send(());
                    release_older_hook_rx
                        .recv()
                        .expect("Test must release older hook");
                });
                for event_content in [&older_self_follow, &older_follower, &older_self_post] {
                    older_db.process_event_tx(&event_content.event, Timestamp::ZERO, tx)?;
                    older_db.process_event_content_tx(event_content, Timestamp::ZERO, tx)?;
                }
                Ok(())
            })
            .await
    });

    let older_hook_entered = tokio::task::spawn_blocking(move || {
        older_hook_entered_rx.recv_timeout(Duration::from_secs(5))
    })
    .await;

    let (newer_started_tx, newer_started_rx) = tokio::sync::oneshot::channel();
    let newer_db = db.clone();
    let newer_task = tokio::spawn(async move {
        let _ = newer_started_tx.send(());
        newer_db
            .write_with(|tx| {
                for event_content in [
                    &newer_self_follow,
                    &newer_follower,
                    &newer_extended,
                    &newer_self_post,
                ] {
                    newer_db.process_event_tx(&event_content.event, Timestamp::ZERO, tx)?;
                    newer_db.process_event_content_tx(event_content, Timestamp::ZERO, tx)?;
                }
                Ok(())
            })
            .await
    });

    let newer_started = tokio::time::timeout(Duration::from_secs(5), newer_started_rx).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let newer_waited = !newer_task.is_finished();

    let release_result = release_older_hook_tx.send(());
    let older_result = older_task.await;
    let newer_result = newer_task.await;

    older_hook_entered
        .expect("Hook waiter must not panic")
        .expect("Older transaction must reach its first commit hook");
    newer_started
        .expect("Newer transaction start must not time out")
        .expect("Newer transaction must start");
    assert!(
        newer_waited,
        "Newer commit must wait for older current-state publication"
    );
    release_result.expect("Older hook must still be waiting");
    older_result
        .expect("Older task must not panic")
        .expect("Older transaction must commit");
    newer_result
        .expect("Newer task must not panic")
        .expect("Newer transaction must commit");

    assert!(followees_rx.borrow().contains_key(&direct_a));
    assert!(followees_rx.borrow().contains_key(&direct_b));
    assert!(followers_rx.borrow().contains_key(&follower_a));
    assert!(followers_rx.borrow().contains_key(&follower_b));
    assert!(wot_rx.borrow().followees.contains_key(&direct_a));
    assert!(wot_rx.borrow().followees.contains_key(&direct_b));
    assert!(wot_rx.borrow().extended.contains(&extended));
    assert_eq!(*head_rx.borrow(), Some(expected_head));

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_current_state_publication_survives_earlier_hook_panic() -> BoxedErrorResult<()> {
    use std::sync::Arc;

    let self_secret = RostraIdSecretKey::generate();
    let self_id = self_secret.id();
    let followee = RostraIdSecretKey::generate().id();
    let (_dir, db) = temp_db(self_id).await?;
    let db = Arc::new(db);
    let follow = build_follow_event_content(
        self_secret,
        followee,
        time::OffsetDateTime::UNIX_EPOCH,
        None,
    );
    let post = build_post_event_content(
        self_secret,
        time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        Some(follow.event_id()),
        "write after hook panic",
    );

    let panic_db = db.clone();
    let panic_task = tokio::spawn(async move {
        panic_db
            .write_with(|tx| {
                tx.on_commit(|| panic!("controlled commit-hook panic"));
                panic_db.process_event_tx(&follow.event, Timestamp::ZERO, tx)?;
                panic_db.process_event_content_tx(&follow, Timestamp::ZERO, tx)?;
                Ok(())
            })
            .await
    });
    assert!(
        panic_task
            .await
            .expect_err("Commit hook must propagate its panic")
            .is_panic()
    );

    let followees_rx = db.self_followees_subscribe();
    let wot_rx = db.self_wot_subscribe();
    assert!(followees_rx.borrow().contains_key(&followee));
    assert!(wot_rx.borrow().followees.contains_key(&followee));

    db.process_event_with_content(&post).await;
    assert_eq!(
        *db.self_head_subscribe().borrow(),
        Some(post.event_id().to_short())
    );

    Ok(())
}

/// Test that new_heads_tx broadcasts when a new head event is inserted
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_new_heads_broadcast() -> BoxedErrorResult<()> {
    use std::time::Duration;

    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Subscribe to new heads before inserting events
    let mut new_heads_rx = db.new_heads_subscribe();

    // Create and insert an event (will be a head since no children)
    let content = content_kind::SocialPost::new(
        "Test post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let content_raw = content.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .content(&content_raw)
        .build();
    let signed_event = event.signed_by(id_secret);
    let verified_event = VerifiedEvent::verify_signed(author, signed_event).expect("Valid event");
    let event_id = verified_event.event_id;

    db.process_event(&verified_event).await;

    // Should receive the new head notification
    let result = tokio::time::timeout(Duration::from_secs(1), new_heads_rx.recv()).await;
    assert!(result.is_ok(), "Should receive new head notification");
    let (received_author, received_head) = result.unwrap().expect("Channel should not be closed");
    assert_eq!(received_author, author);
    assert_eq!(received_head, event_id.into());

    info!("=== new_heads_broadcast test passed ===");
    Ok(())
}

/// Test that new_heads_tx does NOT broadcast for non-head events (was_missing)
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_new_heads_broadcast_not_for_non_head() -> BoxedErrorResult<()> {
    use std::time::Duration;

    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Create parent and child events
    let content = content_kind::SocialPost::new(
        "Parent post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let content_raw = content.serialize_cbor().unwrap();
    let parent_event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .content(&content_raw)
        .build();
    let parent_signed = parent_event.signed_by(id_secret);
    let parent_verified = VerifiedEvent::verify_signed(author, parent_signed).expect("Valid event");
    let parent_id = parent_verified.event_id;

    let child_content = content_kind::SocialPost::new(
        "Child post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let child_content_raw = child_content.serialize_cbor().unwrap();
    let child_event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .parent_prev(parent_id.into())
        .content(&child_content_raw)
        .build();
    let child_signed = child_event.signed_by(id_secret);
    let child_verified = VerifiedEvent::verify_signed(author, child_signed).expect("Valid event");

    // Insert child first (parent becomes "missing")
    db.process_event(&child_verified).await;

    // Subscribe after child is inserted
    let mut new_heads_rx = db.new_heads_subscribe();

    // Now insert parent - it should NOT be a head (was_missing = true)
    db.process_event(&parent_verified).await;

    // Should NOT receive a notification for the parent (it's not a head)
    let result = tokio::time::timeout(Duration::from_millis(100), new_heads_rx.recv()).await;
    assert!(
        result.is_err(),
        "Should NOT receive new head notification for non-head event"
    );

    info!("=== new_heads_broadcast_not_for_non_head test passed ===");
    Ok(())
}

/// Test that self_head_updated broadcasts when self head changes
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_self_head_updated_broadcast() -> BoxedErrorResult<()> {
    use std::time::Duration;

    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Subscribe to self head updates
    let mut self_head_rx = db.self_head_subscribe();

    // Create and insert an event from self
    let content = content_kind::SocialPost::new(
        "Self post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let content_raw = content.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .content(&content_raw)
        .build();
    let signed_event = event.signed_by(id_secret);
    let verified_event = VerifiedEvent::verify_signed(author, signed_event).expect("Valid event");
    let event_id = verified_event.event_id;

    db.process_event(&verified_event).await;

    // Should receive the self head update notification
    let result = tokio::time::timeout(Duration::from_secs(1), self_head_rx.changed()).await;
    assert!(
        result.is_ok(),
        "Should receive self head update notification"
    );
    assert!(result.unwrap().is_ok(), "Channel should not be closed");

    let received_head = *self_head_rx.borrow();
    assert_eq!(received_head, Some(event_id.into()));

    info!("=== self_head_updated_broadcast test passed ===");
    Ok(())
}

/// Test that self_head_updated does NOT broadcast for other users' events
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_self_head_updated_not_for_others() -> BoxedErrorResult<()> {
    use std::time::Duration;

    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let self_secret = RostraIdSecretKey::generate();
    let self_id = self_secret.id();
    let other_secret = RostraIdSecretKey::generate();
    let other_id = other_secret.id();

    let (_dir, db) = temp_db(self_id).await?;

    // Subscribe to self head updates
    let mut self_head_rx = db.self_head_subscribe();

    // Create and insert an event from OTHER user
    let content = content_kind::SocialPost::new(
        "Other user post".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let content_raw = content.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(other_id)
        .kind(EventKind::SOCIAL_POST)
        .content(&content_raw)
        .build();
    let signed_event = event.signed_by(other_secret);
    let verified_event = VerifiedEvent::verify_signed(other_id, signed_event).expect("Valid event");

    db.process_event(&verified_event).await;

    // Should NOT receive a self head update notification
    let result = tokio::time::timeout(Duration::from_millis(100), self_head_rx.changed()).await;
    assert!(
        result.is_err(),
        "Should NOT receive self head update for other user's event"
    );

    info!("=== self_head_updated_not_for_others test passed ===");
    Ok(())
}

/// Test that new_content_tx broadcasts when content is processed
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_new_content_broadcast() -> BoxedErrorResult<()> {
    use std::time::Duration;

    use rostra_core::event::VerifiedEventContent;
    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Subscribe to new content
    let mut new_content_rx = db.new_content_subscribe();

    // Create event with content
    let content = content_kind::SocialPost::new(
        "Test content".to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let content_raw = content.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .content(&content_raw)
        .build();
    let signed_event = event.signed_by(id_secret);
    let verified_event = VerifiedEvent::verify_signed(author, signed_event).expect("Valid event");
    let event_id = verified_event.event_id;

    let verified_content = VerifiedEventContent::assume_verified(verified_event, content_raw);

    // Process event with content
    db.process_event_with_content(&verified_content).await;

    // Should receive the new content notification
    let result = tokio::time::timeout(Duration::from_secs(1), new_content_rx.recv()).await;
    assert!(result.is_ok(), "Should receive new content notification");
    let received_content = result.unwrap().expect("Channel should not be closed");
    assert_eq!(received_content.event_id(), event_id);

    info!("=== new_content_broadcast test passed ===");
    Ok(())
}

/// Test that new_posts_tx broadcasts when a social post is processed
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_new_posts_broadcast() -> BoxedErrorResult<()> {
    use std::time::Duration;

    use rostra_core::event::VerifiedEventContent;
    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Subscribe to new posts
    let mut new_posts_rx = db.new_posts_subscribe();

    // Create a social post
    let post_text = "Test social post";
    let content = content_kind::SocialPost::new(
        post_text.to_string(),
        None,               // reply_to
        Default::default(), // persona_tags
    );
    let content_raw = content.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(author)
        .kind(EventKind::SOCIAL_POST)
        .content(&content_raw)
        .build();
    let signed_event = event.signed_by(id_secret);
    let verified_event = VerifiedEvent::verify_signed(author, signed_event).expect("Valid event");
    let event_id = verified_event.event_id;

    let verified_content = VerifiedEventContent::assume_verified(verified_event, content_raw);

    // Process event with content
    db.process_event_with_content(&verified_content).await;

    // Should receive the new post notification
    let result = tokio::time::timeout(Duration::from_secs(1), new_posts_rx.recv()).await;
    assert!(result.is_ok(), "Should receive new post notification");
    let (received_event_content, received_post) =
        result.unwrap().expect("Channel should not be closed");
    assert_eq!(received_event_content.event_id(), event_id);
    assert_eq!(received_post.djot_content, Some(post_text.to_string()));

    info!("=== new_posts_broadcast test passed ===");
    Ok(())
}

/// Test that self_followees_updated is triggered when self follows/unfollows
/// someone
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_self_followees_watch_channel() -> BoxedErrorResult<()> {
    use std::time::Duration;

    use rostra_core::event::VerifiedEventContent;
    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let id_secret = RostraIdSecretKey::generate();
    let self_id = id_secret.id();
    let followee = RostraIdSecretKey::generate().id();
    let (_dir, db) = temp_db(self_id).await?;

    // Subscribe to followees updates
    let mut followees_rx = db.self_followees_subscribe();

    // Initially should be empty
    assert!(
        followees_rx.borrow().is_empty(),
        "Should start with no followees"
    );

    // Create a follow event from self to followee
    let follow_content = content_kind::Follow {
        followee,
        persona: None,
        selector: Some(rostra_core::event::PersonaSelector::Except { ids: vec![] }),
        persona_tags_selector: None,
    };
    let content_raw = follow_content.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(self_id)
        .kind(EventKind::FOLLOW)
        .content(&content_raw)
        .build();
    let signed_event = event.signed_by(id_secret);
    let verified_event = VerifiedEvent::verify_signed(self_id, signed_event).expect("Valid event");
    let verified_content = VerifiedEventContent::assume_verified(verified_event, content_raw);

    // Process the follow event
    db.process_event_with_content(&verified_content).await;

    // Should receive the followees update
    let result = tokio::time::timeout(Duration::from_secs(1), followees_rx.changed()).await;
    assert!(
        result.is_ok(),
        "Should receive followees update notification"
    );
    assert!(result.unwrap().is_ok(), "Channel should not be closed");

    // Verify the followee is now in the map
    let followees = followees_rx.borrow();
    assert!(
        followees.contains_key(&followee),
        "Followee should be in the map"
    );
    assert_eq!(followees.len(), 1, "Should have exactly one followee");

    info!("=== self_followees_watch_channel test passed ===");
    Ok(())
}

/// Test that self_followers_updated is triggered when someone follows/unfollows
/// self
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_self_followers_watch_channel() -> BoxedErrorResult<()> {
    use std::time::Duration;

    use rostra_core::event::VerifiedEventContent;
    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let self_secret = RostraIdSecretKey::generate();
    let self_id = self_secret.id();
    let other_secret = RostraIdSecretKey::generate();
    let other_id = other_secret.id();
    let (_dir, db) = temp_db(self_id).await?;

    // Subscribe to followers updates
    let mut followers_rx = db.self_followers_subscribe();

    // Initially should be empty
    assert!(
        followers_rx.borrow().is_empty(),
        "Should start with no followers"
    );

    // Create a follow event from other to self
    let follow_content = content_kind::Follow {
        followee: self_id,
        persona: None,
        selector: Some(rostra_core::event::PersonaSelector::Except { ids: vec![] }),
        persona_tags_selector: None,
    };
    let content_raw = follow_content.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(other_id)
        .kind(EventKind::FOLLOW)
        .content(&content_raw)
        .build();
    let signed_event = event.signed_by(other_secret);
    let verified_event = VerifiedEvent::verify_signed(other_id, signed_event).expect("Valid event");
    let verified_content = VerifiedEventContent::assume_verified(verified_event, content_raw);

    // Process the follow event
    db.process_event_with_content(&verified_content).await;

    // Should receive the followers update
    let result = tokio::time::timeout(Duration::from_secs(1), followers_rx.changed()).await;
    assert!(
        result.is_ok(),
        "Should receive followers update notification"
    );
    assert!(result.unwrap().is_ok(), "Channel should not be closed");

    // Verify the follower is now in the map
    let followers = followers_rx.borrow();
    assert!(
        followers.contains_key(&other_id),
        "Follower should be in the map"
    );
    assert_eq!(followers.len(), 1, "Should have exactly one follower");

    info!("=== self_followers_watch_channel test passed ===");
    Ok(())
}

/// Test that self_wot_updated (web of trust) is triggered when self follows
/// someone
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_self_wot_watch_channel() -> BoxedErrorResult<()> {
    use std::time::Duration;

    use rostra_core::event::VerifiedEventContent;
    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let self_secret = RostraIdSecretKey::generate();
    let self_id = self_secret.id();
    let followee_a = RostraIdSecretKey::generate().id();
    let (_dir, db) = temp_db(self_id).await?;

    // Subscribe to WoT updates
    let mut wot_rx = db.self_wot_subscribe();

    // Initially should be empty
    assert!(wot_rx.borrow().is_empty(), "Should start with empty WoT");

    // Self follows A
    let follow_a_content = content_kind::Follow {
        followee: followee_a,
        persona: None,
        selector: Some(rostra_core::event::PersonaSelector::Except { ids: vec![] }),
        persona_tags_selector: None,
    };
    let content_raw = follow_a_content.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(self_id)
        .kind(EventKind::FOLLOW)
        .content(&content_raw)
        .build();
    let signed_event = event.signed_by(self_secret);
    let verified_event = VerifiedEvent::verify_signed(self_id, signed_event).expect("Valid event");
    let verified_content = VerifiedEventContent::assume_verified(verified_event, content_raw);

    db.process_event_with_content(&verified_content).await;

    // Should receive WoT update
    let result = tokio::time::timeout(Duration::from_secs(1), wot_rx.changed()).await;
    assert!(result.is_ok(), "Should receive WoT update notification");
    assert!(result.unwrap().is_ok(), "Channel should not be closed");

    // Verify WoT now contains followee_a as a direct followee
    {
        let wot = wot_rx.borrow();
        assert!(
            wot.followees.contains_key(&followee_a),
            "A should be in direct followees"
        );
        assert!(
            wot.extended.is_empty(),
            "Extended should be empty (A hasn't followed anyone)"
        );
        assert_eq!(wot.len(), 1, "WoT should have 1 entry");
    }

    info!("=== self_wot_watch_channel test passed ===");
    Ok(())
}

/// Test that WoT contains method works correctly
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_wot_contains() -> BoxedErrorResult<()> {
    use rostra_core::event::VerifiedEventContent;
    use rostra_core::event::content_kind::{self, EventContentKind as _};

    let self_secret = RostraIdSecretKey::generate();
    let self_id = self_secret.id();
    let followee = RostraIdSecretKey::generate().id();
    let stranger = RostraIdSecretKey::generate().id();
    let (_dir, db) = temp_db(self_id).await?;

    // Subscribe to WoT
    let wot_rx = db.self_wot_subscribe();

    // Follow someone
    let follow_content = content_kind::Follow {
        followee,
        persona: None,
        selector: Some(rostra_core::event::PersonaSelector::Except { ids: vec![] }),
        persona_tags_selector: None,
    };
    let content_raw = follow_content.serialize_cbor().unwrap();
    let event = Event::builder_raw_content()
        .author(self_id)
        .kind(EventKind::FOLLOW)
        .content(&content_raw)
        .build();
    let signed_event = event.signed_by(self_secret);
    let verified_event = VerifiedEvent::verify_signed(self_id, signed_event).expect("Valid event");
    let verified_content = VerifiedEventContent::assume_verified(verified_event, content_raw);

    db.process_event_with_content(&verified_content).await;

    // Check contains
    let wot = wot_rx.borrow();
    assert!(wot.contains(self_id, self_id), "Self should be in WoT");
    assert!(wot.contains(followee, self_id), "Followee should be in WoT");
    assert!(
        !wot.contains(stranger, self_id),
        "Stranger should NOT be in WoT"
    );

    info!("=== wot_contains test passed ===");
    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_process_social_vote_with_content() -> BoxedErrorResult<()> {
    use rostra_core::ExternalEventId;
    use rostra_core::event::{VerifiedEventContent, content_kind};

    let voter_secret = RostraIdSecretKey::generate();
    let voter = voter_secret.id();
    let post_author = RostraIdSecretKey::generate().id();
    let post_id = ExternalEventId::new(post_author, rostra_core::ShortEventId::ZERO);
    let (_dir, db) = temp_db(voter).await?;

    let vote = content_kind::SocialVote::new(post_id, Some(true));
    let (event, content_raw) = Event::builder(&vote).author(voter).build()?;
    let signed_event = event.signed_by(voter_secret);
    let verified_event = VerifiedEvent::verify_signed(voter, signed_event).expect("Valid event");
    let verified_content = VerifiedEventContent::assume_verified(verified_event, content_raw);

    db.process_event_with_content(&verified_content).await;

    assert_eq!(db.get_social_vote_sum(post_id).await, 1);

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_expired_news_rank_removed_on_score_recalculation() -> BoxedErrorResult<()> {
    use rostra_core::{ExternalEventId, ShortEventId, Timestamp};

    use crate::{
        SocialNewsRankRecord, social_news_rank_by_post_id, social_news_rank_by_score,
        social_news_rank_by_time,
    };

    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;
    let post_id = ExternalEventId::new(id_secret.id(), ShortEventId::ZERO);
    let creation_ts = Timestamp::from(1_000);
    let score = 42;
    let now = creation_ts.saturating_add_secs(crate::news::NEWS_MAX_AGE_SECS + 1);

    db.write_with(|tx| {
        tx.open_table(&social_news_rank_by_post_id::TABLE)?
            .insert(&post_id, &SocialNewsRankRecord { creation_ts, score })?;
        tx.open_table(&social_news_rank_by_score::TABLE)?
            .insert(&(score, post_id), &())?;
        tx.open_table(&social_news_rank_by_time::TABLE)?
            .insert(&(creation_ts, post_id), &())?;

        let updated = Database::recalculate_news_post_score_tx(post_id, now, tx)?;
        assert!(!updated);

        assert!(
            tx.open_table(&social_news_rank_by_post_id::TABLE)?
                .get(&post_id)?
                .is_none()
        );
        assert!(
            tx.open_table(&social_news_rank_by_score::TABLE)?
                .get(&(score, post_id))?
                .is_none()
        );
        assert!(
            tx.open_table(&social_news_rank_by_time::TABLE)?
                .get(&(creation_ts, post_id))?
                .is_none()
        );

        Ok(())
    })
    .await?;

    Ok(())
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_news_rank_at_max_age_not_removed_on_score_recalculation() -> BoxedErrorResult<()> {
    use rostra_core::{ExternalEventId, ShortEventId, Timestamp};

    use crate::{
        SocialNewsRankRecord, social_news_rank_by_post_id, social_news_rank_by_score,
        social_news_rank_by_time,
    };

    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;
    let post_id = ExternalEventId::new(id_secret.id(), ShortEventId::ZERO);
    let creation_ts = Timestamp::from(1_000);
    let now = creation_ts.saturating_add_secs(crate::news::NEWS_MAX_AGE_SECS);
    let score = Database::calculate_news_score(creation_ts, 0, now);

    db.write_with(|tx| {
        tx.open_table(&social_news_rank_by_post_id::TABLE)?
            .insert(&post_id, &SocialNewsRankRecord { creation_ts, score })?;
        tx.open_table(&social_news_rank_by_score::TABLE)?
            .insert(&(score, post_id), &())?;
        tx.open_table(&social_news_rank_by_time::TABLE)?
            .insert(&(creation_ts, post_id), &())?;

        let updated = Database::recalculate_news_post_score_tx(post_id, now, tx)?;
        assert!(updated);

        assert!(
            tx.open_table(&social_news_rank_by_post_id::TABLE)?
                .get(&post_id)?
                .is_some()
        );
        assert!(
            tx.open_table(&social_news_rank_by_score::TABLE)?
                .get(&(score, post_id))?
                .is_some()
        );
        assert!(
            tx.open_table(&social_news_rank_by_time::TABLE)?
                .get(&(creation_ts, post_id))?
                .is_some()
        );

        Ok(())
    })
    .await?;

    Ok(())
}
