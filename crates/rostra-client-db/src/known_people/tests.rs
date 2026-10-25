//! Offline database integration tests; no Client, peer, discovery or network.

use std::collections::BTreeSet;

use rostra_core::event::content_kind::{EventContentKind as _, SocialProfileUpdate};
use rostra_core::event::{Event, EventExt as _, EventKind, VerifiedEvent, VerifiedEventContent};
use rostra_core::id::RostraIdSecretKey;
use rostra_core::{ShortEventId, Timestamp};

use super::*;
use crate::{Database, DbError, EventRecord, IdSocialProfileRecord, IdsFolloweesRecord, Latest};

fn id(n: u32) -> RostraId {
    let mut bytes = [0; 32];
    bytes[28..].copy_from_slice(&n.to_be_bytes());
    RostraId::from_bytes(bytes)
}

async fn edges(db: &Database, edges: &[(RostraId, RostraId)]) -> anyhow::Result<()> {
    db.write_with(|tx| {
        let mut table = tx.open_table(&crate::ids_followees::TABLE)?;
        for &(a, b) in edges {
            table.insert(
                &(a, b),
                &IdsFolloweesRecord::new(
                    Timestamp::from(1),
                    ShortEventId::from_bytes([0; 16]),
                    Timestamp::from(1),
                    None,
                    None,
                ),
            )?;
        }
        Ok(())
    })
    .await?;
    Ok(())
}

async fn profile(
    db: &Database,
    secret: RostraIdSecretKey,
    name: &str,
    avatar_size: usize,
) -> anyhow::Result<Latest<IdSocialProfileRecord>> {
    let body = SocialProfileUpdate {
        display_name: name.into(),
        bio: "biography".into(),
        avatar: Some(("image/png".into(), vec![42; avatar_size])),
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
            bio: "biography".into(),
            avatar: Some(("image/png".into(), vec![42; avatar_size])),
        },
    };
    db.write_with(|tx| {
        tx.open_table(&crate::events::TABLE)?
            .insert(&event.compute_short_id(), &EventRecord { signed: event })?;
        tx.open_table(&crate::social_profiles::TABLE)?
            .insert(&secret.id(), &record)?;
        Ok(())
    })
    .await?;
    Ok(record)
}

async fn exhaust(db: &Database, query: &str) -> anyhow::Result<KnownPeoplePage> {
    let mut cursor = None;
    for _ in 0..10_000 {
        let page = db.search_known_people(query, cursor.as_ref()).await?;
        assert!(page.graph_steps > 0 && page.graph_steps <= GRAPH_STEPS_MAX);
        assert!(page.profiles_read <= PROFILES_MAX);
        assert!(page.rows.len() <= RESULTS_MAX);
        assert_eq!(
            page.rows
                .iter()
                .map(|r| r.identity)
                .collect::<BTreeSet<_>>()
                .len(),
            page.rows.len()
        );
        assert_eq!(
            page.next_cursor.is_some(),
            page.stop == SearchStop::Incomplete
        );
        if page.stop == SearchStop::Exhausted {
            return Ok(page);
        }
        cursor = page.next_cursor;
    }
    panic!("bounded continuation did not terminate");
}

