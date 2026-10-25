use std::sync::Arc;

use rostra_core::ShortEventId;
use rostra_core::event::{Event, EventContentRaw, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};
use rostra_dm::Announcement;

use crate::dm_device_request::{
    DmDeviceOutcome, DmDeviceRequest, DmDeviceRequestId, DmEnrollmentExpectation,
};
use crate::{Database, DbError, DbResult};

type RawTableSnapshot = Vec<(Vec<u8>, Vec<u8>)>;

fn enroll(expected: DmEnrollmentExpectation) -> DmDeviceRequest {
    DmDeviceRequest::Reenroll { expected }
}

async fn commit(
    db: &Database,
    secret: RostraIdSecretKey,
    id: u8,
    request: DmDeviceRequest,
) -> DbResult<DmDeviceOutcome> {
    let mut allocation = db
        .reserve_payload_allocation(148)
        .map_err(|reason| DbError::PayloadAdmissionPaused { reason })?;
    let mut buffer = None;
    db.commit_dm_device_request(
        secret,
        DmDeviceRequestId([id; 16]),
        &request,
        &mut allocation,
        &mut buffer,
    )
    .await
}

async fn announce(
    db: &Database,
    secret: RostraIdSecretKey,
    announcement: &Announcement,
) -> anyhow::Result<()> {
    let raw = EventContentRaw::new(announcement.encode()?);
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::DM_DEVICE)
        .content(&raw)
        .build()
        .signed_by(secret);
    db.try_process_event_with_content(&VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), signed)?,
        raw,
    )?)
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_device_requests_bind_exact_target_and_incarnation() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let foreign = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let first = commit(&db, secret, 1, enroll(DmEnrollmentExpectation::Absent)).await?;
    let active = db.dm_maintain_local_now().await?;
    let foreign_device = [92; 16];
    announce(
        &db,
        foreign,
        &Announcement {
            device_id: foreign_device,
            epoch: active.epoch.clone(),
        },
    )
    .await?;
    assert!(matches!(
        commit(
            &db,
            secret,
            2,
            DmDeviceRequest::Retire {
                device_id: foreign_device
            }
        )
        .await,
        Err(DbError::DmDeviceStateConflict)
    ));
    assert!(matches!(
        commit(&db, foreign, 1, enroll(DmEnrollmentExpectation::Absent)).await,
        Err(DbError::DmDeviceCandidate)
    ));
    assert!(
        db.dm_device_receipt(
            foreign,
            DmDeviceRequestId([1; 16]),
            &enroll(DmEnrollmentExpectation::Absent)
        )
        .await
        .is_err()
    );
    let other = [93; 16];
    announce(
        &db,
        secret,
        &Announcement {
            device_id: other,
            epoch: active.epoch.clone(),
        },
    )
    .await?;
    let retire = DmDeviceRequest::Retire {
        device_id: first.device_id,
    };
    let retired = commit(&db, secret, 2, retire).await?;
    assert!(db.dm_local_installation().await?.unwrap().retired);
    assert!(matches!(
        commit(&db, secret, 2, DmDeviceRequest::Retire { device_id: other }).await,
        Err(DbError::DmDeviceRequestMismatch)
    ));
    assert!(matches!(
        commit(&db, secret, 3, retire).await,
        Err(DbError::DmDeviceStateConflict)
    ));
    // Delayed maintenance cannot undo the exact permanent tombstone.
    announce(&db, secret, &active).await?;
    let own = db.dm_own_devices(None, 64).await?;
    assert!(
        own.iter()
            .find(|(id, _)| *id == first.device_id)
            .unwrap()
            .1
            .retired()
    );
    assert!(!own.iter().find(|(id, _)| *id == other).unwrap().1.retired());
    assert!(own.iter().all(|(id, _)| *id != foreign_device));
    let reenroll = enroll(DmEnrollmentExpectation::Retired {
        device_id: first.device_id,
    });
    let second = commit(&db, secret, 3, reenroll).await?;
    assert_ne!(second.device_id, first.device_id);
    commit(
        &db,
        secret,
        4,
        DmDeviceRequest::Retire {
            device_id: second.device_id,
        },
    )
    .await?;
    // Both original outcomes survive a later retirement. Neither re-applies.
    assert_eq!(
        commit(&db, secret, 3, reenroll).await?.event.event_id,
        second.event.event_id
    );
    assert_eq!(
        commit(&db, secret, 2, retire).await?.event.event_id,
        retired.event.event_id
    );
    assert_eq!(
        commit(&db, secret, 1, enroll(DmEnrollmentExpectation::Absent))
            .await?
            .device_id,
        first.device_id
    );
    let local = db.dm_local_installation().await?.unwrap();
    assert_eq!(local.device_id, second.device_id);
    assert!(local.retired);
    assert!(matches!(
        commit(&db, secret, 5, reenroll).await,
        Err(DbError::DmDeviceStateConflict)
    ));
    assert!(matches!(
        commit(
            &db,
            secret,
            5,
            enroll(DmEnrollmentExpectation::Retired {
                device_id: [94; 16]
            })
        )
        .await,
        Err(DbError::DmDeviceStateConflict)
    ));
    // Retirement/re-enrollment preserve every still-live receive epoch.
    assert_eq!(
        db.read_with(|tx| {
            Ok(tx
                .open_table(&crate::ids_dm_epochs::TABLE)?
                .range(..)?
                .count())
        })
        .await?,
        2
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_device_requests_serialize_with_each_other_and_maintenance() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Arc::new(Database::new_in_memory(secret.id()).await?);
    let input = enroll(DmEnrollmentExpectation::Absent);
    let a = tokio::spawn({
        let db = db.clone();
        async move { commit(&db, secret, 1, input).await }
    });
    let b = tokio::spawn({
        let db = db.clone();
        async move { commit(&db, secret, 1, input).await }
    });
    let first = a.await??;
    assert_eq!(first.event.event_id, b.await??.event.event_id);
    let retire = DmDeviceRequest::Retire {
        device_id: first.device_id,
    };
    let a = tokio::spawn({
        let db = db.clone();
        async move { commit(&db, secret, 2, retire).await }
    });
    let b = tokio::spawn({
        let db = db.clone();
        async move { commit(&db, secret, 3, retire).await }
    });
    let (a, b) = (a.await?, b.await?);
    assert!(matches!(
        (&a, &b),
        (Ok(_), Err(DbError::DmDeviceStateConflict)) | (Err(DbError::DmDeviceStateConflict), Ok(_))
    ));
    let input = enroll(DmEnrollmentExpectation::Retired {
        device_id: first.device_id,
    });
    let a = tokio::spawn({
        let db = db.clone();
        async move { commit(&db, secret, 4, input).await }
    });
    let b = tokio::spawn({
        let db = db.clone();
        async move { commit(&db, secret, 5, input).await }
    });
    let (a, b) = (a.await?, b.await?);
    assert!(matches!(
        (&a, &b),
        (Ok(_), Err(DbError::DmDeviceStateConflict)) | (Err(DbError::DmDeviceStateConflict), Ok(_))
    ));
    let current = db.dm_local_installation().await?.unwrap();
    let maintained = db.dm_maintain_local_now().await?;
    assert_eq!(current.device_id, maintained.device_id);
    assert!(db.dm_announcement_known(&maintained).await?);

    let db = Arc::new(Database::new_in_memory(secret.id()).await?);
    let a = tokio::spawn({
        let db = db.clone();
        async move { commit(&db, secret, 1, enroll(DmEnrollmentExpectation::Absent)).await }
    });
    let b = tokio::spawn({
        let db = db.clone();
        async move { db.dm_maintain_local_now().await }
    });
    let a = a.await?;
    let b = b.await??;
    match a {
        Ok(outcome) => assert_eq!(outcome.device_id, b.device_id),
        Err(DbError::DmDeviceStateConflict) => {
            assert!(
                db.dm_device_receipt(
                    secret,
                    DmDeviceRequestId([1; 16]),
                    &enroll(DmEnrollmentExpectation::Absent)
                )
                .await?
                .is_none()
            );
        }
        Err(error) => return Err(error.into()),
    }
    assert!(matches!(
        commit(&db, secret, 9, enroll(DmEnrollmentExpectation::Absent)).await,
        Err(DbError::DmDeviceStateConflict)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_device_requests_rollback_lost_ack_reopen_and_rebuild() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("device.redb");
    let db = Database::open(&path, secret.id()).await?;
    // Exercise the actual schema-40 additive upgrade.
    db.write_with(|tx| {
        tx.as_raw()
            .delete_table(crate::ids_dm_device_receipts::TABLE.as_raw())?;
        tx.open_table(&crate::db_version::TABLE)?.insert(&(), &40)?;
        Ok(())
    })
    .await?;
    drop(db);
    let db = Arc::new(Database::open(&path, secret.id()).await?);
    let input = enroll(DmEnrollmentExpectation::Absent);
    for panic in [false, true] {
        let task = tokio::spawn({
            let db = db.clone();
            async move {
                db.commit_dm_device_request_inner(
                    secret,
                    DmDeviceRequestId([1; 16]),
                    &input,
                    &mut None,
                    &mut None,
                    Some(Box::new(move |_| {
                        assert!(!panic, "simulated precommit interruption");
                        Err(DbError::DmDeviceCandidate)
                    })),
                )
                .await
            }
        });
        if panic {
            assert!(matches!(task.await, Err(error) if error.is_panic()));
        } else {
            assert!(task.await?.is_err());
        }
        assert!(db.dm_local_installation().await?.is_none());
        assert!(db.dm_own_devices(None, 64).await?.is_empty());
        assert!(
            db.dm_device_receipt(secret, DmDeviceRequestId([1; 16]), &input)
                .await?
                .is_none()
        );
        assert_eq!(
            db.read_with(|tx| {
                Ok(tx
                    .open_table(&crate::ids_dm_epochs::TABLE)?
                    .range(..)?
                    .count())
            })
            .await?,
            0
        );
    }
    let task = tokio::spawn({
        let db = db.clone();
        async move {
            db.commit_dm_device_request_inner(
                secret,
                DmDeviceRequestId([1; 16]),
                &input,
                &mut None,
                &mut None,
                Some(Box::new(|tx| {
                    tx.on_commit(|| panic!("simulated lost acknowledgment"));
                    Ok(())
                })),
            )
            .await
        }
    });
    assert!(matches!(task.await, Err(error) if error.is_panic()));
    let accepted = db
        .dm_device_receipt(secret, DmDeviceRequestId([1; 16]), &input)
        .await?
        .unwrap();
    let old_public = db.dm_maintain_local_now().await?.epoch.unwrap();
    let body = rostra_dm::MessageBody::new(secret.id(), secret.id(), "retained fixture".into())?;
    db.write_with(|tx| Database::dm_store_history_tx(tx, &body, ShortEventId::random(), 1))
        .await?;
    commit(
        &db,
        secret,
        2,
        DmDeviceRequest::Retire {
            device_id: accepted.device_id,
        },
    )
    .await?;
    let deletion = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::DM_DEVICE)
        .delete(accepted.event.event_id.to_short())
        .build()
        .signed_by(secret);
    db.try_process_event(&VerifiedEvent::verify_signed(secret.id(), deletion)?)
        .await?;
    assert!(
        db.get_event_content(accepted.event.event_id)
            .await
            .is_none()
    );
    db.write_with(|tx| Database::prepare_total_migration(tx, 41))
        .await?;
    drop(db);
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        commit(&db, secret, 1, input).await?.event.event_id,
        accepted.event.event_id
    );
    assert!(
        db.get_event_content(accepted.event.event_id)
            .await
            .is_none()
    );
    let local = db.dm_local_installation().await?.unwrap();
    assert!(local.retired);
    assert_eq!(local.device_id, accepted.device_id);
    assert!(db.dm_own_devices(None, 64).await?[0].1.retired());
    assert_eq!(
        db.dm_history_with(secret.id(), None, 10).await?[0].text,
        "retained fixture"
    );
    db.read_with(|tx| {
        let epochs = tx.open_table(&crate::ids_dm_epochs::TABLE)?;
        let epoch = epochs
            .get(&(accepted.device_id, old_public.public_key))?
            .unwrap()
            .value_try()?;
        assert!(epoch.public() == &old_public);
        Ok(())
    })
    .await?;
    let extension = crate::ExtensionTableDefinition::<u64, u64>::new("ids_dm_device_receipts");
    assert!(
        db.extension_write(|tx| {
            tx.open_table(&extension)?;
            Ok(())
        })
        .await
        .is_err()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_device_requests_admission_refusal_is_atomic() -> anyhow::Result<()> {
    use std::num::{NonZeroU64, NonZeroUsize};

    use crate::{PayloadAdmissionConfig, PayloadAdmissionLimits};
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    while !db
        .rebuild_payload_accounting(NonZeroUsize::new(100).unwrap())
        .await?
        .ready
    {}
    let config = PayloadAdmissionConfig::new(PayloadAdmissionLimits {
        database_bytes: NonZeroU64::new(1).unwrap(),
        author_bytes: NonZeroU64::new(1).unwrap(),
        overrides: Default::default(),
        in_flight_count: NonZeroUsize::new(4).unwrap(),
        in_flight_bytes: NonZeroU64::new(10000).unwrap(),
    })
    .unwrap();
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = Some(config.clone());
        Ok(())
    })
    .await?;
    let input = enroll(DmEnrollmentExpectation::Absent);
    assert!(matches!(
        commit(&db, secret, 1, input).await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    assert!(db.dm_local_installation().await?.is_none());
    assert!(db.dm_own_devices(None, 64).await?.is_empty());
    assert!(
        db.dm_device_receipt(secret, DmDeviceRequestId([1; 16]), &input)
            .await?
            .is_none()
    );
    assert_eq!(db.payload_admission_usage().acquisitions, 0);
    assert_eq!(db.payload_admission_usage().buffers, 0);
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = None;
        Ok(())
    })
    .await?;
    let accepted = commit(&db, secret, 1, input).await?;
    db.write_with(|_| {
        db.payload_admission.state.lock().unwrap().config = Some(config);
        Ok(())
    })
    .await?;
    assert_eq!(
        commit(&db, secret, 1, input).await?.event.event_id,
        accepted.event.event_id
    );
    assert!(matches!(
        commit(
            &db,
            secret,
            2,
            DmDeviceRequest::Retire {
                device_id: accepted.device_id
            }
        )
        .await,
        Err(DbError::PayloadAdmissionPaused { .. })
    ));
    assert!(!db.dm_local_installation().await?.unwrap().retired);
    assert!(!db.dm_own_devices(None, 64).await?[0].1.retired());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dm_device_requests_cancellation_does_not_imply_rollback() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Arc::new(Database::new_in_memory(secret.id()).await?);
    let input = enroll(DmEnrollmentExpectation::Absent);
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let task = tokio::spawn({
        let db = db.clone();
        async move {
            db.commit_dm_device_request_inner(
                secret,
                DmDeviceRequestId([1; 16]),
                &input,
                &mut None,
                &mut None,
                Some(Box::new(move |_| {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                    Ok(())
                })),
            )
            .await
        }
    });
    started.await?;
    task.abort();
    release.send(())?;
    // A synchronous transaction may complete even if its async owner is aborted.
    let _ = task.await;
    let outcome = commit(&db, secret, 1, input).await?;
    assert_eq!(
        db.dm_local_installation().await?.unwrap().device_id,
        outcome.device_id
    );
    assert_eq!(db.dm_own_devices(None, 64).await?.len(), 1);
    assert_eq!(
        db.read_with(|tx| {
            Ok(tx
                .open_table(&crate::ids_dm_epochs::TABLE)?
                .range(..)?
                .count())
        })
        .await?,
        1
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_device_requests_inventory_and_receipt_corruption_are_fallible() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    for corruption in 0..4 {
        let db = Database::new_in_memory(secret.id()).await?;
        let key = bincode::encode_to_vec((secret.id(), [7u8; 16]), redb_bincode::BINCODE_CONFIG)?;
        let value = bincode::encode_to_vec(
            rostra_dm::DeviceState::default(),
            redb_bincode::BINCODE_CONFIG,
        )?;
        let (key, value) = match corruption {
            0 => ([key.as_slice(), &[0]].concat(), value),
            1 => (key, [value.as_slice(), &[0]].concat()),
            2 => (key[..key.len() - 1].to_vec(), value),
            _ => (key, vec![255]),
        };
        db.write_with(|tx| {
            tx.as_raw()
                .open_table(crate::ids_dm_devices::TABLE.as_raw())?
                .insert(key.as_slice(), value.as_slice())?;
            Ok(())
        })
        .await?;
        assert!(db.dm_own_devices(None, 64).await.is_err());
        assert!(db.dm_own_devices(None, 0).await?.is_empty());
    }
    let db = Database::new_in_memory(secret.id()).await?;
    let foreign = RostraIdSecretKey::generate().id();
    let foreign_key = bincode::encode_to_vec((foreign, [0u8; 16]), redb_bincode::BINCODE_CONFIG)?;
    let corrupt_key =
        bincode::encode_to_vec((secret.id(), [64u8; 16]), redb_bincode::BINCODE_CONFIG)?;
    db.write_with(|tx| {
        for index in 0..64u8 {
            db.dm_apply_announcement_tx(
                secret.id(),
                &Announcement {
                    device_id: [index; 16],
                    epoch: None,
                },
                1,
                ShortEventId::random(),
                tx,
            )?;
        }
        let mut raw = tx
            .as_raw()
            .open_table(crate::ids_dm_devices::TABLE.as_raw())?;
        raw.insert(foreign_key.as_slice(), &[255][..])?;
        raw.insert(corrupt_key.as_slice(), &[255][..])?;
        Ok(())
    })
    .await?;
    let first = db.dm_own_devices(None, usize::MAX).await?;
    assert_eq!(first.len(), 64);
    assert_eq!(first.first().unwrap().0, [0; 16]);
    assert_eq!(first.last().unwrap().0, [63; 16]);
    assert!(db.dm_own_devices(Some([63; 16]), 64).await.is_err());
    let db = Database::new_in_memory(secret.id()).await?;
    let input = enroll(DmEnrollmentExpectation::Absent);
    let accepted = commit(&db, secret, 1, input).await?;
    db.write_with(|tx| {
        let mut receipts = tx.open_table(&crate::ids_dm_device_receipts::TABLE)?;
        let mut receipt = receipts.get(&[1; 16])?.unwrap().value_try()?;
        let crate::dm_device_receipt::DmDeviceReceipt::V1 { announcement, .. } = &mut receipt;
        announcement.device_id = [88; 16];
        receipts.insert(&[1; 16], &receipt)?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        commit(&db, secret, 1, input).await,
        Err(DbError::DmDeviceCandidate)
    ));
    assert_eq!(
        db.dm_local_installation().await?.unwrap().device_id,
        accepted.device_id
    );
    let key = bincode::encode_to_vec((), redb_bincode::BINCODE_CONFIG)?;
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(crate::ids_dm_installation::TABLE.as_raw())?
            .insert(key.as_slice(), &[255][..])?;
        Ok(())
    })
    .await?;
    assert!(db.dm_local_installation().await.is_err());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_device_requests_upgrade_preserves_active_and_retired_installations()
