use rostra_core::EventId;
use rostra_core::event::{Event, EventContentRaw, EventExt as _, EventKind, VerifiedEvent};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;
use tempfile::{TempDir, tempdir};
use tracing::info;

use crate::event::EventContentStateNew;
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
        let content_store_table = tx.open_table(&content_store::TABLE).boxed()?;
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
                    &content_store_table,
                    &mut content_rc_table,
                    &mut events_content_missing_table,
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

    db.write_with(|tx| {
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE).boxed()?;
        let mut events_table = tx.open_table(&events::TABLE).boxed()?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE).boxed()?;
        let content_store_table = tx.open_table(&content_store::TABLE).boxed()?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE).boxed()?;
        let mut events_content_missing_table =
            tx.open_table(&events_content_missing::TABLE).boxed()?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE).boxed()?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE).boxed()?;

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
                    // Note: event_c also deleted `a`, so we're kind of testing impl details here
                    Some(Some(event_b_id.into())),
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
                    &content_store_table,
                    &mut content_rc_table,
                    &mut events_content_missing_table,
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
                            Some(EventContentStateNew::Deleted { deleted_by }) => Some(deleted_by),
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

/// Test content deduplication - same content from different events shares
/// storage.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_content_deduplication() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

    use rostra_core::id::ToShort;

    use crate::event::ContentStoreRecord;

    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;

    // Create two events with the same content
    let event_a = build_test_event(id_secret, None);
    let event_a_id = event_a.event_id.to_short();
    let event_b = build_test_event(id_secret, event_a.event_id);
    let event_b_id = event_b.event_id.to_short();

    // Both events have empty content, so they share the same content hash
    let content_hash = event_a.content_hash();
    assert_eq!(
        content_hash,
        event_b.content_hash(),
        "Both events should have the same content hash"
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

        // Insert first event
        Database::insert_event_tx(
            event_a,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
        )?;

        // RC should be 1 for this content hash
        let rc_after_first = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc_after_first, 1, "RC should be 1 after first event");

        // Insert second event with same content
        Database::insert_event_tx(
            event_b,
            &mut ids_full_tbl,
            &mut events_table,
            &mut events_missing_table,
            &mut events_heads_table,
            &mut events_by_time_table,
            &mut events_content_state_table,
            &content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
        )?;

        // RC should be 2 now - both events reference same content
        let rc_after_second = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc_after_second, 2, "RC should be 2 after second event");

        // Store some content to make the test complete
        let test_content = EventContentRaw::new(vec![]);
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(test_content)),
        )?;
        events_content_state_table.insert(&event_a_id, &EventContentStateNew::Available)?;
        events_content_state_table.insert(&event_b_id, &EventContentStateNew::Available)?;

        // Now prune event_a's content - RC should decrement
        Database::prune_event_content_tx(
            event_a_id,
            content_hash,
            &mut events_content_state_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
        )?;

        let rc_after_prune = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(
            rc_after_prune, 1,
            "RC should be 1 after pruning first event"
        );

        // Event_b should still see content as available
        let state_b =
            Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?;
        assert!(
            matches!(state_b, Some(EventContentStateNew::Available)),
            "Event B should still have content available"
        );

        // Content should still exist in store
        let content_exists = content_store_table.get(&content_hash)?.is_some();
        assert!(
            content_exists,
            "Content should still exist in store (RC > 0)"
        );

        Ok(())
    })
    .await?;

    Ok(())
}