#[tokio::test(flavor = "multi_thread")]
async fn dense_cycles_duplicates_and_sparse_empty_pages_equal_web_membership() -> anyhow::Result<()>
{
    let db = Database::new_in_memory(id(500)).await?;
    let mut graph = Vec::new();
    for n in 0..24 {
        graph.push((id(500), id(n)));
        for m in 0..32 {
            graph.push((id(n), id(m)));
        }
        graph.push((id(n), id(500)));
    }
    // Third-hop-only and unrelated identities never become candidates.
    graph.push((id(31), id(900)));
    graph.push((id(901), id(902)));
    edges(&db, &graph).await?;
    let first = db.search_known_people("does-not-match", None).await?;
    assert!(first.rows.is_empty());
    assert_eq!(first.stop, SearchStop::Incomplete);
    let empty = exhaust(&db, "does-not-match").await?;
    assert!(empty.rows.is_empty());

    let all = exhaust(&db, "rs").await?;
    let (direct, extended) = db.get_followees_extended(id(500)).await;
    let expected: BTreeSet<_> = [id(500)]
        .into_iter()
        .chain(direct.keys().copied())
        .chain(extended)
        .collect();
    assert_eq!(
        all.rows.iter().map(|r| r.identity).collect::<Vec<_>>(),
        expected.into_iter().take(10).collect::<Vec<_>>()
    );
    assert_eq!(all.unscorable_profiles, 0);
    assert!(exhaust(&db, "").await?.rows.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn unicode_names_scoring_ties_and_query_limits_preserve_web_rules() -> anyhow::Result<()> {
    let owner = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(owner.id()).await?;
    let mut people = vec![(owner.id(), "İ Alpha".to_owned())];
    profile(&db, owner, "İ Alpha", 500_000).await?;
    for name in [
        "İ Alpha",
        "I\u{307} alpha",
        "i--alpha",
        "Alpha Î",
        "space name",
        "😀Î",
    ] {
        let secret = RostraIdSecretKey::generate();
        profile(&db, secret, name, 0).await?;
        edges(&db, &[(owner.id(), secret.id())]).await?;
        people.push((secret.id(), name.into()));
    }
    for query in ["İ", "i\u{307}a", "AL", " a", "😀", "rs", "", "missing"] {
        let q = query.to_lowercase();
        let mut expected: Vec<_> = people
            .iter()
            .filter_map(|(identity, name)| {
                let score = fuzzy_match(&q, &name.to_lowercase())
                    .max(fuzzy_match(&q, &identity.to_string().to_lowercase()));
                (score > 0).then_some((score, *identity))
            })
            .collect();
        expected.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let page = exhaust(&db, query).await?;
        assert_eq!(
            page.rows
                .iter()
                .map(|r| (r.score, r.identity))
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(page.unscorable_profiles, 0);
    }
    assert!(db.search_known_people(&"İ".repeat(128), None).await.is_ok());
    assert!(matches!(
        db.search_known_people(&"İ".repeat(129), None).await,
        Err(DbError::KnownPeopleQuery)
    ));
    // Fixed scoring expectations guard the extracted web implementation too.
    assert_eq!(fuzzy_match("ab", "ab"), 7);
    assert_eq!(fuzzy_match("ab", "a_b"), 8);
    assert_eq!(fuzzy_match("ab", "ba"), 0);
    assert_eq!(fuzzy_match("", "ab"), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn exact_lookup_separates_membership_profile_and_retained_prefix() -> anyhow::Result<()> {
    let db = Database::new_in_memory(id(500)).await?;
    let graph: Vec<_> = (0..260).map(|n| (id(500), id(n))).collect();
    edges(&db, &graph).await?;
    edges(&db, &[(id(259), id(700))]).await?;
    for target in [id(500), id(0)] {
        let lookup = db.lookup_known_person(target, None).await?;
        assert_eq!(lookup.graph_membership, GraphMembership::Known);
        assert!(!lookup.retained_identity);
        assert_eq!(
            lookup.profile.display_name,
            ProfileField::Unavailable(ProfileIssue::Missing)
        );
        assert_eq!(lookup.graph_steps, 0);
    }
    let first = db.lookup_known_person(id(700), None).await?;
    assert_eq!(first.graph_membership, GraphMembership::Incomplete);
    assert_eq!(first.graph_steps, GRAPH_STEPS_MAX);
    let cursor = first.next_cursor.unwrap();
    assert!(matches!(
        db.lookup_known_person(id(701), Some(&cursor)).await,
        Err(DbError::KnownPeopleCursorStale)
    ));
    let second = db.lookup_known_person(id(700), Some(&cursor)).await?;
    assert_eq!(second.graph_membership, GraphMembership::Incomplete);
    let last = db
        .lookup_known_person(id(700), second.next_cursor.as_ref())
        .await?;
    assert_eq!(last.graph_membership, GraphMembership::Known);

    db.write_with(|tx| {
        crate::ids_full::Table::open(tx)?.register(id(900))?;
        Ok(())
    })
    .await?;
    // Same short prefix, different full ID is not retained evidence.
    assert!(
        !db.lookup_known_person(id(901), None)
            .await?
            .retained_identity
    );
    let mut cursor = None;
    loop {
        let lookup = db.lookup_known_person(id(900), cursor.as_ref()).await?;
        assert!(lookup.retained_identity);
        if lookup.graph_membership != GraphMembership::Incomplete {
            assert_eq!(lookup.graph_membership, GraphMembership::Unknown);
            break;
        }
        cursor = lookup.next_cursor;
    }
    Ok(())
}

async fn raw_profile(db: &Database, identity: RostraId, bytes: &[u8]) -> anyhow::Result<()> {
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(crate::social_profiles::TABLE.as_raw())?
            .insert(read::encoded(identity)?.as_slice(), bytes)?;
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_corrupt_and_mismatched_profiles_keep_independent_evidence() -> anyhow::Result<()>
{
    let owner = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(owner.id()).await?;
    let other = RostraIdSecretKey::generate();
    let record = profile(&db, other, "Alice", 0).await?;
    edges(&db, &[(owner.id(), other.id())]).await?;
    let assert_name_issue = |profile: &PersonProfile, issue| {
        assert_eq!(profile.display_name, ProfileField::Unavailable(issue));
        assert_eq!(profile.timestamp, ProfileField::Available(record.ts));
    };
    let mut oversized = record.clone();
    oversized.inner.avatar = Some(("image/png".into(), vec![0; PROFILE_BYTES_MAX]));
    raw_profile(&db, other.id(), &read::encoded(&oversized)?).await?;
    let lookup = db.lookup_known_person(other.id(), None).await?;
    assert_eq!(lookup.graph_membership, GraphMembership::Known);
    assert_name_issue(&lookup.profile, ProfileIssue::Oversized);
    let page = exhaust(&db, "not-a-match").await?;
    assert!(page.rows.is_empty());
    assert_eq!(page.unscorable_profiles, 1);

    let mut oversized = record.clone();
    oversized.inner.display_name = "x".repeat(NAME_BYTES_MAX + 1);
    raw_profile(&db, other.id(), &read::encoded(&oversized)?).await?;
    assert_name_issue(
        &db.lookup_known_person(other.id(), None).await?.profile,
        ProfileIssue::Oversized,
    );
    let mut trailing = read::encoded(&record)?;
    trailing.push(0);
    raw_profile(&db, other.id(), &trailing).await?;
    assert_name_issue(
        &db.lookup_known_person(other.id(), None).await?.profile,
        ProfileIssue::Corrupt,
    );

    // An enormous hostile string length is borrowed/preflighted, never allocated.
    let mut hostile = read::encoded((record.ts, record.inner.event_id))?;
    hostile.extend(read::encoded(u64::MAX)?);
    raw_profile(&db, other.id(), &hostile).await?;
    assert_name_issue(
        &db.lookup_known_person(other.id(), None).await?.profile,
        ProfileIssue::Corrupt,
    );

    raw_profile(&db, owner.id(), &read::encoded(&record)?).await?;
    assert_eq!(
        db.lookup_known_person(owner.id(), None)
            .await?
            .profile
            .display_name,
        ProfileField::Unavailable(ProfileIssue::MismatchedSource)
    );
    let mut missing = record.clone();
    missing.inner.event_id = ShortEventId::from_bytes([99; 16]);
    raw_profile(&db, other.id(), &read::encoded(missing)?).await?;
    assert_eq!(
        db.lookup_known_person(other.id(), None)
            .await?
            .profile
            .display_name,
        ProfileField::Unavailable(ProfileIssue::MissingSource)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn live_cursor_scope_mutation_rebuild_and_reopen() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("people.redb");
    let owner = id(500);
    let db = Database::open(&path, owner).await?;
    edges(&db, &[(owner, id(10))]).await?;
    edges(&db, &(0..300).map(|n| (id(10), id(n))).collect::<Vec<_>>()).await?;
    let cursor = db
        .search_known_people("rs", None)
        .await?
        .next_cursor
        .unwrap();
    assert!(matches!(
        db.search_known_people("different", Some(&cursor)).await,
        Err(DbError::KnownPeopleCursorStale)
    ));
    let other = Database::new_in_memory(id(501)).await?;
    assert!(matches!(
        other.search_known_people("rs", Some(&cursor)).await,
        Err(DbError::KnownPeopleCursorStale)
    ));
    // Removing the current outer edge prevents further traversal under it, but
    // previously returned rows remain explicitly best-so-far until refresh.
    db.write_with(|tx| {
        tx.open_table(&crate::ids_followees::TABLE)?
            .remove(&(owner, id(10)))?;
        Ok(())
    })
    .await?;
    let page = db.search_known_people("rs", Some(&cursor)).await?;
    assert_eq!(page.stop, SearchStop::Exhausted);
    assert!(page.rows.iter().any(|r| r.identity != owner));
    assert_eq!(exhaust(&db, "rs").await?.rows.len(), 1);
    edges(&db, &(0..150).map(|n| (owner, id(n))).collect::<Vec<_>>()).await?;
    let lookup_cursor = db
        .lookup_known_person(id(999), None)
        .await?
        .next_cursor
        .unwrap();
    db.write_with(|tx| {
        tx.open_table(&crate::social_notification_epoch::TABLE)?
            .insert(&(), &[7; 16])?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.search_known_people("rs", Some(&cursor)).await,
        Err(DbError::KnownPeopleCursorStale)
    ));
    assert!(matches!(
        db.lookup_known_person(id(999), Some(&lookup_cursor)).await,
        Err(DbError::KnownPeopleCursorStale)
    ));
    let cursor = db
        .search_known_people("rs", None)
        .await?
        .next_cursor
        .unwrap();
    let lookup_cursor = db
        .lookup_known_person(id(999), None)
        .await?
        .next_cursor
        .unwrap();
    drop(db);
    let reopened = Database::open(&path, owner).await?;
    assert!(matches!(
        reopened.search_known_people("rs", Some(&cursor)).await,
        Err(DbError::KnownPeopleCursorStale)
    ));
    assert!(matches!(
        reopened
            .lookup_known_person(id(999), Some(&lookup_cursor))
            .await,
        Err(DbError::KnownPeopleCursorStale)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_graph_keys_fail_without_typed_range_panics() -> anyhow::Result<()> {
    let owner = id(500);
    let db = Database::new_in_memory(owner).await?;
    db.write_with(|tx| {
        let mut key = read::encoded(owner)?;
        key.push(0);
        tx.as_raw()
            .open_table(crate::ids_followees::TABLE.as_raw())?
            .insert(key.as_slice(), &[][..])?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.search_known_people("rs", None).await,
        Err(DbError::KnownPeopleInvariant)
    ));
    assert!(matches!(
        db.lookup_known_person(id(999), None).await,
        Err(DbError::KnownPeopleInvariant)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn real_ingested_profile_is_local_evidence_outside_the_candidate_graph() -> anyhow::Result<()>
{
    let db = Database::new_in_memory(id(500)).await?;
    let secret = RostraIdSecretKey::generate();
    let raw = SocialProfileUpdate {
        display_name: "Outside".into(),
        bio: String::new(),
        avatar: None,
    }
    .serialize_cbor()?;
    let signed = Event::builder_raw_content()
        .author(secret.id())
        .kind(EventKind::SOCIAL_PROFILE_UPDATE)
        .content(&raw)
        .build()
        .signed_by(secret);
    db.try_process_event_with_content(&VerifiedEventContent::verify(
        VerifiedEvent::verify_signed(secret.id(), signed)?,
        raw,
    )?)
    .await?;
    let lookup = db.lookup_known_person(secret.id(), None).await?;
    assert_eq!(lookup.graph_membership, GraphMembership::Unknown);
    assert!(lookup.retained_identity);
    assert_eq!(
        lookup.profile.display_name,
        ProfileField::Available("Outside".into())
    );
    assert_eq!(
        lookup.profile.timestamp,
        ProfileField::Available(signed.timestamp())
    );
    assert!(exhaust(&db, "Outside").await?.rows.is_empty());
    edges(&db, &[(id(500), secret.id())]).await?;
    assert_eq!(exhaust(&db, "Outside").await?.rows[0].identity, secret.id());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn omitted_profile_warning_survives_continuation_and_bad_sources_are_explicit()
-> anyhow::Result<()> {
    let owner = RostraIdSecretKey::generate();
    let db = Database::new_in_memory(owner.id()).await?;
    let record = profile(&db, owner, "Hidden", 0).await?;
    let mut bytes = read::encoded(&record)?;
    bytes.push(0);
    raw_profile(&db, owner.id(), &bytes).await?;
    edges(
        &db,
        &(0..30).map(|n| (owner.id(), id(n))).collect::<Vec<_>>(),
    )
    .await?;
    let first = db.search_known_people("never-found", None).await?;
    assert!(first.rows.is_empty());
    assert_eq!(first.unscorable_profiles, 1);
    assert_eq!(first.stop, SearchStop::Incomplete);
    let mut cursor = first.next_cursor;
    loop {
        let page = db
            .search_known_people("never-found", cursor.as_ref())
            .await?;
        assert_eq!(page.unscorable_profiles, 1);
        if page.stop == SearchStop::Exhausted {
            break;
        }
        cursor = page.next_cursor;
    }
    // Malformed source bytes cannot produce a name or timestamp, even if the
    // projection itself is valid. The exact graph identity remains known.
    raw_profile(&db, owner.id(), &read::encoded(&record)?).await?;
    for bytes in [vec![0], vec![0; 1025]] {
        db.write_with(|tx| {
            tx.as_raw()
                .open_table(crate::events::TABLE.as_raw())?
                .insert(
                    read::encoded(record.inner.event_id)?.as_slice(),
                    bytes.as_slice(),
                )?;
            Ok(())
        })
        .await?;
        let lookup = db.lookup_known_person(owner.id(), None).await?;
        assert_eq!(lookup.graph_membership, GraphMembership::Known);
        assert_eq!(
            lookup.profile.display_name,
            ProfileField::Unavailable(ProfileIssue::Corrupt)
        );
        assert_eq!(
            lookup.profile.timestamp,
            ProfileField::Unavailable(ProfileIssue::Corrupt)
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn active_graph_keys_define_membership_without_decoding_selector_values() -> anyhow::Result<()>
{
    let owner = id(500);
    let db = Database::new_in_memory(owner).await?;
    for value in [vec![255], vec![0; PROFILE_BYTES_MAX + 1]] {
        db.write_with(|tx| {
            let mut table = tx
                .as_raw()
                .open_table(crate::ids_followees::TABLE.as_raw())?;
            for edge in [(owner, id(1)), (id(1), id(2))] {
                table.insert(read::encoded(edge)?.as_slice(), value.as_slice())?;
            }
            Ok(())
        })
        .await?;
        let page = exhaust(&db, "rs").await?;
        assert_eq!(
            page.rows
                .iter()
                .map(|r| r.identity)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([owner, id(1), id(2)])
        );
        for target in [id(1), id(2)] {
            assert_eq!(
                db.lookup_known_person(target, None).await?.graph_membership,
                GraphMembership::Known
            );
        }
    }
    Ok(())
}
