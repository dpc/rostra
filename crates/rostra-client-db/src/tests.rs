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

    use crate::event::{ContentStoreRecord, EventContentStateNew};
    use crate::{
        Database, content_rc, content_store, events, events_by_time, events_content_missing,
        events_content_state, events_heads, events_missing, ids_full,
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
    /// An event contributes +1 to RC for its content_hash if and only if
    /// its state is `Available`. No state, `Pruned`, or `Deleted` means no RC.
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

            // Only count events with Available state
            if matches!(state, Some(EventContentStateNew::Available)) {
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
        proptest!(ProptestConfig::with_cases(50), |(
            seed in 0u64..10000,
            num_events in 5usize..20,
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

                        // If event was already inserted, try to claim content
                        if events_inserted.contains(&event_idx) {
                            let event_id = event_ids[event_idx].unwrap().to_short();
                            Database::try_claim_content_tx(
                                event_id,
                                content.hash,
                                &mut events_content_state_table,
                                &content_store_table,
                                &mut content_rc_table,
                                &mut events_content_missing_table,
                            )?;
                        }

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
