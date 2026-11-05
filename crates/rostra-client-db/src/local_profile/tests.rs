//! Local-only integration coverage, including raw hostile projection records.

use rostra_core::event::content_kind::{EventContentKind as _, SocialProfileUpdate};
use rostra_core::event::{Event, EventExt as _, EventKind};
use rostra_core::id::RostraIdSecretKey;

use super::*;
use crate::{EventRecord, IdSocialProfileRecord, Latest};

async fn store(
    db: &Database,
    secret: RostraIdSecretKey,
    name: &str,
) -> anyhow::Result<Latest<IdSocialProfileRecord>> {
    let body = SocialProfileUpdate {
        display_name: name.into(),
        bio: "bio".into(),
        avatar: Some(("image/png".into(), vec![42; 16])),
    }
    .serialize_cbor()?;
    let event = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_PROFILE_UPDATE)
        .content(&body)
        .build()
        .signed_by(secret);
    let record = Latest {
        ts: event.timestamp(),
        inner: IdSocialProfileRecord {
            event_id: event.compute_short_id(),
            display_name: name.into(),
            bio: "bio".into(),
            avatar: Some(("image/png".into(), vec![42; 16])),
        },
    };
    db.write_with(|tx| {
        tx.open_table(&crate::events::TABLE)?
            .insert(&event.compute_short_id(), &EventRecord { signed: event })?;
        Ok(())
    })
    .await?;
    raw(db, secret.id(), encoded(&record)?).await?;
    Ok(record)
}

fn encoded<T: bincode::Encode>(value: T) -> anyhow::Result<Vec<u8>> {
    Ok(bincode::encode_to_vec(value, redb_bincode::BINCODE_CONFIG)?)
}

async fn raw(db: &Database, identity: RostraId, raw: Vec<u8>) -> anyhow::Result<()> {
    let key = encoded(identity)?;
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(crate::social_profiles::TABLE.as_raw())?
            .insert(key.as_slice(), raw.as_slice())?;
        Ok(())
    })
    .await?;
    Ok(())
}

async fn available(db: &Database, identity: RostraId) -> anyhow::Result<LocalProfile> {
    let LocalProfileRead::Available(profile) = db.read_local_profile(identity).await? else {
        anyhow::bail!("expected available profile");
    };
    Ok(profile)
}

