use rostra_core::Timestamp;
use rostra_core::event::content_kind::Shoutbox;
use rostra_core::event::{EventExt as _, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraIdSecretKey;

use crate::news_request_tests::{ingest, signed};
use crate::shoutbox::{ShoutboxBounds, ShoutboxIssue, ShoutboxStop};
use crate::shoutbox_request::{ShoutboxRequest, ShoutboxRequestId};
use crate::{Database, DbError};

fn bounds() -> ShoutboxBounds {
    ShoutboxBounds {
        rows: 100,
        raw_rows: 4096,
        payload_bytes: 16 * 1024 * 1024,
    }
}

#[test]
fn shoutbox_permanent_v1_vectors() {
    let account = rostra_core::id::RostraId::from_bytes([3; 32]);
    let request = ShoutboxRequest {
        body: "hello".into(),
    };
    assert_eq!(
        request.digest(account),
        [
            237, 253, 50, 202, 107, 106, 4, 173, 246, 227, 226, 24, 121, 50, 250, 11, 248, 96, 163,
            152, 190, 161, 81, 74, 69, 114, 93, 85, 219, 18, 125, 60,
        ]
    );
    assert_eq!(
        ShoutboxRequestId([6; 16]).event_aux_key(account).to_bytes(),
        [
            225, 31, 220, 105, 135, 57, 15, 5, 168, 55, 44, 96, 66, 27, 244, 139,
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_fresh_key_rejects_unbound_candidates_without_mutation() -> anyhow::Result<()> {
    use rostra_core::event::{Event, EventContentKind as _, EventKind};
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let request = ShoutboxRequest {
        body: "requested".into(),
    };
    let id = ShoutboxRequestId([1; 16]);
    let raw = request.to_shoutbox()?.serialize_cbor()?;
    let other_raw = Shoutbox {
        djot_content: "different".into(),
    }
    .serialize_cbor()?;
    let mut ordinary = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SHOUTBOX)
        .content(&raw)
        .build();
    ordinary.key_aux = id.event_aux_key(secret.id());
    let mut singleton = ordinary;
    singleton.flags |= Event::SINGLETON_FLAG;
    let mut deletion = ordinary;
    deletion.flags |= Event::DELETE_PARENT_AUX_CONTENT_FLAG;
    let mut auxiliary = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SHOUTBOX)
        .parent_aux(rostra_core::ShortEventId::from_bytes([9; 16]))
        .content(&raw)
        .build();
    auxiliary.key_aux = id.event_aux_key(secret.id());
    let mut wrong_body = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SHOUTBOX)
        .content(&other_raw)
        .build();
    wrong_body.key_aux = id.event_aux_key(secret.id());
    let mut wrong_nonce = ordinary;
    wrong_nonce.key_aux = ShoutboxRequestId([2; 16]).event_aux_key(secret.id());
    let mut wrong_kind = ordinary;
    wrong_kind.kind = EventKind::SOCIAL_POST;
    let frontier = db
        .read_with(|tx| {
            Ok(tx
                .open_table(&crate::reception_order_next::TABLE)?
                .get(&())?
                .map(|r| r.value_try())
                .transpose()?)
        })
        .await?;
    for (event, bytes) in [
        (wrong_body, other_raw),
        (wrong_nonce, raw.clone()),
        (singleton, raw.clone()),
        (deletion, raw.clone()),
        (auxiliary, raw.clone()),
        (wrong_kind, raw.clone()),
    ] {
        let content = VerifiedEventContent::verify(
            VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))?,
            bytes,
        )?;
        assert!(matches!(
            db.commit_shoutbox(id, &request, &content, &mut None, &mut None)
                .await,
            Err(DbError::ShoutboxCandidate)
        ));
        assert!(db.shoutbox_receipt(id, &request).await?.is_none());
        assert!(db.get_event(content.event_id()).await.is_none());
        assert!(db.paginate_shoutbox(None, bounds()).await?.rows.is_empty());
        let current = db
            .read_with(|tx| {
                Ok(tx
                    .open_table(&crate::reception_order_next::TABLE)?
                    .get(&())?
                    .map(|r| r.value_try())
                    .transpose()?)
            })
            .await?;
        assert_eq!(current, frontier);
    }
    Ok(())
}

