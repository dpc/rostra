use rostra_core::EventId;
use rostra_core::event::{Event, EventContentRaw, EventExt as _, EventKind, VerifiedEvent};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;
use tempfile::{TempDir, tempdir};
use tracing::info;

use crate::event::EventContentState;
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

/// Test: Event arrives before content (normal flow).
///
/// Flow (new model - RC managed at event insertion):
/// 1. Event is inserted - content not in store yet
/// 2. Event is added to events_content_missing, marked as Unprocessed, RC=1
///    immediately
/// 3. Content is stored
/// 4. Content becomes available (check with is_content_available_for_event_tx)
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_event_arrives_before_content() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

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
            &content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        // Verify: Event should be in events_content_missing
        assert!(
            events_content_missing_table.get(&event_id)?.is_some(),
            "Event should be in events_content_missing"
        );

        // Verify: Event should be marked as Unprocessed (content not yet processed)
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_id, &events_content_state_table)?,
                Some(EventContentState::Unprocessed)
            ),
            "Event should be marked as Unprocessed"
        );

        // Verify: RC should be 1 (incremented at event insertion)
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 - incremented at event insertion");

        // Verify: Content not available yet
        assert!(
            !Database::is_content_available_for_event_tx(
                event_id,
                content_hash,
                &events_content_state_table,
                &content_store_table
            )?,
            "Content should not be available yet"
        );

        // Step 2: Store content in content_store (simulating content arrival)
        let test_content = EventContentRaw::new(vec![]);
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(test_content)),
        )?;

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

        // Verify: RC unchanged (still 1)
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should still be 1");

        // Verify: Still Unprocessed (content stored but not processed yet)
        // Note: Unprocessed state is only removed by process_event_content_tx
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_id, &events_content_state_table)?,
                Some(EventContentState::Unprocessed)
            ),
            "Event should still be Unprocessed (content not yet processed)"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Content exists when event arrives (immediate availability).
