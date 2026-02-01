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

/// Test: Event arrives before content (normal flow).
///
/// Flow:
/// 1. Event is inserted - content not in store yet
/// 2. Event is added to events_content_missing, no RC, no state
/// 3. Content is stored and event claims it via try_claim_content_tx
/// 4. RC becomes 1, event marked Available, removed from missing
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
        )?;

        // Verify: Event should be in events_content_missing
        assert!(
            events_content_missing_table.get(&event_id)?.is_some(),
            "Event should be in events_content_missing"
        );

        // Verify: No state set for event
        assert!(
            Database::get_event_content_state_tx(event_id, &events_content_state_table)?.is_none(),
            "Event should have no content state yet"
        );

        // Verify: RC should be 0
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 0, "RC should be 0 - event hasn't claimed content");

        // Step 2: Store content in content_store (simulating content arrival)
        let test_content = EventContentRaw::new(vec![]);
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(test_content)),
        )?;

        // Step 3: Event claims content via try_claim_content_tx
        let claimed = Database::try_claim_content_tx(
            event_id,
            content_hash,
            &mut events_content_state_table,
            &content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
        )?;
        assert!(claimed, "Event should successfully claim content");

        // Verify: Event removed from missing
        assert!(
            events_content_missing_table.get(&event_id)?.is_none(),
            "Event should be removed from events_content_missing"
        );

        // Verify: Event marked as Available
        let state = Database::get_event_content_state_tx(event_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentStateNew::Available)),
            "Event should be marked as Available"
        );

        // Verify: RC is now 1
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 after claiming");

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Content exists when event arrives (immediate claiming).
///
/// Flow:
/// 1. Content is pre-stored in content_store (from another event)
/// 2. Event is inserted - detects content exists
/// 3. RC is incremented immediately, event marked Available
/// 4. Event is NOT added to events_content_missing
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
        )?;

        // Verify: Event should NOT be in events_content_missing
        assert!(
            events_content_missing_table.get(&event_id)?.is_none(),
            "Event should NOT be in events_content_missing"
        );

        // Verify: Event should be marked as Available immediately
        let state = Database::get_event_content_state_tx(event_id, &events_content_state_table)?;
        assert!(
            matches!(state, Some(EventContentStateNew::Available)),
            "Event should be marked as Available immediately"
        );

        // Verify: RC should be 1
        let rc = Database::get_content_rc_tx(content_hash, &content_rc_table)?;
        assert_eq!(rc, 1, "RC should be 1 after event claims content");

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Multiple events with same content hash share storage.
///
/// Flow:
/// 1. Event A arrives (no content) -> added to missing, RC=0
/// 2. Content arrives for A -> stored, A claims it, RC=1
/// 3. Event B arrives (content exists) -> claims immediately, RC=2
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
        )?;

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            0,
            "RC=0 after A arrives (no content yet)"
        );
        assert!(
            events_content_missing_table.get(&event_a_id)?.is_some(),
            "A should be in missing"
        );

        // Step 2: Content arrives for A - store and claim
        let test_content = EventContentRaw::new(vec![]);
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(test_content)),
        )?;
        Database::try_claim_content_tx(
            event_a_id,
            content_hash,
            &mut events_content_state_table,
            &content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
        )?;

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            1,
            "RC=1 after A claims content"
        );

        // Step 3: Event B arrives - content already exists, claims immediately
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

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            2,
            "RC=2 after B claims content"
        );
        assert!(
            events_content_missing_table.get(&event_b_id)?.is_none(),
            "B should NOT be in missing"
        );
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?,
                Some(EventContentStateNew::Available)
            ),
            "B should be Available"
        );

        // Step 4: Prune A's content - RC decrements, B still has content
        Database::prune_event_content_tx(
            event_a_id,
            content_hash,
            &mut events_content_state_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
        )?;

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            1,
            "RC=1 after pruning A"
        );
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_a_id, &events_content_state_table)?,
                Some(EventContentStateNew::Pruned)
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
        )?;

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            0,
            "RC=0 after pruning B"
        );
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?,
                Some(EventContentStateNew::Pruned)
            ),
            "B should be Pruned"
        );
        // Note: Content still exists in store - would need explicit GC to remove

        Ok(())
    })
    .await?;

    Ok(())
}

/// Test: Lazy claiming for event that was waiting when content arrived via
/// another event.
///
/// Flow:
/// 1. Event A and B arrive (both waiting for content) -> both in missing, RC=0
/// 2. Content is stored (simulating arrival via A)
/// 3. A claims content -> RC=1
/// 4. B is still in missing (lazy - hasn't been processed yet)
/// 5. B calls try_claim_content_tx -> RC=2, B marked Available
#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_lazy_content_claiming() -> BoxedErrorResult<()> {
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
        )?;

        // Both should be in missing
        assert!(events_content_missing_table.get(&event_a_id)?.is_some());
        assert!(events_content_missing_table.get(&event_b_id)?.is_some());
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            0
        );

        // Step 2: Content is stored
        let test_content = EventContentRaw::new(vec![]);
        content_store_table.insert(
            &content_hash,
            &ContentStoreRecord::Present(Cow::Owned(test_content)),
        )?;

        // Step 3: A claims content
        Database::try_claim_content_tx(
            event_a_id,
            content_hash,
            &mut events_content_state_table,
            &content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
        )?;

        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            1
        );
        assert!(events_content_missing_table.get(&event_a_id)?.is_none());

        // Step 4: B is still in missing (lazy - not processed yet)
        assert!(
            events_content_missing_table.get(&event_b_id)?.is_some(),
            "B should still be in missing (lazy)"
        );
        assert!(
            Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?
                .is_none(),
            "B should have no state yet"
        );

        // Step 5: B lazily claims content
        let claimed = Database::try_claim_content_tx(
            event_b_id,
            content_hash,
            &mut events_content_state_table,
            &content_store_table,
            &mut content_rc_table,
            &mut events_content_missing_table,
        )?;

        assert!(claimed, "B should successfully claim content");
        assert_eq!(
            Database::get_content_rc_tx(content_hash, &content_rc_table)?,
            2
        );
        assert!(events_content_missing_table.get(&event_b_id)?.is_none());
        assert!(
            matches!(
                Database::get_event_content_state_tx(event_b_id, &events_content_state_table)?,
                Some(EventContentStateNew::Available)
            ),
            "B should be Available after lazy claim"
        );

        Ok(())
    })
    .await?;

    Ok(())
}