fn candidate(secret: RostraIdSecretKey, id: u8, request: &ShoutboxRequest) -> VerifiedEventContent {
    let mut content = signed(secret, &request.to_shoutbox().unwrap(), 10, None);
    let mut event = content.event.event;
    event.key_aux = ShoutboxRequestId([id; 16]).event_aux_key(secret.id());
    content.event = VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret)).unwrap();
    content
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_raw_index_corruption_is_fallible_not_a_typed_iterator_panic() -> anyhow::Result<()>
{
    use rostra_core::id::ToShort as _;
    for corruption in 0..4 {
        let secret = RostraIdSecretKey::generate();
        let db = Database::new_in_memory(secret.id()).await?;
        let post = signed(
            secret,
            &Shoutbox {
                djot_content: "row".into(),
            },
            1,
            None,
        );
        ingest(&db, &post).await?;
        let received = db.paginate_shoutbox(None, bounds()).await?.rows[0].received;
        let key =
            bincode::encode_to_vec((received.ts, received.seq), redb_bincode::BINCODE_CONFIG)?;
        let value =
            bincode::encode_to_vec(post.event_id().to_short(), redb_bincode::BINCODE_CONFIG)?;
        let mut bad_key = key.clone();
        let mut bad_value = value;
        match corruption {
            0 => {
                bad_key.pop();
            }
            1 => bad_key.push(0),
            2 => {
                bad_value.pop();
            }
            3 => bad_value.push(0),
            _ => unreachable!(),
        }
        db.write_with(|tx| {
            let mut raw = tx
                .as_raw()
                .open_table(crate::shoutbox_posts_by_received_at::TABLE.as_raw())?;
            raw.remove(key.as_slice())?;
            raw.insert(bad_key.as_slice(), bad_value.as_slice())?;
            Ok(())
        })
        .await?;
        assert!(db.paginate_shoutbox(None, bounds()).await.is_err());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_requests_are_independent_exact_and_receipt_first() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let request = ShoutboxRequest {
        body: "hello".into(),
    };
    let a = candidate(secret, 1, &request);
    let b = candidate(secret, 2, &request);
    assert_eq!(a.parent_prev(), b.parent_prev());
    assert_eq!(a.timestamp(), b.timestamp());
    assert_ne!(a.event_id(), b.event_id());
    let commit = async |id, content| {
        db.commit_shoutbox(id, &request, content, &mut None, &mut None)
            .await
    };
    // Distinct intents may share head/time/body, but never their result.
    let (x, y) = tokio::join!(
        commit(ShoutboxRequestId([1; 16]), &a),
        commit(ShoutboxRequestId([2; 16]), &b),
    );
    assert_ne!(x?.event_id, y?.event_id);
    assert_eq!(db.paginate_shoutbox(None, bounds()).await?.rows.len(), 2);
    // A malformed candidate cannot override an already accepted request.
    assert_eq!(
        db.commit_shoutbox(
            ShoutboxRequestId([1; 16]),
            &request,
            &b,
            &mut None,
            &mut None
        )
        .await?
        .event_id,
        a.event_id()
    );
    let invalid = ShoutboxRequest {
        body: " hello ".into(),
    };
    assert!(matches!(
        db.commit_shoutbox(
            ShoutboxRequestId([1; 16]),
            &invalid,
            &a,
            &mut None,
            &mut None
        )
        .await,
        Err(DbError::ShoutboxRequestMismatch)
    ));
    assert!(matches!(
        db.commit_shoutbox(
            ShoutboxRequestId([3; 16]),
            &invalid,
            &a,
            &mut None,
            &mut None
        )
        .await,
        Err(DbError::ShoutboxInvalidInput)
    ));
    Ok(())
}

#[test]
fn shoutbox_input_uses_exact_trimmed_utf8_bytes() {
    for body in [
        "".into(),
        " ".into(),
        "\nhi".into(),
        "hi\u{2003}".into(),
        "é".repeat(501),
    ] {
        assert!(ShoutboxRequest { body }.to_shoutbox().is_err());
    }
    for body in ["hi\nthere".into(), "é".repeat(500)] {
        assert_eq!(
            ShoutboxRequest { body: body.clone() }
                .to_shoutbox()
                .unwrap()
                .djot_content,
            body
        );
    }
    assert!(Shoutbox::decode_supported(&[0xa1, 0x61, b'c', 0x81, 0x80]).is_none());
    assert!(Shoutbox::decode_supported(&[0xa1, 0x61, b'x', 0x81, 0x80]).is_none());
    // Duplicate body, trailing value, invalid UTF-8, and huge declared length
    // are unsupported without recursive parsing or length-sized allocation.
    for bytes in [
        &[0xa2, 0x61, b'c', 0x61, b'x', 0x61, b'c', 0x61, b'y'][..],
        &[0xa1, 0x61, b'c', 0x61, b'x', 0x00],
        &[0xa1, 0x61, b'c', 0x61, 0xff],
        &[
            0xa1, 0x61, b'c', 0x7b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ],
    ] {
        assert!(Shoutbox::decode_supported(bytes).is_none());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_bounded_pages_placeholder_and_new_allocation_exclusion() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    for time in 1..=4 {
        ingest(
            &db,
            &signed(
                secret,
                &Shoutbox {
                    djot_content: format!("post {time}"),
                },
                time,
                None,
            ),
        )
        .await?;
    }
    let small = ShoutboxBounds {
        rows: 1,
        ..bounds()
    };
    let first = db.paginate_shoutbox(None, small).await?;
    assert_eq!(first.rows.len(), 1);
    let original = first.next_cursor.unwrap();
    assert_eq!(original.next_sequence, first.rows[0].received.seq + 1);
    let later = signed(
        secret,
        &Shoutbox {
            djot_content: "backdated".into(),
        },
        0,
        None,
    );
    db.write_with(|tx| {
        db.process_event_tx(&later.event, Timestamp::from(5), tx)?;
        db.process_event_content_tx(&later, Timestamp::from(5), tx)
    })
    .await?;
    let mut cursor = Some(original.clone());
    let mut remaining = Vec::new();
    while let Some(current) = cursor {
        let page = db.paginate_shoutbox(Some(current), small).await?;
        remaining.extend(page.rows);
        cursor = page.next_cursor;
    }
    assert_eq!(remaining.len(), 3);
    assert!(remaining.iter().all(|row| row.target.1 != later.event_id()));
    assert_eq!(db.paginate_shoutbox(None, bounds()).await?.rows.len(), 5);
    let placeholders = db
        .paginate_shoutbox(
            None,
            ShoutboxBounds {
                payload_bytes: 1,
                ..bounds()
            },
        )
        .await?;
    assert_eq!(placeholders.rows.len(), 5);
    assert_eq!(placeholders.payload_bytes_read, 0);
    assert!(
        placeholders
            .rows
            .iter()
            .all(|row| row.issue == Some(ShoutboxIssue::Oversized))
    );
    assert_eq!(placeholders.stop, ShoutboxStop::End);
    let foreign = Database::new_in_memory(secret.id()).await?;
    assert!(matches!(
        foreign.paginate_shoutbox(Some(original), bounds()).await,
        Err(DbError::ShoutboxCursorStale)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_sparse_budget_ties_and_delayed_materialization() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let remote = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let mut posts = Vec::new();
    for i in 0..270 {
        let post = signed(
            if i % 2 == 0 { secret } else { remote },
            &Shoutbox {
                djot_content: format!("post {i}"),
            },
            i + 1,
            None,
        );
        ingest(&db, &post).await?;
        posts.push(post);
    }
    let delayed = signed(
        remote,
        &Shoutbox {
            djot_content: "delayed".into(),
        },
        0,
        None,
    );
    db.try_process_event(&delayed.event).await?;
    let first = db
        .paginate_shoutbox(
            None,
            ShoutboxBounds {
                rows: 1,
                ..bounds()
            },
        )
        .await?;
    let cursor = first.next_cursor.unwrap();
    assert!(cursor.next_sequence > first.rows[0].received.seq);
    // Header arrival allocated globally but did not add a shoutbox row.
    // Failed materialization must not advance the shared frontier.
    let failed = db
        .write_with(|tx| -> crate::DbResult<()> {
            db.process_event_content_tx(&delayed, Timestamp::from(2), tx)?;
            Err(DbError::ShoutboxInvariant)
        })
        .await;
    assert!(failed.is_err());
    let after_failure = db
        .paginate_shoutbox(
            None,
            ShoutboxBounds {
                rows: 1,
                ..bounds()
            },
        )
        .await?;
    assert_eq!(
        after_failure.next_cursor.unwrap().next_sequence,
        cursor.next_sequence
    );
    db.write_with(|tx| db.process_event_content_tx(&delayed, Timestamp::from(2), tx))
        .await?;
    // Make a sparse span without cloning/filtering the full database in the reader.
    db.write_with(|tx| {
        let mut store = tx.open_table(&crate::content_store::TABLE)?;
        for post in &posts[1..269] {
            store.remove(&post.content_hash())?;
        }
        Ok(())
    })
    .await?;
    let page = db
        .paginate_shoutbox(
            Some(cursor),
            ShoutboxBounds {
                raw_rows: 256,
                ..bounds()
            },
        )
        .await?;
    assert_eq!(page.raw_rows_scanned, 256);
    assert_eq!(page.unavailable_rows, 256);
    assert!(page.rows.is_empty());
    assert_eq!(page.stop, ShoutboxStop::ScanBudget);
    let cursor = serde_json::from_str(&serde_json::to_string(&page.next_cursor.unwrap())?)?;
    let last = db.paginate_shoutbox(Some(cursor), bounds()).await?;
    assert_eq!(last.stop, ShoutboxStop::End);
    assert_eq!(last.raw_rows_scanned, 14); // 12 missing, one original, one excluded allocation.
    assert_eq!(last.rows.len(), 1);
    assert_eq!(last.rows[0].target.1, posts[0].event_id());
    assert!(last.rows[0].received.seq < first.rows[0].received.seq);
    let refreshed = db.paginate_shoutbox(None, bounds()).await?;
    assert!(
        refreshed
            .rows
            .iter()
            .any(|r| r.target.1 == delayed.event_id())
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_admission_refusal_rolls_back_header_receipt_and_allocation() -> anyhow::Result<()>
{
    use std::num::{NonZeroU64, NonZeroUsize};
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let request = ShoutboxRequest {
        body: "hello".into(),
    };
    let content = candidate(secret, 1, &request);
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await?
        .ready
    {}
    db.payload_admission.state.lock().unwrap().config = Some(
        crate::PayloadAdmissionConfig::new(crate::PayloadAdmissionLimits {
            database_bytes: NonZeroU64::new(1).unwrap(),
            author_bytes: NonZeroU64::new(1).unwrap(),
            overrides: Default::default(),
            in_flight_count: NonZeroUsize::new(4).unwrap(),
            in_flight_bytes: NonZeroU64::new(10000).unwrap(),
        })
        .unwrap(),
    );
    let mut allocation = db.reserve_payload_allocation(u64::from(content.content_len()))?;
    let mut buffer = None;
    assert!(matches!(
        db.commit_shoutbox(
            ShoutboxRequestId([1; 16]),
            &request,
            &content,
            &mut allocation,
            &mut buffer
        )
        .await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    assert!(
        db.shoutbox_receipt(ShoutboxRequestId([1; 16]), &request)
            .await?
            .is_none()
    );
    assert!(db.get_event(content.event_id()).await.is_none());
    assert!(db.paginate_shoutbox(None, bounds()).await?.rows.is_empty());
    drop(buffer);
    drop(allocation);
    assert_eq!(db.payload_admission_usage().acquisitions, 0);
    assert_eq!(db.payload_admission_usage().buffers, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_cancellation_after_commit_reconciles_on_reopen() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cancel.redb");
    let db = std::sync::Arc::new(Database::open(&path, secret.id()).await?);
    let request = ShoutboxRequest {
        body: "accepted".into(),
    };
    let content = candidate(secret, 1, &request);
    let id = ShoutboxRequestId([1; 16]);
    let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = tokio::spawn({
        let db = db.clone();
        let request = request.clone();
        let content = content.clone();
        async move {
            db.commit_shoutbox_inner(
                id,
                &request,
                &content,
                &mut None,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(move || {
                        committed_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    });
                    Ok(())
                })),
            )
            .await?;
            std::future::pending::<()>().await;
            Ok::<(), DbError>(())
        }
    });
    committed_rx.await?;
    task.abort();
    release_tx.send(())?;
    assert!(task.await.unwrap_err().is_cancelled());
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        db.commit_shoutbox(id, &request, &content, &mut None, &mut None)
            .await?
            .event_id,
        content.event_id()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_historical_large_and_corrupt_index_are_explicit() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    // Ingestion compatibility is intentionally broader than native presentation.
    // Valid short body plus a large unknown historical extension field.
    let mut bytes = vec![0xa2, 0x61, b'c', 0x61, b'x', 0x61, b'z', 0x7a];
    bytes.extend_from_slice(&1_100_000u32.to_be_bytes());
    bytes.resize(bytes.len() + 1_100_000, b'x');
    let raw = rostra_core::event::EventContentRaw::new(bytes);
    let event = rostra_core::event::Event::builder_raw_content()
        .author(secret.id())
        .kind(rostra_core::event::EventKind::SHOUTBOX)
        .content(&raw)
        .build();
    let large = VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))?,
        raw,
    )?;
    ingest(&db, &large).await?;
    let page = db.paginate_shoutbox(None, bounds()).await?;
    assert_eq!(page.rows[0].issue, Some(ShoutboxIssue::Oversized));
    assert_eq!(page.payload_bytes_read, 0);
    assert_eq!(page.rows[0].encoded_bytes, large.content_len());
    assert!(db.get_event_content(large.event_id()).await.is_some());
    db.write_with(|tx| {
        tx.open_table(&crate::shoutbox_posts_by_received_at::TABLE)?
            .insert(
                &(page.rows[0].received.ts, page.rows[0].received.seq),
                &rostra_core::ShortEventId::from_bytes([8; 16]),
            )?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.paginate_shoutbox(None, bounds()).await,
        Err(DbError::ShoutboxInvariant)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_payload_boundary_unsupported_corruption_and_invalid_bounds() -> anyhow::Result<()>
{
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let a = signed(
        secret,
        &Shoutbox {
            djot_content: "123456".into(),
        },
        1,
        None,
    );
    let b = signed(
        secret,
        &Shoutbox {
            djot_content: "abcdef".into(),
        },
        2,
        None,
    );
    ingest(&db, &a).await?;
    ingest(&db, &b).await?;
    let budget = ShoutboxBounds {
        payload_bytes: b.content_len() as usize,
        ..bounds()
    };
    let page = db.paginate_shoutbox(None, budget).await?;
    assert_eq!(page.stop, ShoutboxStop::PayloadBudget);
    assert_eq!(page.payload_bytes_read, budget.payload_bytes);
    assert_eq!(page.raw_rows_scanned, 1);
    assert_eq!(page.rows[0].target.1, b.event_id());
    let next = db.paginate_shoutbox(page.next_cursor, budget).await?;
    assert_eq!(next.rows[0].target.1, a.event_id());
    assert_eq!(next.stop, ShoutboxStop::End);
    for invalid in [
        ShoutboxBounds {
            rows: 0,
            ..bounds()
        },
        ShoutboxBounds {
            rows: 101,
            ..bounds()
        },
        ShoutboxBounds {
            raw_rows: 0,
            ..bounds()
        },
        ShoutboxBounds {
            raw_rows: 4097,
            ..bounds()
        },
        ShoutboxBounds {
            payload_bytes: 0,
            ..bounds()
        },
        ShoutboxBounds {
            payload_bytes: 16 * 1024 * 1024 + 1,
            ..bounds()
        },
    ] {
        assert!(matches!(
            db.paginate_shoutbox(None, invalid).await,
            Err(DbError::ShoutboxBounds)
        ));
    }
    // Permissive historical serde ignores the unknown scalar field.
    let raw = rostra_core::event::EventContentRaw::new(vec![
        0xa2, 0x61, b'c', 0x61, b'x', 0x61, b'z', 0x01,
    ]);
    let event = rostra_core::event::Event::builder_raw_content()
        .author(secret.id())
        .kind(rostra_core::event::EventKind::SHOUTBOX)
        .content(&raw)
        .build();
    let historical = VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), event.signed_by(secret))?,
        raw,
    )?;
    ingest(&db, &historical).await?;
    let page = db.paginate_shoutbox(None, bounds()).await?;
    assert_eq!(page.rows[0].issue, Some(ShoutboxIssue::Unsupported));
    assert_eq!(page.rows[0].target.1, historical.event_id());
    db.write_with(|tx| {
        let mut store = tx
            .as_raw()
            .open_table(crate::content_store::TABLE.as_raw())?;
        let key = crate::payload_admission::encode_content_hash_key(b.content_hash());
        let bad =
            bincode::encode_to_vec(vec![0u8; 2_000_000], redb_bincode::BINCODE_CONFIG).unwrap();
        store.insert(key.as_slice(), bad.as_slice())?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.paginate_shoutbox(None, bounds()).await,
        Err(DbError::PayloadAccountingInvariant)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn shoutbox_receipt_rollback_ack_loss_prune_delete_upgrade_rebuild() -> anyhow::Result<()> {
    use rostra_core::id::ToShort as _;
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("shoutbox.redb");
    let db = Database::open(&path, secret.id()).await?;
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(crate::shoutbox_request_receipts::TABLE.as_raw())?;
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &38)?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = std::sync::Arc::new(Database::open(&path, secret.id()).await?);
    let request = ShoutboxRequest {
        body: "permanent".into(),
    };
    let content = candidate(secret, 1, &request);
    let id = ShoutboxRequestId([1; 16]);
    assert!(
        db.commit_shoutbox_inner(
            id,
            &request,
            &content,
            &mut None,
            &mut None,
            Some(Box::new(|_| Err(DbError::ShoutboxCandidate)))
        )
        .await
        .is_err()
    );
    assert!(db.get_event(content.event_id()).await.is_none());
    assert!(db.shoutbox_receipt(id, &request).await?.is_none());
    assert!(db.paginate_shoutbox(None, bounds()).await?.rows.is_empty());
    let task = tokio::spawn({
        let db = db.clone();
        let request = request.clone();
        let content = content.clone();
        async move {
            db.commit_shoutbox_inner(
                id,
                &request,
                &content,
                &mut None,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(|| panic!("lost ack"));
                    Ok(())
                })),
            )
            .await
        }
    });
    assert!(task.await.unwrap_err().is_panic());
    assert_eq!(
        db.shoutbox_receipt(id, &request).await?.unwrap().event_id,
        content.event_id()
    );
    // Quota pruning must not erase the permanent receipt or be undone by retry.
    db.write_with(|tx| {
        let before = db.payload_before_tx(tx, &content.event)?;
        Database::prune_event_content_tx(
            content.event_id(),
            content.content_hash(),
            &mut tx.open_table(&crate::events_content_state::TABLE)?,
            &mut tx.open_table(&crate::content_rc::TABLE)?,
            &mut tx.open_table(&crate::events_content_missing::TABLE)?,
            Some((
                content.author(),
                content.content_len(),
                &mut tx.open_table(&crate::ids_data_usage::TABLE)?,
            )),
        )?;
        db.payload_after_tx(tx, before)
    })
    .await?;
    db.commit_shoutbox(id, &request, &content, &mut None, &mut None)
        .await?;
    assert!(db.get_event_content(content.event_id()).await.is_none());
    let deletion = rostra_core::event::Event::builder_raw_content()
        .author(secret.id())
        .kind(rostra_core::event::EventKind::SHOUTBOX)
        .delete(content.event_id().to_short())
        .build();
    db.try_process_event(&VerifiedEvent::verify_signed(
        secret.id(),
        deletion.signed_by(secret),
    )?)
    .await?;
    assert_eq!(
        db.commit_shoutbox(id, &request, &content, &mut None, &mut None)
            .await?
            .event_id,
        content.event_id()
    );
    let second = candidate(secret, 2, &request);
    db.commit_shoutbox(
        ShoutboxRequestId([2; 16]),
        &request,
        &second,
        &mut None,
        &mut None,
    )
    .await?;
    let third = candidate(secret, 3, &request);
    db.commit_shoutbox(
        ShoutboxRequestId([3; 16]),
        &request,
        &third,
        &mut None,
        &mut None,
    )
    .await?;
    let cursor = db
        .paginate_shoutbox(
            None,
            ShoutboxBounds {
                rows: 1,
                ..bounds()
            },
        )
        .await?
        .next_cursor
        .unwrap();
    db.write_with(|tx| Database::prepare_total_migration(tx, 39))
        .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
    assert!(matches!(
        db.paginate_shoutbox(Some(cursor.clone()), bounds()).await,
        Err(DbError::ShoutboxCursorStale)
    ));
    // Preserve permanent rows through an interrupted stash/reopen too.
    db.write_with(|tx| Database::prepare_total_migration(tx, 39))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert!(matches!(
        db.paginate_shoutbox(Some(cursor), bounds()).await,
        Err(DbError::ShoutboxCursorStale)
    ));
    assert_eq!(
        db.commit_shoutbox(id, &request, &content, &mut None, &mut None)
            .await?
            .event_id,
        content.event_id()
    );
    let table = crate::ExtensionTableDefinition::<u64, u64>::new("shoutbox_request_receipts");
    assert!(
        db.extension_write(|tx| {
            tx.open_table(&table)?;
            Ok(())
        })
        .await
        .is_err()
    );
    Ok(())
}
