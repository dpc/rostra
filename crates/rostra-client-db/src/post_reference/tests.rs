use rostra_core::event::{
    Event, EventContentKind as _, EventContentRaw, EventKind, SignedEvent, SocialPost,
    VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraIdSecretKey, ToShort as _};

use super::*;
use crate::post_reference::PostReferenceResolution as Resolution;

fn post(secret: RostraIdSecretKey, replace: Option<ShortEventId>) -> VerifiedEventContent {
    let content = SocialPost::new_text("body".to_owned(), None, Default::default())
        .serialize_cbor()
        .unwrap();
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_POST)
        .maybe_parent_prev(replace)
        .maybe_delete(replace)
        .content(&content)
        .build()
        .signed_by(secret);
    VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), signed).unwrap(),
        content,
    )
    .unwrap()
}

fn reference(author: RostraId, event: EventId) -> PostReference {
    PostReference {
        author: PostAuthorReference::Full(author),
        event: PostEventReference::Full(event),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn retained_resolution_preserves_exact_ids_author_binding_and_unknown_forms()
-> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let other = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let content = post(secret, None);
    let id = content.event_id();
    let mut requested = reference(secret.id(), id);
    assert_eq!(
        db.resolve_post_reference(requested).await?,
        Resolution::UnknownEvent {
            author: secret.id(),
            event: requested.event
        }
    );
    requested.author = PostAuthorReference::Short(secret.id().to_short());
    assert_eq!(
        db.resolve_post_reference(requested).await?,
        Resolution::UnknownAuthor
    );
    assert_eq!(
        db.try_get_known_identity(secret.id().to_short()).await?,
        None
    );
    db.try_process_event(&content.event).await?;
    assert_eq!(
        db.try_get_known_identity(secret.id().to_short()).await?,
        Some(secret.id())
    );
    for author in [
        PostAuthorReference::Full(secret.id()),
        PostAuthorReference::Short(secret.id().to_short()),
    ] {
        for event in [
            PostEventReference::Full(id),
            PostEventReference::Short(id.to_short()),
        ] {
            assert_eq!(
                db.resolve_post_reference(PostReference { author, event })
                    .await?,
                Resolution::Resolved {
                    author: secret.id(),
                    requested_event_id: id,
                    availability: EventContentAvailability::Missing,
                }
            );
        }
    }
    let mut spoof: [u8; 32] = id.into();
    spoof[31] ^= 1;
    let spoof = EventId::from_bytes(spoof);
    assert_eq!(spoof.to_short(), id.to_short());
    assert_eq!(
        db.resolve_post_reference(reference(secret.id(), spoof))
            .await?,
        Resolution::FullEventMismatch
    );
    assert_eq!(
        db.resolve_post_reference(reference(other.id(), id)).await?,
        Resolution::AuthorMismatch
    );
    // A full author sharing the retained prefix is not silently remapped.
    let mut collision = secret.id().to_bytes();
    collision[31] ^= 1;
    let collision = RostraId::from_bytes(collision);
    assert_eq!(collision.to_short(), secret.id().to_short());
    assert_eq!(
        db.resolve_post_reference(reference(collision, id)).await?,
        Resolution::AuthorMismatch
    );
    let absent = ShortEventId::from_bytes([42; 16]);
    assert_eq!(
        db.resolve_post_reference(PostReference {
            author: PostAuthorReference::Short(secret.id().to_short()),
            event: PostEventReference::Short(absent),
        })
        .await?,
        Resolution::UnknownEvent {
            author: secret.id(),
            event: PostEventReference::Short(absent)
        }
    );
    let null = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::NULL)
        .content(&EventContentRaw::new(vec![]))
        .build()
        .signed_by(secret);
    let null = VerifiedEvent::verify_signed(secret.id(), null)?;
    db.try_process_event(&null).await?;
    assert_eq!(
        db.resolve_post_reference(reference(secret.id(), null.event_id))
            .await?,
        Resolution::UnsupportedKind
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn requested_lifecycle_survives_reopen_without_canonicalizing_before_validation()
-> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let old = post(secret, None);
    let edit = post(secret, Some(old.event_id().to_short()));
    {
        let db = Database::open(&path, secret.id()).await?;
        db.try_process_event(&old.event).await?;
        db.process_event_content(&old).await;
        assert_eq!(
            db.resolve_post_reference(reference(secret.id(), old.event_id()))
                .await?,
            Resolution::Resolved {
                author: secret.id(),
                requested_event_id: old.event_id(),
                availability: EventContentAvailability::Available,
            }
        );
        db.try_process_event(&edit.event).await?;
        db.process_event_content(&edit).await;
    }
    let db = Database::open(&path, secret.id()).await?;
    assert_eq!(
        db.resolve_post_reference(reference(secret.id(), old.event_id()))
            .await?,
        Resolution::Resolved {
            author: secret.id(),
            requested_event_id: old.event_id(),
            availability: EventContentAvailability::Deleted,
        }
    );
    assert_eq!(
        db.get_social_post_state(old.event_id().to_short())
            .await
            .unwrap()
            .event_id,
        edit.event_id().to_short()
    );
    let mut spoof: [u8; 32] = old.event_id().into();
    spoof[31] ^= 1;
    assert_eq!(
        db.resolve_post_reference(reference(secret.id(), EventId::from_bytes(spoof)))
            .await?,
        Resolution::FullEventMismatch
    );
    // Lifecycle reads do not require or decode any content-store payload.
    for (state, availability) in [
        (
            EventContentState::Pruned,
            EventContentAvailability::Pruned { quota_reason: None },
        ),
        (
            EventContentState::Invalid,
            EventContentAvailability::Invalid,
        ),
    ] {
        db.write_with(|tx| {
            tx.open_table(&crate::events_content_state::TABLE)?
                .insert(&edit.event_id().to_short(), &state)?;
            Ok(())
        })
        .await?;
        assert_eq!(
            db.resolve_post_reference(reference(secret.id(), edit.event_id()))
                .await?,
            Resolution::Resolved {
                author: secret.id(),
                requested_event_id: edit.event_id(),
                availability
            }
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_envelopes_and_lifecycle_rows_fail_closed() -> anyhow::Result<()> {
    let secret = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(secret.id()).await?;
    let post = post(secret, None);
    db.try_process_event(&post.event).await?;
    let key = bincode::encode_to_vec(post.event_id().to_short(), redb_bincode::BINCODE_CONFIG)?;
    let valid = bincode::encode_to_vec(
        EventRecord {
            signed: post.event.into(),
        },
        redb_bincode::BINCODE_CONFIG,
    )?;
    let mut trailing = valid.clone();
    trailing.push(0);
    let mut bad_signature: SignedEvent = post.event.into();
    bad_signature.sig = post
        .event
        .event
        .signed_by(RostraIdSecretKey::generate())
        .sig;
    let bad_signature = bincode::encode_to_vec(
        EventRecord {
            signed: bad_signature,
        },
        redb_bincode::BINCODE_CONFIG,
    )?;
    for bytes in [vec![], vec![0; 1025], trailing, bad_signature] {
        db.write_with(|tx| {
            tx.as_raw()
                .open_table(crate::events::TABLE.as_raw())?
                .insert(key.as_slice(), bytes.as_slice())?;
            Ok(())
        })
        .await?;
        assert_eq!(
            db.resolve_post_reference(reference(secret.id(), post.event_id()))
                .await?,
            Resolution::Invalid
        );
    }
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(crate::events::TABLE.as_raw())?
            .insert(key.as_slice(), valid.as_slice())?;
        tx.as_raw()
            .open_table(crate::events_content_state::TABLE.as_raw())?
            .insert(key.as_slice(), &[0; 65][..])?;
        Ok(())
    })
    .await?;
    assert_eq!(
        db.resolve_post_reference(reference(secret.id(), post.event_id()))
            .await?,
        Resolution::Invalid
    );
    Ok(())
}
