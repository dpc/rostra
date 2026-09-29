use rostra_core::ShortEventId;
use rostra_core::id::RostraIdSecretKey;
use rostra_dm::{Announcement, DeviceState};

use crate::Database;
use crate::dm::HistoryEntry;

fn dm_event(secret: RostraIdSecretKey, bytes: Vec<u8>) -> rostra_core::event::VerifiedEventContent {
    use rostra_core::event::{
        Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent,
    };
    let content = EventContentRaw::new(bytes);
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::DIRECT_MESSAGE)
        .timestamp(time::OffsetDateTime::from_unix_timestamp(1000).unwrap())
        .content(&content)
        .build()
        .signed_by(secret);
    VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), signed).unwrap(),
        content,
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_conversation_and_device_views_page_without_history_scans() -> anyhow::Result<()> {
    let own = RostraIdSecretKey::generate().id();
    let db = Database::new_in_memory(own).await?;
    let mut peers = Vec::new();
    for index in 0u8..70 {
        let peer = RostraIdSecretKey::generate().id();
        peers.push(peer);
        let body = rostra_dm::MessageBody::new(own, peer, format!("conversation {index}"))?;
        db.write_with(|tx| {
            Database::dm_store_history_tx(tx, &body, ShortEventId::random(), 100)?;
            db.dm_apply_announcement_tx(
                own,
                &Announcement {
                    device_id: [index; 16],
                    epoch: None,
                },
                100,
                ShortEventId::random(),
                tx,
            )
        })
        .await?;
    }
    // Hundreds of rows in one conversation must not consume page capacity.
    for index in 0u16..300 {
        let body = rostra_dm::MessageBody::new(peers[0], own, format!("later {index}"))?;
        db.write_with(|tx| {
            Database::dm_store_history_tx(tx, &body, ShortEventId::ZERO, 1000 + u64::from(index))
        })
        .await?;
    }
    assert!(db.dm_conversations(None, 0).await?.is_empty());
    let first = db.dm_conversations(None, usize::MAX).await?;
    assert_eq!(first.len(), 64);
    let last = first.last().unwrap();
    let second = db
        .dm_conversations(
            Some((
                last.sender.min(last.recipient),
                last.sender.max(last.recipient),
            )),
            64,
        )
        .await?;
    assert_eq!(second.len(), 6);
    let mut found = first
        .iter()
        .chain(&second)
        .map(|entry| {
            if entry.sender == own {
                entry.recipient
            } else {
                entry.sender
            }
        })
        .collect::<Vec<_>>();
    found.sort();
    peers.sort();
    assert_eq!(found, peers);
    assert!(
        first
            .iter()
            .chain(&second)
            .any(|entry| entry.text == "later 299")
    );
    let devices = db.dm_own_devices(None, usize::MAX).await?;
    assert_eq!(devices.len(), 64);
    assert!(devices.iter().all(|(_, state)| state.retired()));
    let rest = db
        .dm_own_devices(Some(devices.last().unwrap().0), 64)
        .await?;
    assert_eq!(rest.len(), 6);
    assert!(db.dm_local_installation().await?.is_none());
    let local = db.dm_maintain_local_now().await?;
    let view = db.dm_local_installation().await?.unwrap();
    assert_eq!(view.device_id, local.device_id);
    assert!(!view.retired);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_send_rejects_learned_retirement_and_supersession_atomically() -> anyhow::Result<()> {
    use rostra_core::id::ToShort as _;
    let sender = RostraIdSecretKey::generate();
    let recipient = RostraIdSecretKey::generate().id();
    let now = rostra_core::Timestamp::now().as_u64();
    for retired in [false, true] {
        let db = Database::new_in_memory(sender.id()).await?;
        let epoch = rostra_dm::LocalEpoch::generate(now)?;
        let original = Announcement {
            device_id: [1; 16],
            epoch: Some(epoch.public().clone()),
        };
        db.write_with(|tx| {
            db.dm_apply_announcement_tx(recipient, &original, now, ShortEventId::ZERO, tx)
        })
        .await?;
        let permit = db.dm_destinations_now(recipient).await?;
        let body = rostra_dm::MessageBody::new(sender.id(), recipient, "race".to_owned())?;
        let event = dm_event(sender, rostra_dm::encrypt(&body, permit.keys())?);
        let replacement = Announcement {
            device_id: original.device_id,
            epoch: if retired {
                None
            } else {
                Some(rostra_dm::LocalEpoch::generate(now)?.public().clone())
            },
        };
        db.write_with(|tx| {
            db.dm_apply_announcement_tx(recipient, &replacement, now, ShortEventId::MAX, tx)
        })
        .await?;
        assert!(matches!(
            db.dm_commit_outgoing(&event, &body, &permit, None).await,
            Err(crate::DbError::DmRecipientUnavailable),
        ));
        assert!(!db.has_event(event.event_id().to_short()).await);
        assert!(db.dm_history_with(recipient, None, 64).await?.is_empty());
        assert!(db.get_self_current_head().await.is_none());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_local_retirement_rejects_existing_and_new_send_permits() -> anyhow::Result<()> {
    use rostra_core::id::ToShort as _;
    let sender = RostraIdSecretKey::generate();
    let recipient = RostraIdSecretKey::generate().id();
    let db = Database::new_in_memory(sender.id()).await?;
    let local = db.dm_maintain_local_now().await?;
    let now = rostra_core::Timestamp::now().as_u64();
    db.write_with(|tx| {
        db.dm_apply_announcement_tx(
            recipient,
            &Announcement {
                device_id: [1; 16],
                epoch: Some(
                    rostra_dm::LocalEpoch::generate(now)
                        .unwrap()
                        .public()
                        .clone(),
                ),
            },
            now,
            ShortEventId::ZERO,
            tx,
        )
    })
    .await?;
    let permit = db.dm_destinations_now(recipient).await?;
    let body = rostra_dm::MessageBody::new(sender.id(), recipient, "retirement race".to_owned())?;
    let event = dm_event(sender, rostra_dm::encrypt(&body, permit.keys())?);
    db.write_with(|tx| {
        db.dm_apply_announcement_tx(
            sender.id(),
            &Announcement {
                device_id: local.device_id,
                epoch: None,
            },
            now,
            ShortEventId::MAX,
            tx,
        )
    })
    .await?;
    assert!(matches!(
        db.dm_commit_outgoing(&event, &body, &permit, None).await,
        Err(crate::DbError::DmRecipientUnavailable),
    ));
    assert!(matches!(
        db.dm_destinations_now(recipient).await,
        Err(crate::DbError::DmRecipientUnavailable),
    ));
    assert!(!db.has_event(event.event_id().to_short()).await);
    assert!(db.dm_history_with(recipient, None, 64).await?.is_empty());
    assert!(db.get_self_current_head().await.is_none());
    // Explicit reenrollment, not an unchanged device ID, restores sending.
    assert_ne!(db.dm_reenroll().await?, local.device_id);
    assert!(db.dm_destinations_now(recipient).await.is_ok());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_history_index_pages_and_rebuilds_without_ciphertext() -> anyhow::Result<()> {
    let owner = RostraIdSecretKey::generate().id();
    let peer = RostraIdSecretKey::generate().id();
    let unrelated = RostraIdSecretKey::generate().id();
    let db = Database::new_in_memory(owner).await?;
    db.write_with(|tx| {
        for n in 0u8..100 {
            for recipient in [peer, unrelated] {
                let body =
                    rostra_dm::MessageBody::new(owner, recipient, format!("message {n}")).unwrap();
                Database::dm_store_history_tx(
                    tx,
                    &body,
                    ShortEventId::from_bytes([n; 16]),
                    u64::from(n / 2),
                )?;
            }
        }
        Ok(())
    })
    .await?;
    for replay in [false, true] {
        if replay {
            db.write_with(|tx| Database::prepare_total_migration(tx, 32))
                .await?;
            db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
        }
        let first = db.dm_history_with(peer, None, 64).await?;
        assert_eq!(first.len(), 64);
        let last = first.last().unwrap();
        let second = db
            .dm_history_with(peer, Some((last.timestamp, last.event_id)), 64)
            .await?;
        assert_eq!(second.len(), 36);
        let entries = first.into_iter().chain(second).collect::<Vec<_>>();
        for (entry, n) in entries.iter().zip((0..100).rev()) {
            assert_eq!(entry.text, format!("message {n}"));
            assert_eq!(entry.recipient, peer);
        }
        assert!(
            db.dm_history_with(peer, Some((0, ShortEventId::ZERO)), 64)
                .await?
                .is_empty()
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_unread_is_exact_session_scoped_and_survives_rebuild() -> anyhow::Result<()> {
    let owner = RostraIdSecretKey::generate().id();
    let first_peer = RostraIdSecretKey::generate().id();
    let second_peer = RostraIdSecretKey::generate().id();
    let db = Database::new_in_memory(owner).await?;
    db.write_with(|tx| {
        for (sender, timestamp, text) in [
            (first_peer, 9000, "late signed time"),
            (second_peer, 1, "arrived second"),
            (first_peer, 2, "arrived third"),
        ] {
            let body = rostra_dm::MessageBody::new(sender, owner, text.to_owned()).unwrap();
            Database::dm_store_history_tx(tx, &body, ShortEventId::random(), timestamp)?;
        }
        let outgoing =
            rostra_dm::MessageBody::new(owner, first_peer, "outgoing".to_owned()).unwrap();
        Database::dm_store_history_tx(tx, &outgoing, ShortEventId::random(), 0)
    })
    .await?;

    let session = [7; 16];
    assert_eq!(db.dm_count_unread(session, None, 99).await?, 3);
    assert_eq!(db.dm_count_unread(session, Some(first_peer), 99).await?, 2);
    db.dm_mark_read(session, &[1, 3]).await?;
    assert_eq!(db.dm_count_unread(session, None, 99).await?, 1);
    db.write_with(|tx| Database::prepare_total_migration(tx, 33))
        .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
    assert_eq!(db.dm_count_unread(session, None, 99).await?, 1);
    assert_eq!(db.dm_count_unread(session, Some(first_peer), 99).await?, 0);
    assert_eq!(db.dm_count_unread([8; 16], None, 99).await?, 3);
    db.dm_mark_read(session, &[1, 3]).await?;
    assert_eq!(db.dm_count_unread(session, None, 99).await?, 1);
    let history = db.dm_history_with_sequences(first_peer, None, 10).await?;
    assert_eq!(
        history
            .iter()
            .filter_map(|entry| entry.incoming_sequence)
            .count(),
        2
    );
    assert!(
        history
            .iter()
            .any(|entry| entry.entry.sender == owner && entry.incoming_sequence.is_none())
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_read_markers_merge_concurrently_beyond_sparse_range_limits() -> anyhow::Result<()> {
    let owner = RostraIdSecretKey::generate().id();
    let peer = RostraIdSecretKey::generate().id();
    let db = Database::new_in_memory(owner).await?;
    db.write_with(|tx| {
        for n in 0..600 {
            let body = rostra_dm::MessageBody::new(peer, owner, format!("incoming {n}")).unwrap();
            Database::dm_store_history_tx(tx, &body, ShortEventId::random(), n)?;
        }
        Ok(())
    })
    .await?;
    let session = [9; 16];
    let odd = (1..=600).step_by(2).collect::<Vec<_>>();
    let even = (2..=600).step_by(2).collect::<Vec<_>>();
    let (odd_result, even_result) = tokio::join!(
        async {
            for chunk in odd.chunks(64) {
                db.dm_mark_read(session, chunk).await?;
            }
            anyhow::Ok(())
        },
        async {
            for chunk in even.chunks(64) {
                db.dm_mark_read(session, chunk).await?;
            }
            anyhow::Ok(())
        }
    );
    odd_result?;
    even_result?;
    assert_eq!(db.dm_count_unread(session, None, 999).await?, 0);
    assert_eq!(db.dm_count_unread(session, Some(peer), 999).await?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_live_pages_follow_arrivals_not_timestamps_or_read_markers() -> anyhow::Result<()> {
    let owner = RostraIdSecretKey::generate().id();
    let peer = RostraIdSecretKey::generate().id();
    let other = RostraIdSecretKey::generate().id();
    let db = Database::new_in_memory(owner).await?;
    assert_eq!(db.dm_latest_incoming_sequence(peer).await?, 0);
    db.write_with(|tx| {
        for n in 0..205 {
            let sender = if n == 1 { other } else { peer };
            let body = rostra_dm::MessageBody::new(sender, owner, format!("arrival {n}")).unwrap();
            Database::dm_store_history_tx(tx, &body, ShortEventId::random(), 1000 - n)?;
        }
        let outgoing = rostra_dm::MessageBody::new(owner, peer, "outgoing".to_owned()).unwrap();
        Database::dm_store_history_tx(tx, &outgoing, ShortEventId::random(), 2000)?;
        Ok(())
    })
    .await?;
    assert_eq!(db.dm_latest_incoming_sequence(peer).await?, 205);
    assert_eq!(db.dm_latest_incoming_sequence(other).await?, 2);
    let mut cursor = 0;
    let mut texts = Vec::new();
    for expected in [100, 100, 4, 0] {
        let entries = db.dm_incoming_after(peer, cursor, usize::MAX).await?;
        assert_eq!(entries.len(), expected);
        let sequences = entries
            .iter()
            .map(|entry| entry.incoming_sequence.unwrap())
            .collect::<Vec<_>>();
        db.dm_mark_read([1; 16], &sequences).await?;
        // Another tab's read markers must not suppress this cursor's page.
        assert_eq!(
            db.dm_incoming_after(peer, cursor, usize::MAX).await?.len(),
            expected
        );
        if let Some(sequence) = sequences.last() {
            cursor = *sequence;
        }
        texts.extend(entries.into_iter().map(|entry| entry.entry.text));
    }
    assert_eq!(texts.len(), 204);
    assert_eq!(texts.first().unwrap(), "arrival 0");
    assert_eq!(texts.last().unwrap(), "arrival 204");
    assert!(db.dm_incoming_after(peer, u64::MAX, 100).await?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_pending_notification_follows_commit_and_retains_early_wakeup() -> anyhow::Result<()> {
    let recipient = RostraIdSecretKey::generate().id();
    let sender = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(recipient).await?;
    let notify = db.dm_pending_notify();
    let epoch = rostra_dm::LocalEpoch::generate(1000)?;
    let body = rostra_dm::MessageBody::new(sender.id(), recipient, "notify".to_owned())?;
    let event = dm_event(
        sender,
        rostra_dm::encrypt(
            &body,
            &[rostra_dm::PublicKey::from_bytes(epoch.public().public_key)?],
        )?,
    );
    db.try_process_event_with_content(&event).await?;
    // Insertion before registration is retained by Notify's one permit.
    tokio::time::timeout(std::time::Duration::from_secs(1), notify.notified()).await?;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), notify.notified(),)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_receive_resumes_all_live_keys_and_retains_history_after_erasure() -> anyhow::Result<()>
{
    let recipient = RostraIdSecretKey::generate().id();
    let sender = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(recipient).await?;
    let mut epochs = (0..12)
        .map(|_| rostra_dm::LocalEpoch::generate(1000).unwrap())
        .collect::<Vec<_>>();
    epochs.sort_by_key(|epoch| epoch.public().public_key);
    let target = epochs.last().unwrap().public().clone();
    db.write_with(|tx| {
        let mut table = tx.open_table(&crate::ids_dm_epochs::TABLE)?;
        for epoch in &epochs {
            table.insert(&([9; 16], epoch.public().public_key), epoch)?;
        }
        Ok(())
    })
    .await?;
    let body = rostra_dm::MessageBody::new(sender.id(), recipient, "late batch".to_owned())?;
    let frame = rostra_dm::encrypt(
        &body,
        &[rostra_dm::PublicKey::from_bytes(target.public_key)?],
    )?;
    let event = dm_event(sender, frame);
    db.try_process_event_with_content(&event).await?;
    let mut progress = db.dm_progress_subscribe();
    assert!(db.dm_process_pending(1000).await?);
    assert!(progress.has_changed()?);
    progress.borrow_and_update();
    db.read_with(|tx| {
        assert!(
            tx.open_table(&crate::events_dm_history::TABLE)?
                .get(&(sender.id(), body.message_id()))?
                .is_none()
        );
        assert!(
            tx.open_table(&crate::events_dm_pending::TABLE)?
                .first()?
                .is_some()
        );
        Ok(())
    })
    .await?;
    assert!(db.dm_process_pending(1000).await?);
    assert!(
        progress.has_changed()?,
        "success and misses both notify progress"
    );
    progress.borrow_and_update();
    assert!(!db.dm_process_pending(1000).await?);
    assert!(
        !progress.has_changed()?,
        "idle work must not cause a refresh loop"
    );
    db.dm_process_pending(target.decrypt_until).await?;
    db.write_with(|tx| Database::prepare_total_migration(tx, 32))
        .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
    db.read_with(|tx| {
        assert!(
            tx.open_table(&crate::ids_dm_epochs::TABLE)?
                .first()?
                .is_none()
        );
        let entry = tx
            .open_table(&crate::events_dm_history::TABLE)?
            .get(&(sender.id(), body.message_id()))?
            .unwrap()
            .value_try()?;
        assert_eq!(entry.text, "late batch");
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_expired_key_cannot_open_queued_ciphertext() -> anyhow::Result<()> {
    let recipient = RostraIdSecretKey::generate().id();
    let sender = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(recipient).await?;
    let announcement = db.dm_maintain_local(1000).await?;
    let target = announcement.epoch.unwrap();
    let body = rostra_dm::MessageBody::new(sender.id(), recipient, "too late".to_owned())?;
    let event = dm_event(
        sender,
        rostra_dm::encrypt(
            &body,
            &[rostra_dm::PublicKey::from_bytes(target.public_key)?],
        )?,
    );
    db.try_process_event_with_content(&event).await?;
    assert!(db.dm_process_pending(target.decrypt_until).await?);
    db.read_with(|tx| {
        assert!(
            tx.open_table(&crate::ids_dm_epochs::TABLE)?
                .first()?
                .is_none()
        );
        assert!(
            tx.open_table(&crate::events_dm_history::TABLE)?
                .first()?
                .is_none()
        );
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_clock_advance_between_purge_and_trial_requires_another_purge() -> anyhow::Result<()> {
    let recipient = RostraIdSecretKey::generate().id();
    let sender = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(recipient).await?;
    let target = db.dm_maintain_local(1000).await?.epoch.unwrap();
    let body = rostra_dm::MessageBody::new(
        sender.id(),
        recipient,
        "expired during lock wait".to_owned(),
    )?;
    let event = dm_event(
        sender,
        rostra_dm::encrypt(
            &body,
            &[rostra_dm::PublicKey::from_bytes(target.public_key)?],
        )?,
    );
    db.try_process_event_with_content(&event).await?;
    let calls = std::cell::Cell::new(0);
    assert!(
        db.dm_process_pending_with_clock(|| {
            let call = calls.get();
            calls.set(call + 1);
            if call == 0 {
                1000
            } else {
                target.decrypt_until
            }
        })
        .await?
    );
    assert!(db.dm_history_with(sender.id(), None, 64).await?.is_empty());
    db.read_with(|tx| {
        assert!(
            tx.open_table(&crate::events_dm_pending::TABLE)?
                .first()?
                .is_some()
        );
        Ok(())
    })
    .await?;
    assert!(db.dm_process_pending(target.decrypt_until).await?);
    db.read_with(|tx| {
        assert!(
            tx.open_table(&crate::ids_dm_epochs::TABLE)?
                .first()?
                .is_none()
        );
        assert!(
            tx.open_table(&crate::events_dm_history::TABLE)?
                .first()?
                .is_none()
        );
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_lifecycle_assigns_once_rotates_and_purges_before_return() -> anyhow::Result<()> {
    let db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let first = db.dm_maintain_local(1000).await?;
    let public = first.epoch.as_ref().unwrap();
    assert!(db.dm_maintain_local(999).await? == first);
    assert!(db.dm_maintain_local(public.send_until - 1).await? == first);
    let next = db.dm_maintain_local(public.send_until).await?;
    assert_eq!(first.device_id, next.device_id);
    assert_ne!(public.public_key, next.epoch.as_ref().unwrap().public_key);
    assert!(db.dm_maintain_local(500).await? == next);
    db.dm_maintain_local(public.decrypt_until).await?;
    db.read_with(|tx| {
        assert!(
            tx.open_table(&crate::ids_dm_epochs::TABLE)?
                .get(&(first.device_id, public.public_key))?
                .is_none()
        );
        Ok(())
    })
    .await?;
    db.dm_maintain_local(500).await?;
    db.read_with(|tx| {
        assert!(
            tx.open_table(&crate::ids_dm_epochs::TABLE)?
                .get(&(first.device_id, public.public_key))?
                .is_none()
        );
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_total_replay_preserves_non_replayable_state() -> anyhow::Result<()> {
    let author = RostraIdSecretKey::generate().id();
    let db = Database::new_in_memory(author).await?;
    let announcement = db.dm_maintain_local(1000).await?;
    let mut retired = DeviceState::default();
    retired.apply(
        &Announcement {
            device_id: [3; 16],
            epoch: None,
        },
        1,
        ShortEventId::ZERO,
    );
    let message_id = [4; 16];
    db.write_with(|tx| {
        tx.open_table(&crate::ids_dm_devices::TABLE)?
            .insert(&(author, [3; 16]), &retired)?;
        tx.open_table(&crate::events_dm_history::TABLE)?.insert(
            &(author, message_id),
            &HistoryEntry {
                sender: author,
                recipient: author,
                message_id,
                text: "retained without any ciphertext".to_owned(),
                event_id: ShortEventId::ZERO,
                timestamp: 12,
                conflicted: true,
            },
        )?;
        Ok(())
    })
    .await?;
    db.write_with(|tx| Database::prepare_total_migration(tx, 32))
        .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
    assert!(db.dm_maintain_local(1000).await? == announcement);
    db.read_with(|tx| {
        assert!(
            tx.open_table(&crate::ids_dm_devices::TABLE)?
                .get(&(author, [3; 16]))?
                .unwrap()
                .value_try()?
                .retired()
        );
        let history = tx
            .open_table(&crate::events_dm_history::TABLE)?
            .get(&(author, message_id))?
            .unwrap()
            .value_try()?;
        assert_eq!(history.text, "retained without any ciphertext");
        assert!(history.conflicted);
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_missing_authoritative_stash_fails_without_replacing_state() -> anyhow::Result<()> {
    let db = Database::new_in_memory(RostraIdSecretKey::generate().id()).await?;
    let announcement = db.dm_maintain_local(1000).await?;
    db.write_with(|tx| Database::prepare_total_migration(tx, 32))
        .await?;
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(redb::TableDefinition::<&[u8], &[u8]>::new(
                "_total_migration_ids_dm_epochs",
            ))?;
        Ok(())
    })
    .await?;
    assert!(
        db.write_with(|tx| db.reprocess_migration_stash(tx))
            .await
            .is_err()
    );
    assert!(db.write_with(Database::has_pending_migration_stash).await?);
    db.read_with(|tx| {
        let installation = tx
            .open_table(&crate::ids_dm_installation::TABLE)?
            .get(&())?
            .unwrap()
            .value_try()?;
        assert_eq!(installation.device_id, announcement.device_id);
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_retirement_reenroll_and_reopen_preserve_original_deadlines() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("dm.redb");
    let id = RostraIdSecretKey::generate().id();
    let db = Database::open(&path, id).await?;
    let first = db.dm_maintain_local(1000).await?;
    db.write_with(|tx| {
        db.dm_apply_announcement_tx(
            id,
            &Announcement {
                device_id: first.device_id,
                epoch: None,
            },
            1,
            ShortEventId::ZERO,
            tx,
        )
    })
    .await?;
    assert!(db.dm_maintain_local(1001).await?.epoch.is_none());
    db.write_with(|tx| Database::prepare_total_migration(tx, 32))
        .await?;
    // Reopen the committed authoritative stash through the real startup path.
    drop(db);
    let db = Database::open(&path, id).await?;
    assert!(db.dm_maintain_local(1002).await?.epoch.is_none());
    let replacement = db.dm_reenroll().await?;
    assert_ne!(replacement, first.device_id);
    let second = db.dm_maintain_local(1003).await?;
    assert_eq!(second.device_id, replacement);
    assert_ne!(
        second.epoch.as_ref().unwrap().public_key,
        first.epoch.as_ref().unwrap().public_key
    );
    db.read_with(|tx| {
        let old = tx
            .open_table(&crate::ids_dm_epochs::TABLE)?
            .get(&(first.device_id, first.epoch.as_ref().unwrap().public_key))?
            .unwrap()
            .value_try()?;
        assert!(old.public() == first.epoch.as_ref().unwrap());
        Ok(())
    })
    .await?;
    let expiry = first.epoch.as_ref().unwrap().decrypt_until;
    drop(db);
    let db = Database::open(&path, id).await?;
    db.dm_process_pending(expiry).await?;
    db.read_with(|tx| {
        assert!(
            tx.open_table(&crate::ids_dm_epochs::TABLE)?
                .get(&(first.device_id, first.epoch.as_ref().unwrap().public_key))?
                .is_none()
        );
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_destinations_apply_four_four_spillover_and_never_fall_back() -> anyhow::Result<()> {
    let sender = RostraIdSecretKey::generate().id();
    let recipient = RostraIdSecretKey::generate().id();
    for (recipients, senders, expected_recipient, expected_sender) in
        [(7, 7, 4, 4), (6, 2, 6, 2), (2, 6, 2, 6), (1, 1, 1, 1)]
    {
        let db = Database::new_in_memory(sender).await?;
        let local = db.dm_maintain_local(1000).await?;
        let mut recipient_keys = Vec::new();
        let mut sender_keys = Vec::new();
        db.write_with(|tx| {
            db.dm_apply_announcement_tx(sender, &local, 1000, ShortEventId::ZERO, tx)?;
            for (account, count, keys) in [
                (recipient, recipients, &mut recipient_keys),
                (sender, senders, &mut sender_keys),
            ] {
                for n in 0..count {
                    let epoch = rostra_dm::LocalEpoch::generate(1000).unwrap();
                    keys.push(epoch.public().public_key);
                    db.dm_apply_announcement_tx(
                        account,
                        &Announcement {
                            device_id: [n + 1; 16],
                            epoch: Some(epoch.public().clone()),
                        },
                        1000 + u64::from(n),
                        ShortEventId::ZERO,
                        tx,
                    )?;
                }
            }
            Ok(())
        })
        .await?;
        let selected = db.dm_destinations(recipient, 1000).await?;
        assert_eq!(
            selected
                .keys()
                .iter()
                .filter(|key| recipient_keys.contains(&key.to_bytes()))
                .count(),
            expected_recipient
        );
        assert_eq!(
            selected
                .keys()
                .iter()
                .filter(|key| sender_keys.contains(&key.to_bytes()))
                .count(),
            expected_sender
        );
        assert!(
            !selected
                .keys()
                .iter()
                .any(|key| key.to_bytes() == local.epoch.as_ref().unwrap().public_key)
        );
        assert!(
            db.dm_destinations(recipient, local.epoch.as_ref().unwrap().send_until)
                .await
                .is_err()
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_conflicting_body_preserves_original_history_and_replay_winner() -> anyhow::Result<()> {
    let sender = RostraIdSecretKey::generate();
    let recipient = RostraIdSecretKey::generate().id();
    let db = Database::new_in_memory(recipient).await?;
    let local = db.dm_maintain_local(1000).await?;
    let body = rostra_dm::MessageBody::new(sender.id(), recipient, "conflicting".to_owned())?;
    db.write_with(|tx| {
        tx.open_table(&crate::events_dm_history::TABLE)?.insert(
            &(sender.id(), body.message_id()),
            &HistoryEntry {
                sender: sender.id(),
                recipient,
                message_id: body.message_id(),
                text: "original".to_owned(),
                event_id: ShortEventId::ZERO,
                timestamp: 1,
                conflicted: false,
            },
        )?;
        Ok(())
    })
    .await?;
    let event = dm_event(
        sender,
        rostra_dm::encrypt(
            &body,
            &[rostra_dm::PublicKey::from_bytes(
                local.epoch.as_ref().unwrap().public_key,
            )?],
        )?,
    );
    db.try_process_event_with_content(&event).await?;
    db.dm_process_pending(1000).await?;
    db.write_with(|tx| Database::prepare_total_migration(tx, 32))
        .await?;
    db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
    db.dm_process_pending(1000).await?;
    let history = db.dm_history_with(sender.id(), None, 64).await?;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "original");
    assert_eq!(history[0].event_id, ShortEventId::ZERO);
    assert!(history[0].conflicted);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_outgoing_admission_rollback_and_exact_retry_are_atomic() -> anyhow::Result<()> {
    use std::num::{NonZeroU64, NonZeroUsize};

    use rostra_core::id::ToShort as _;
    let sender = RostraIdSecretKey::generate();
    let recipient = RostraIdSecretKey::generate().id();
    let db = Database::new_in_memory(sender.id()).await?;
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await?
        .ready
    {}
    let configure = |bytes| {
        crate::PayloadAdmissionConfig::new(crate::PayloadAdmissionLimits {
            database_bytes: NonZeroU64::new(bytes).unwrap(),
            author_bytes: NonZeroU64::new(bytes).unwrap(),
            overrides: Default::default(),
            in_flight_count: NonZeroUsize::new(8).unwrap(),
            in_flight_bytes: NonZeroU64::new(100_000).unwrap(),
        })
        .unwrap()
    };
    // Isolated admission primitive fixture; production account configuration is
    // immutable startup input, not a live setting.
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = Some(configure(100));
        Ok(())
    })
    .await?;
    let epoch = rostra_dm::LocalEpoch::generate(1000)?;
    let body = rostra_dm::MessageBody::new(sender.id(), recipient, "atomic".to_owned())?;
    let permit = crate::dm::SendPermit {
        sender: sender.id(),
        recipient,
        keys: vec![],
        send_until: u64::MAX,
        send_from: 0,
        selected: vec![],
        sending_device: None,
    };
    let event = dm_event(
        sender,
        rostra_dm::encrypt(
            &body,
            &[rostra_dm::PublicKey::from_bytes(epoch.public().public_key)?],
        )?,
    );
    assert!(matches!(
        db.dm_commit_outgoing(&event, &body, &permit, None).await,
        Err(crate::DbError::PayloadAdmissionPaused { .. })
    ));
    assert!(db.get_event(event.event_id().to_short()).await.is_none());
    assert!(db.dm_history_with(recipient, None, 64).await?.is_empty());
    assert!(db.get_self_current_head().await.is_none());
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = Some(configure(100_000));
        Ok(())
    })
    .await?;
    db.dm_commit_outgoing(&event, &body, &permit, None).await?;
    db.dm_commit_outgoing(&event, &body, &permit, None).await?;
    assert!(db.get_event(event.event_id().to_short()).await.is_some());
    let history = db.dm_history_with(recipient, None, 64).await?;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].event_id, event.event_id().to_short());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_published_head_already_has_history_even_if_publisher_is_cancelled() -> anyhow::Result<()>
{
    let sender = RostraIdSecretKey::generate();
    let recipient = RostraIdSecretKey::generate().id();
    let db = std::sync::Arc::new(Database::new_in_memory(sender.id()).await?);
    let epoch = rostra_dm::LocalEpoch::generate(1000)?;
    let body = rostra_dm::MessageBody::new(sender.id(), recipient, "committed".to_owned())?;
    let permit = crate::dm::SendPermit {
        sender: sender.id(),
        recipient,
        keys: vec![],
        send_until: u64::MAX,
        send_from: 0,
        selected: vec![],
        sending_device: None,
    };
    let event = dm_event(
        sender,
        rostra_dm::encrypt(
            &body,
            &[rostra_dm::PublicKey::from_bytes(epoch.public().public_key)?],
        )?,
    );
    let mut heads = db.self_head_subscribe();
    let publisher_db = db.clone();
    let publisher = tokio::spawn(async move {
        publisher_db
            .dm_commit_outgoing(&event, &body, &permit, None)
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), heads.changed()).await??;
    publisher.abort();
    let _ = publisher.await;
    let history = db.dm_history_with(recipient, None, 64).await?;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "committed");
    Ok(())
}
