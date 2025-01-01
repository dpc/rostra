use rostra_core::event::EventKind::SocialPost;
use rostra_core::event::{Event, EventContent, VerifiedEvent};
use rostra_core::id::RostraIdSecretKey;
use rostra_core::EventId;
use rostra_util_error::BoxedErrorResult;
use snafu::ResultExt as _;
use tempfile::{tempdir, TempDir};
use tracing::info;

use crate::db::tables::{TABLE_EVENTS, TABLE_EVENTS_HEADS, TABLE_EVENTS_MISSING};
use crate::db::Database;

async fn temp_db() -> BoxedErrorResult<(TempDir, super::Database)> {
    let dir = tempdir()?;
    let db = super::Database::open(dir.path().join("db.redb"))
        .await
        .boxed()?;

    Ok((dir, db))
}

fn build_test_event(
    id_secret: RostraIdSecretKey,
    parent: impl Into<Option<EventId>>,
) -> VerifiedEvent {
    let parent = parent.into();

    let content = EventContent::from(vec![]);
    let event = Event::builder()
        .author(id_secret.id())
        .kind(SocialPost)
        .maybe_parent_prev(parent.map(Into::into))
        .content(content.clone())
        .build();

    let signed_event = event.signed_by(id_secret);

    VerifiedEvent::verify_signed(signed_event, content).expect("Valid event")
}

#[test_log::test(tokio::test(flavor = "multi_thread"))]
async fn test_store_event() -> BoxedErrorResult<()> {
    let (_dir, db) = temp_db().await?;

    let id_secret = RostraIdSecretKey::generate();
    let author = id_secret.id();

    let event_a = build_test_event(id_secret, None);
    let event_a_id = event_a.event_id;
    let event_b = build_test_event(id_secret, event_a.event_id);
    let event_b_id = event_b.event_id;
    let event_c = build_test_event(id_secret, event_b.event_id);
    let event_c_id = event_c.event_id;
    let event_d = build_test_event(id_secret, event_c.event_id);
    let event_d_id = event_d.event_id;

    db.write_with(|tx| {
        let mut events_table = tx.open_table(&TABLE_EVENTS).boxed()?;
        let mut events_missing_table = tx.open_table(&TABLE_EVENTS_MISSING).boxed()?;
        let mut events_heads_table = tx.open_table(&TABLE_EVENTS_HEADS).boxed()?;

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
                    &event,
                    &mut events_table,
                    &mut events_missing_table,
                    &mut events_heads_table,
                )?;

                info!(event_id = %event.event_id, "Checking missing");
                let missing = Database::get_missing_events_tx(author, &events_missing_table)?;
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