#[tokio::test(flavor = "multi_thread")]
async fn exact_identity_revision_and_removal() -> anyhow::Result<()> {
    let owner = RostraIdSecretKey::generate();
    let other = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(owner.id()).await?;
    assert_eq!(
        db.read_local_profile(other.id()).await?,
        LocalProfileRead::Unavailable(ProfileIssue::Missing)
    );
    let mut record = store(&db, other, "Alice").await?;
    let first = available(&db, other.id()).await?;
    assert_eq!(first.identity, other.id());
    assert_eq!(first.timestamp, record.ts);
    assert_eq!(
        first.display_name,
        ProfileField::Available("Alice".to_owned())
    );
    assert_eq!(
        first.avatar,
        ProfileField::Available(Some(LocalProfileAvatar {
            declared_mime: "image/png".into(),
            data: vec![42; 16]
        }))
    );
    assert_eq!(available(&db, other.id()).await?, first);
    // Same-source local projection repairs must invalidate cache revisions too.
    record.inner.avatar = None;
    raw(&db, other.id(), encoded(&record)?).await?;
    let absent_avatar = available(&db, other.id()).await?;
    assert_eq!(absent_avatar.event_id, first.event_id);
    assert_ne!(absent_avatar.revision, first.revision);
    assert_eq!(absent_avatar.avatar, ProfileField::Available(None));
    record.inner.display_name = "Bob".into();
    raw(&db, other.id(), encoded(&record)?).await?;
    assert_ne!(
        available(&db, other.id()).await?.revision,
        absent_avatar.revision
    );
    let replacement = store(&db, other, "Replacement").await?;
    assert_ne!(replacement.inner.event_id, record.inner.event_id);
    assert_ne!(available(&db, other.id()).await?.event_id, first.event_id);
    // Exact author checks reject copying an otherwise-valid projection to self
    // or to a different full ID with an identical short prefix.
    let mut collision = other.id().to_bytes();
    collision[31] ^= 1;
    for wrong in [owner.id(), RostraId::from_bytes(collision)] {
        raw(&db, wrong, encoded(&record)?).await?;
        assert_eq!(
            db.read_local_profile(wrong).await?,
            LocalProfileRead::Unavailable(ProfileIssue::MismatchedSource)
        );
    }
    db.write_with(|tx| {
        tx.open_table(&crate::social_profiles::TABLE)?
            .remove(&other.id())?;
        Ok(())
    })
    .await?;
    assert_eq!(
        db.read_local_profile(other.id()).await?,
        LocalProfileRead::Unavailable(ProfileIssue::Missing)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn field_caps_and_raw_preflight() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let original = store(&db, secret, "Alice").await?;
    let mut record = original.clone();
    record.inner.display_name = "x".repeat(NAME_BYTES_MAX);
    record.inner.avatar = Some(("x".repeat(MIME_BYTES_MAX), vec![42; AVATAR_BYTES_MAX]));
    raw(&db, secret.id(), encoded(&record)?).await?;
    let result = available(&db, secret.id()).await?;
    assert!(matches!(result.display_name, ProfileField::Available(s) if s.len() == NAME_BYTES_MAX));
    assert!(
        matches!(result.avatar, ProfileField::Available(Some(a)) if a.data.len() == AVATAR_BYTES_MAX && a.declared_mime.len() == MIME_BYTES_MAX)
    );
    record.inner.display_name.push('x');
    raw(&db, secret.id(), encoded(&record)?).await?;
    let result = available(&db, secret.id()).await?;
    assert_eq!(
        result.display_name,
        ProfileField::Unavailable(ProfileIssue::Oversized)
    );
    assert!(matches!(result.avatar, ProfileField::Available(Some(_))));
    for (mime, data) in [
        ("x".repeat(MIME_BYTES_MAX + 1), vec![0]),
        ("image/png".into(), vec![0; AVATAR_BYTES_MAX + 1]),
    ] {
        record = original.clone();
        record.inner.avatar = Some((mime, data));
        raw(&db, secret.id(), encoded(&record)?).await?;
        let result = available(&db, secret.id()).await?;
        assert_eq!(result.display_name, ProfileField::Available("Alice".into()));
        assert_eq!(
            result.avatar,
            ProfileField::Unavailable(ProfileIssue::Oversized)
        );
    }
    // Invalid from byte zero: Oversized rather than Corrupt proves the raw cap
    // precedes deserialization, independently of any valid source header.
    raw(&db, secret.id(), vec![255; PROFILE_BYTES_MAX + 1]).await?;
    assert_eq!(
        db.read_local_profile(secret.id()).await?,
        LocalProfileRead::Unavailable(ProfileIssue::Oversized)
    );
    raw(&db, secret.id(), vec![255; PROFILE_BYTES_MAX]).await?;
    assert_eq!(
        db.read_local_profile(secret.id()).await?,
        LocalProfileRead::Unavailable(ProfileIssue::Corrupt)
    );
    record = original;
    record.inner.bio = "b".repeat(PROFILE_BYTES_MAX);
    let overhead = encoded(&record)?.len() - record.inner.bio.len();
    record.inner.bio.truncate(PROFILE_BYTES_MAX - overhead);
    let exact = encoded(&record)?;
    assert_eq!(exact.len(), PROFILE_BYTES_MAX);
    raw(&db, secret.id(), exact).await?;
    assert!(matches!(
        available(&db, secret.id()).await?.avatar,
        ProfileField::Available(Some(_))
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_and_stale_sources() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let record = store(&db, secret, "Alice").await?;
    let mut trailing = encoded(&record)?;
    trailing.push(0);
    let mut hostile = encoded((record.ts, record.inner.event_id))?;
    hostile.extend(encoded(u64::MAX)?);
    for invalid in [vec![], trailing, hostile] {
        raw(&db, secret.id(), invalid).await?;
        assert_eq!(
            db.read_local_profile(secret.id()).await?,
            LocalProfileRead::Unavailable(ProfileIssue::Corrupt)
        );
    }
    let mut wrong_timestamp = record.clone();
    wrong_timestamp.ts = Timestamp::from(1);
    raw(&db, secret.id(), encoded(wrong_timestamp)?).await?;
    assert_eq!(
        db.read_local_profile(secret.id()).await?,
        LocalProfileRead::Unavailable(ProfileIssue::MismatchedSource)
    );
    raw(&db, secret.id(), encoded(&record)?).await?;
    let source = db
        .read_with(|tx| {
            Ok(tx
                .open_table(&crate::events::TABLE)?
                .get(&record.inner.event_id)?
                .expect("stored source")
                .value())
        })
        .await?;
    let unrelated = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_PROFILE_UPDATE)
        .content(&rostra_core::event::EventContentRaw::new(
            b"unrelated".to_vec(),
        ))
        .build()
        .signed_by(secret);
    let invalid_signature = EventRecord {
        signed: rostra_core::event::SignedEvent::unverified(
            *source.signed.event(),
            unrelated.sig(),
        ),
    };
    assert_eq!(
        invalid_signature.signed.compute_id(),
        source.signed.compute_id()
    );
    let mut trailing_source = encoded(&source)?;
    trailing_source.push(0);
    for (source_bytes, issue) in [
        (encoded(invalid_signature)?, ProfileIssue::MismatchedSource),
        (trailing_source, ProfileIssue::MismatchedSource),
        (vec![255; 1025], ProfileIssue::Corrupt),
    ] {
        let key = encoded(record.inner.event_id)?;
        db.write_with(|tx| {
            tx.as_raw()
                .open_table(crate::events::TABLE.as_raw())?
                .insert(key.as_slice(), source_bytes.as_slice())?;
            Ok(())
        })
        .await?;
        assert_eq!(
            db.read_local_profile(secret.id()).await?,
            LocalProfileRead::Unavailable(issue)
        );
    }
    db.write_with(|tx| {
        tx.open_table(&crate::events::TABLE)?
            .remove(&record.inner.event_id)?;
        Ok(())
    })
    .await?;
    assert_eq!(
        db.read_local_profile(secret.id()).await?,
        LocalProfileRead::Unavailable(ProfileIssue::MissingSource)
    );
    Ok(())
}