///
/// Flow (new model - RC managed at event insertion):
/// 1. Content is pre-stored in content_store (from another event)
/// 2. Event is inserted - RC incremented to 1
/// 3. Event is NOT added to events_content_missing (content already exists)
/// 4. Content is immediately available
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_content_exists_when_event_arrives() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

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
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(test_content)),
        )?;

        // Step 2: Insert event - content already exists
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
            None,
        )?;

        // Verify: Event should NOT be in events_content_missing
        assert!(
            events_content_missing_table.get(&event_id)?.is_none(),
            "Event should NOT be in events_content_missing"
        );

        // Verify: Event should be marked as Unprocessed (content not yet processed)
        let state = Database::get_event_content_state_tx(event_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentState::Unprocessed)),
            "Event should be Unprocessed (content available but not yet processed)"
        );

        // Verify: RC should be 1
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 after event insertion");

        // Verify: Content is available
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
/// Flow (new model - RC managed at event insertion):
/// 1. Event A arrives (no content) -> added to missing, RC=1
/// 2. Content arrives (stored)
/// 3. Event B arrives (content exists) -> RC=2, NOT in missing
/// 4. Prune A -> RC=1, content still exists for B
/// 5. Prune B -> RC=0, content can be GC'd
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_multiple_events_share_content() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

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
            &content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            1,
            "RC=1 after A arrives (incremented at insertion)"
        );
        assert!(
            events_content_missing_table.get(&event_a_id)?.is_some(),
            "A should be in missing"
        );

        // Step 2: Content arrives - just store it
        let test_content = EventContentRaw::new(vec![]);
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(test_content)),
        )?;

        // RC unchanged (still 1)
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            1,
            "RC=1 - content arrival doesn't change RC"
        );

        // Step 3: Event B arrives - content already exists
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
            None,
        )?;

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            2,
            "RC=2 after B arrives"
        );
        assert!(
            events_content_missing_table.get(&event_b_id)?.is_none(),
            "B should NOT be in missing"
        );
        // B should be marked as Unprocessed (content available but not yet processed)
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?,
                Some(EventContentState::Unprocessed)
            ),
            "B should be Unprocessed"
        );

        // Step 4: Prune A's content - RC decrements, B still has content
        Database::prune_event_content_tx(
            event_a_id,
            content_hash,
            &mut events_content_state_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            1,
            "RC=1 after pruning A"
        );
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_a_id, &events_content_state_table)?,
                Some(EventContentState::Pruned)
            ),
            "A should be Pruned"
        );
        assert!(
            content_store_table.get(&content_hash)?.is_some(),
            "Content should still exist (RC > 0)"
        );

        // Step 5: Prune B's content - RC goes to 0
        Database::prune_event_content_tx(
            event_b_id,
            content_hash,
            &mut events_content_state_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            0,
            "RC=0 after pruning B"
        );
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?,
                Some(EventContentState::Pruned)
            ),
            "B should be Pruned"
        );
        // Note: Content still exists in store - would need explicit GC to remove

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Multiple events waiting for same content.
///
/// Flow (new model - RC managed at event insertion):
/// 1. Event A and B arrive (both waiting for content) -> both in missing, RC=2
/// 2. Content is stored
/// 3. Both events can now access the content (no claiming step needed)
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_multiple_events_waiting_for_content() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

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
            &content_store_table,
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
            &content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
            None,
        )?;

        // Both should be in missing, RC=2 (both incremented at insertion)
        assert!(events_content_missing_table.get(&event_a_id)?.is_some());
        assert!(events_content_missing_table.get(&event_b_id)?.is_some());
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            2,
            "RC=2 - both events incremented RC at insertion"
        );

        // Content not available for either event yet
        assert!(
            !Database::is_content_available_for_event_tx(
                event_a_id,
                content_hash,
                &events_content_state_table,
                &content_store_table
            )?,
            "Content should not be available for A"
        );
        assert!(
            !Database::is_content_available_for_event_tx(
                event_b_id,
                content_hash,
                &events_content_state_table,
                &content_store_table
            )?,
            "Content should not be available for B"
        );

        // Step 2: Content is stored
        let test_content = EventContentRaw::new(vec![]);
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(test_content)),
        )?;

        // RC unchanged
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            2
        );

        // Step 3: Both events can now access content
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

        // Both should be marked as Unprocessed (content not yet processed)
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_a_id, &events_content_state_table)?,
                Some(EventContentState::Unprocessed)
            ),
            "A should be Unprocessed"
        );
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?,
                Some(EventContentState::Unprocessed)
            ),
            "B should be Unprocessed"
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
        let content_store_table = tx.open_table(&content_store::TABLE)?;
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
            &content_store_table,
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
            &content_store_table,
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
/// - A follow with older or equal timestamp is rejected
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
        };
        let result = Database::insert_follow_tx(
            author,
            ts_100,
            follow_content.clone(),
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(result, "Initial follow should succeed");

        // Verify the record exists with ts=100
        let record = followees_table.get(&(author, followee))?.unwrap().value();
        assert_eq!(record.ts, ts_100);

        // Try to follow with older timestamp - should be rejected
        let result = Database::insert_follow_tx(
            author,
            Timestamp::from(50),
            follow_content.clone(),
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(!result, "Follow with older timestamp should be rejected");

        // Try to follow with same timestamp - should be rejected
        let result = Database::insert_follow_tx(
            author,
            ts_100,
            follow_content.clone(),
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(!result, "Follow with same timestamp should be rejected");

        // Follow with newer timestamp - should succeed
        let result = Database::insert_follow_tx(
            author,
            ts_200,
            follow_content,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(result, "Follow with newer timestamp should succeed");

        // Verify the record was updated
        let record = followees_table.get(&(author, followee))?.unwrap().value();
        assert_eq!(record.ts, ts_200);

        // Now test unfollow timestamp ordering
        // Unfollow with older timestamp than current follow - should be rejected
        let result = Database::insert_unfollow_tx(
            author,
            ts_150, // older than ts_200
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
            ts_300,
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
        };
        let result = Database::insert_follow_tx(
            author,
            ts_200, // older than ts_300 unfollow
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
            ts_400,
            follow_content2,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(
            result,
            "Follow with timestamp newer than unfollow should succeed"
        );

        // Try to unfollow with timestamp older than both current follow and unfollow
        // This tests the second <= check in insert_unfollow_tx
        let result = Database::insert_unfollow_tx(
            author,
            ts_300, // equal to old unfollow, older than current follow
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
            ts_100,
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(result, "Initial unfollow should succeed");

        // Verify unfollow record exists
        let record = unfollowed_table.get(&(author, followee))?.unwrap().value();
        assert_eq!(record.ts, ts_100);

        // Try to unfollow again with same timestamp - should be rejected
        let result = Database::insert_unfollow_tx(
            author,
            ts_100,
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(!result, "Unfollow with same timestamp should be rejected");

        // Try to unfollow with older timestamp - should be rejected
        let result = Database::insert_unfollow_tx(
            author,
            Timestamp::from(50),
            followee,
            &mut followees_table,
            &mut followers_table,
            &mut unfollowed_table,
        )?;
        assert!(!result, "Unfollow with older timestamp should be rejected");

        // Unfollow with newer timestamp - should succeed and update record
        let result = Database::insert_unfollow_tx(
            author,
            ts_200,
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
/// Verifies that values with older or equal timestamps are rejected while
/// newer timestamps update the stored value.
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

        // Try to insert with same timestamp - should be rejected
        let result = Database::insert_latest_value_tx(
            ts_100,
            &author,
            profile_bob.clone(),
            &mut profiles_table,
        )?;
        assert!(!result, "Insert with same timestamp should be rejected");

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

/// Test: Data usage tracking for metadata and content sizes.
///
/// Verifies that metadata size increases when events are added, and content
/// size tracks Available content correctly.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_tracking() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

    use crate::event::ContentStoreRecord;
    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Create events with content
    let event_a = build_test_event(id_secret, None);
    let event_b = build_test_event(id_secret, event_a.event_id);
    let content_hash = event_a.content_hash();

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

        // Initially, no data usage
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(usage.metadata_size, 0);
        assert_eq!(usage.content_size, 0);

        // Insert event A (content not in store yet)
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
            Some(&mut ids_data_usage_table),
        )?;

        // Check metadata size increased, content still 0 (content not claimed)
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(
            usage.metadata_size,
            Database::EVENT_METADATA_SIZE,
            "Metadata should be 192 bytes for one event"
        );
        assert_eq!(usage.content_size, 0, "Content should still be 0");

        // Store content in content_store
        let test_content = EventContentRaw::new(vec![1, 2, 3, 4, 5]); // 5 bytes
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(test_content)),
        )?;

        // Insert event B (content exists, should be claimed immediately)
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
            Some(&mut ids_data_usage_table),
        )?;

        // Check metadata doubled, and content size now reflects B's claim
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(
            usage.metadata_size,
            Database::EVENT_METADATA_SIZE * 2,
            "Metadata should be 384 bytes for two events"
        );
        // Note: content_len is from the event header (0 for our test events from
        // build_test_event) Since build_test_event uses Event::default() which
        // has content_len = 0
        assert_eq!(
            usage.content_size, 0,
            "Content size tracks event.content_len (0 for test events)"
        );

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Data usage tracks content size correctly when content is claimed.
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_data_usage_content_tracking() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

    use rostra_core::event::{Event, EventExt as _, EventKind, VerifiedEvent};

    use crate::event::ContentStoreRecord;
    use crate::ids_data_usage;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();
    let (_dir, db) = temp_db(author).await?;

    // Create an event with a non-zero content_len (1000 bytes of content)
    let content_data = vec![0u8; 1000];
    let content = EventContentRaw::new(content_data.clone());
    let content_len = content.len() as u32;

    let event = {
        let base_event = Event::builder_raw_content()
            .author(author)
            .kind(EventKind::SOCIAL_POST)
            .content(&content)
            .build();
        let signed = base_event.signed_by(id_secret);
        VerifiedEvent::verify_signed(author, signed).expect("Valid event")
    };
    let content_hash = event.content_hash();

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

        // Store content first so it's claimed immediately on insert
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(content.clone())),
        )?;

        // Insert the event
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
            Some(&mut ids_data_usage_table),
        )?;

        // Verify data usage
        let usage = Database::get_data_usage_tx(author, &ids_data_usage_table)?;
        assert_eq!(usage.metadata_size, Database::EVENT_METADATA_SIZE);
        assert_eq!(
            usage.content_size,
            u64::from(content_len),
            "Content size should match content_len from event"
        );

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
        let content_store_table = tx.open_table(&content_store::TABLE)?;
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
            &content_store_table,
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
        let content_store_table = tx.open_table(&content_store::TABLE)?;
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
            &content_store_table,
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
        let content_store_table = tx.open_table(&content_store::TABLE)?;
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
            &content_store_table,
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

            // Count events that are NOT deleted/pruned (new model: RC managed at insertion)
            // Events with no state or Unprocessed state contribute to RC.
            // Only Deleted and Pruned events don't contribute.
            let has_rc = match state {
                None => true,                                 /* Content processed, contributing
                                                                * to RC */
                Some(EventContentState::Unprocessed) => true, /* Not yet processed, but
                                                                * contributes to RC */
                Some(EventContentState::Deleted { .. }) | Some(EventContentState::Pruned) => false,
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
    /// - parent_aux can reference any earlier event (including from other
    ///   authors)
    /// - delete_idx can be set to delete an earlier event's content (mutually
    ///   exclusive with parent_aux)
    fn generate_event_dag(
        num_events: usize,
        rng_seed: u64,
    ) -> (Vec<TestEventSpec>, Vec<(usize, bool)>) {
        let mut rng = StdRng::seed_from_u64(rng_seed);
        let mut events = Vec::new();
        let mut last_event_by_author: [Option<usize>; 3] = [None, None, None];

        for i in 0..num_events {
            let author_idx = rng.random_range(0..3);
            let content_idx = rng.random_range(0..10);

            // parent_prev is the last event from this author
            let parent_prev_idx = last_event_by_author[author_idx];

            // Decide between parent_aux and delete (mutually exclusive)
            let (parent_aux_idx, delete_idx) = if i > 0 {
                let choice = rng.random_range(0..10);
                if choice < 2 {
                    // 20% chance: delete an earlier event
                    (None, Some(rng.random_range(0..i)))
                } else if choice < 5 {
                    // 30% chance: have a parent_aux
                    (Some(rng.random_range(0..i)), None)
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
        proptest!(ProptestConfig::with_cases(500), |(
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
                                &ContentStoreRecord::Present(Cow::Owned(content.raw.clone())),
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
                            &content_store_table,
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
        #![proptest_config(ProptestConfig::with_cases(500))]

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
                        let content_store_table = tx.open_table(&content_store::TABLE)?;
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
                            &content_store_table,
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
    use rostra_core::event::{PersonaId, VerifiedEventContent, content_kind};
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
        let content = content_kind::SocialPost {
            persona: PersonaId(0),
            djot_content: Some(djot_content.to_string()),
            reply_to,
            reaction: None,
        };
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
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_total_migration() -> BoxedErrorResult<()> {
    use rostra_core::Timestamp;
    use rostra_core::event::content_kind::PersonaSelector;
    use rostra_core::event::{PersonaId, VerifiedEventContent, content_kind};

    use crate::{db_version, ids_followees, ids_followers, social_posts_by_time};

    let user_a_secret = RostraIdSecretKey::generate();
    let user_a = user_a_secret.id();

    let user_b_secret = RostraIdSecretKey::generate();
    let user_b = user_b_secret.id();

    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("db.redb");

    // Phase 1: Create database with data
    {
        let db = Database::open(&db_path, user_a).await.boxed()?;

        // Create a follow event (user_a follows user_b)
        // Note: selector must be Some to be a follow, None means unfollow
        let follow_content = content_kind::Follow {
            followee: user_b,
            persona: None,
            selector: Some(PersonaSelector::default()), // Follow all personas
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
                .build();
            let signed = event.signed_by(user_a_secret);
            VerifiedEvent::verify_signed(user_a, signed).expect("Valid event")
        };

        // Create a follow event (user_b follows user_a) - to test "who follows me"
        let reverse_follow_content = content_kind::Follow {
            followee: user_a,
            persona: None,
            selector: Some(PersonaSelector::default()),
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
        let post_content = content_kind::SocialPost {
            persona: PersonaId(0),
            djot_content: Some("Hello world!".to_string()),
            reply_to: None,
            reaction: None,
        };
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
            assert!(
                followee_record.selector.is_some(),
                "Selector should be Some for an active follow"
            );
            info!(
                "Followee record before migration: ts={:?}, selector={:?}",
                followee_record.ts, followee_record.selector
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

        // Database is dropped here
    }

    // Phase 2: Manually downgrade db version to trigger migration
    {
        let raw_db = redb_bincode::Database::from(redb::Database::open(&db_path).boxed()?);
        let write_txn = raw_db.begin_write().boxed()?;
        {
            let mut table = write_txn.open_table(&db_version::TABLE).boxed()?;
            // Set version to 1 to trigger total migration
            let old_version: u64 = 1;
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
        // Note: We can't directly check against DB_VER since it's private,
        // but we can check it's greater than 1
        assert!(
            current_ver.is_some() && current_ver.unwrap() > 1,
            "DB version should be updated after migration"
        );

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
        assert!(
            followee_record.selector.is_some(),
            "Selector should be Some for an active follow after migration"
        );
        info!(
            "Followee record after migration: ts={:?}, selector={:?}",
            followee_record.ts, followee_record.selector
        );

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
    use rostra_core::event::{PersonaId, VerifiedEventContent, content_kind};

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
        let content = content_kind::SocialPost {
            persona: PersonaId(0),
            djot_content: Some(djot_content.to_string()),
            reply_to,
            reaction: None,
        };
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
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::event::{PersonaId, content_kind};
    use rostra_core::id::ToShort as _;

    let _ = tracing_subscriber::fmt::try_init();

    let id_secret_a = RostraIdSecretKey::generate();
    let user_a = id_secret_a.id();

    let id_secret_b = RostraIdSecretKey::generate();
    let user_b = id_secret_b.id();

    let (_tmp, db) = temp_db(user_a).await?;

    // Create a post from user A
    let post_content = content_kind::SocialPost {
        persona: PersonaId(0),
        djot_content: Some("Test post".to_string()),
        reply_to: None,
        reaction: None,
    };
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
    let reply_content = content_kind::SocialPost {
        persona: PersonaId(0),
        djot_content: Some("Reply".to_string()),
        reply_to: Some(ExternalEventId::new(user_a, post_event_id)),
        reaction: None,
    };
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
            matches!(state, Some(EventContentState::Unprocessed)),
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
            matches!(state, Some(EventContentState::Unprocessed)),
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
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::event::{PersonaId, content_kind};
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post
    let post_content = content_kind::SocialPost {
        persona: PersonaId(0),
        djot_content: Some("Test post".to_string()),
        reply_to: None,
        reaction: None,
    };
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
            matches!(state, Some(EventContentState::Unprocessed)),
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
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::event::{PersonaId, content_kind};
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post
    let post_content = content_kind::SocialPost {
        persona: PersonaId(0),
        djot_content: Some("Test post".to_string()),
        reply_to: None,
        reaction: None,
    };
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
        assert!(
            matches!(state, Some(EventContentState::Deleted { .. })),
            "Post should still be Deleted"
        );

        Ok(())
    })
    .await?;

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
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::event::{PersonaId, content_kind};
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post
    let post_content = content_kind::SocialPost {
        persona: PersonaId(0),
        djot_content: Some("Test post".to_string()),
        reply_to: None,
        reaction: None,
    };
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
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::event::{PersonaId, content_kind};
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post
    let post_content = content_kind::SocialPost {
        persona: PersonaId(0),
        djot_content: Some("Test post".to_string()),
        reply_to: None,
        reaction: None,
    };
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

/// Test processing content for an event that was never inserted.
///
/// Verifies that this is handled gracefully (skipped, not crash) in release
/// mode. In debug mode, this will panic via debug_assert - that's intentional.
#[cfg(not(debug_assertions))]
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_process_content_for_nonexistent_event() -> BoxedErrorResult<()> {
    use rostra_core::event::content_kind::EventContentKind as _;
    use rostra_core::event::{PersonaId, VerifiedEventContent, content_kind};
    use rostra_core::id::ToShort as _;

    let user_secret = RostraIdSecretKey::generate();
    let user = user_secret.id();

    let (_tmp, db) = temp_db(user).await?;

    // Create a post event but DON'T insert it
    let post_content = content_kind::SocialPost {
        persona: PersonaId(0),
        djot_content: Some("Test post".to_string()),
        reply_to: None,
        reaction: None,
    };
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