-> anyhow::Result<()> {
    use redb::ReadableTable as _;

    for retired in [false, true] {
        let secret = RostraIdSecretKey::generate();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("upgrade.redb");
        let db = Database::open(&path, secret.id()).await?;
        let original = db.dm_maintain_local_now().await?;
        announce(&db, secret, &original).await?;
        if retired {
            announce(
                &db,
                secret,
                &Announcement {
                    device_id: original.device_id,
                    epoch: None,
                },
            )
            .await?;
        }
        // These authoritative formats are unchanged from schema 40. Compare
        // exact serialized rows without formatting secret epoch material.
        let snapshot = |tx: &crate::ReadTransaction| -> DbResult<Vec<RawTableSnapshot>> {
            [
                crate::ids_dm_installation::TABLE.as_raw(),
                crate::ids_dm_epochs::TABLE.as_raw(),
                crate::ids_dm_devices::TABLE.as_raw(),
            ]
            .into_iter()
            .map(|definition| {
                tx.as_raw()
                    .open_table(definition)?
                    .iter()?
                    .map(|row| {
                        let (key, value) = row?;
                        Ok((key.value().to_vec(), value.value().to_vec()))
                    })
                    .collect()
            })
            .collect()
        };
        let before = db.read_with(snapshot).await?;
        db.write_with(|tx| {
            tx.as_raw()
                .delete_table(crate::ids_dm_device_receipts::TABLE.as_raw())?;
            tx.open_table(&crate::db_version::TABLE)?.insert(&(), &40)?;
            Ok(())
        })
        .await?;
        drop(db);
        let db = Database::open(&path, secret.id()).await?;
        assert!(before == db.read_with(snapshot).await?);
        let local = db.dm_local_installation().await?.unwrap();
        assert_eq!(local.device_id, original.device_id);
        assert_eq!(local.retired, retired);
        let devices = db.dm_own_devices(None, 64).await?;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].0, original.device_id);
        assert_eq!(devices[0].1.retired(), retired);
        if !retired {
            assert!(devices[0].1.latest().unwrap().2 == original.epoch.as_ref().unwrap());
        }
        db.write_with(|tx| {
            tx.open_table(&crate::db_version::TABLE)?
                .insert(&(), &u64::MAX)?;
            Ok(())
        })
        .await?;
        drop(db);
        assert!(matches!(
            Database::open(&path, secret.id()).await,
            Err(DbError::DbVersionTooHigh { .. })
        ));
    }
    Ok(())
}
