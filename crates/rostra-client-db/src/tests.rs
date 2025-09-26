use rostra_core::EventId;
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent};
use rostra_core::id::{RostraId, RostraIdSecretKey};
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;
use tempfile::{TempDir, tempdir};
use tracing::info;

use crate::event::EventContentState;
use crate::{
    Database, events, events_by_time, events_content, events_content_missing,
    events_content_rc_count, events_heads, events_missing, ids_full,
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
        let mut events_content_table = tx.open_table(&events_content::TABLE).boxed()?;
        let mut events_content_rc_count_table =
            tx.open_table(&events_content_rc_count::TABLE).boxed()?;
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
                    &mut events_content_table,
                    &mut events_content_rc_count_table,
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
        let mut events_content_table = tx.open_table(&events_content::TABLE).boxed()?;
        let mut events_content_rc_count_table =
            tx.open_table(&events_content_rc_count::TABLE).boxed()?;
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
                    &mut events_content_table,
                    &mut events_content_rc_count_table,
                    &mut events_content_missing_table,
                )?;

                for (event_id, expected_state) in [event_a_id, event_b_id, event_c_id, event_d_id]
                    .into_iter()
                    .zip(expected_states)
                {
                    info!(event_id = %event_id, "Checking");
                    let state = Database::get_event_tx(event_id, &events_table)?.map(|_record| {
                        let content =
                            Database::get_event_content_tx(event_id, &events_content_table)
                                .expect("no db errors");
                        info!(event_id = %event_id, ?content, "State");

                        match content {
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
async fn test_event_reference_counting() -> BoxedErrorResult<()> {
    use std::borrow::Cow;

    use rostra_core::event::EventContentRaw;
    use rostra_core::id::ToShort;

    let id_secret = RostraIdSecretKey::generate();
    let (_dir, db) = temp_db(id_secret.id()).await?;

    let event_a = build_test_event(id_secret, None);
    let event_a_id = event_a.event_id.to_short();

    db.write_with(|tx| {
        let mut events_content_table = tx.open_table(&events_content::TABLE)?;
        let mut events_content_rc_count_table = tx.open_table(&events_content_rc_count::TABLE)?;
        let mut events_content_missing_table = tx.open_table(&events_content_missing::TABLE)?;

        // Test initial state - no reference count should exist
        let initial_count =
            Database::get_event_content_rc_tx(event_a_id, &events_content_rc_count_table)?;
        assert_eq!(initial_count, 0, "Initial count should be 0");

        // Insert first event reference
        Database::increment_event_content_rc_tx(event_a_id, &mut events_content_rc_count_table)?;
        let count_after_first =
            Database::get_event_content_rc_tx(event_a_id, &events_content_rc_count_table)?;
        assert_eq!(
            count_after_first, 1,
            "Count should be 1 after first increment"
        );

        // Insert second event reference
        Database::increment_event_content_rc_tx(event_a_id, &mut events_content_rc_count_table)?;
        let count_after_second =
            Database::get_event_content_rc_tx(event_a_id, &events_content_rc_count_table)?;
        assert_eq!(
            count_after_second, 2,
            "Count should be 2 after second increment"
        );

        // Add some content to test cleanup
        let test_content = EventContentRaw::new(vec![1, 2, 3]);
        events_content_table.insert(
            &event_a_id,
            &EventContentState::Present(Cow::Owned(test_content)),
        )?;
        events_content_missing_table.insert(&event_a_id, &())?;

        // Remove first reference - should decrement but not delete content
        let was_deleted = Database::decrement_event_content_rc_tx(
            event_a_id,
            &mut events_content_table,
            &mut events_content_rc_count_table,
            &mut events_content_missing_table,
        )?;
        assert!(
            !was_deleted,
            "Content should not be deleted after first decrement"
        );

        let count_after_first_decrement =
            Database::get_event_content_rc_tx(event_a_id, &events_content_rc_count_table)?;
        assert_eq!(
            count_after_first_decrement, 1,
            "Count should be 1 after first decrement"
        );

        // Content should still exist
        let content_exists = events_content_table.get(&event_a_id)?.is_some();
        assert!(
            content_exists,
            "Content should still exist after first decrement"
        );

        // Remove second reference - should delete everything
        let was_deleted = Database::decrement_event_content_rc_tx(
            event_a_id,
            &mut events_content_table,
            &mut events_content_rc_count_table,
            &mut events_content_missing_table,
        )?;
        assert!(
            was_deleted,
            "Content should be deleted after final decrement"
        );

        // Everything should be cleaned up
        let final_count =
            Database::get_event_content_rc_tx(event_a_id, &events_content_rc_count_table)?;
        assert_eq!(final_count, 0, "Count should be 0 after cleanup");

        let content_exists = events_content_table.get(&event_a_id)?.is_some();
        assert!(
            !content_exists,
            "Content should be deleted after final decrement"
        );

        let missing_exists = events_content_missing_table.get(&event_a_id)?.is_some();
        assert!(
            !missing_exists,
            "Content missing entry should be deleted after final decrement"
        );

        Ok(())
    })
    .await?;

    Ok(())
}
