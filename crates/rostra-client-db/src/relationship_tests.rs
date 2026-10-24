use std::collections::BTreeSet;

use rostra_core::event::{PersonaId, PersonaSelector, PersonaTag, PersonasTagsSelector};
use rostra_core::id::RostraId;
use rostra_core::{ShortEventId, Timestamp};

use crate::relationship::{
    RELATIONSHIP_BYTES_MAX, RELATIONSHIP_BYTES_MIN, RELATIONSHIP_RECORD_BYTES_MAX,
    RelationshipBounds, RelationshipDirection, RelationshipIssue, RelationshipSelector,
    RelationshipState, RelationshipStop,
};
use crate::{Database, DbError, IdsFolloweesRecord, ids_followees, ids_followers};

fn id(n: u32) -> RostraId {
    let mut bytes = [0; 32];
    bytes[28..].copy_from_slice(&n.to_be_bytes());
    RostraId::from_bytes(bytes)
}

fn bounds(rows: usize) -> RelationshipBounds {
    RelationshipBounds {
        rows,
        bytes: RELATIONSHIP_BYTES_MAX,
    }
}

fn record(
    legacy: Option<PersonaSelector>,
    tags: Option<PersonasTagsSelector>,
) -> IdsFolloweesRecord {
    IdsFolloweesRecord::new(
        Timestamp::from(3),
        ShortEventId::from_bytes([1; 16]),
        Timestamp::from(2),
        legacy,
        tags,
    )
}

fn active(selector: PersonasTagsSelector) -> RelationshipState {
    RelationshipState::Following {
        selector: RelationshipSelector::Available(selector),
    }
}

fn unavailable() -> RelationshipState {
    RelationshipState::Following {
        selector: RelationshipSelector::Unavailable(RelationshipIssue::Oversized),
    }
}

async fn insert(
    db: &Database,
    owner: RostraId,
    target: RostraId,
    record: &IdsFolloweesRecord,
) -> anyhow::Result<()> {
    db.write_with(|tx| {
        tx.open_table(&ids_followees::TABLE)?
            .insert(&(owner, target), record)?;
        tx.open_table(&ids_followers::TABLE)?
            .insert(&(target, owner), &crate::ids::IdsFollowersRecord {})?;
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relationship_effective_selectors_and_exact_followback() -> anyhow::Result<()> {
    let db = Database::new_in_memory(id(50)).await?;
    let variants = [
        record(None, None),
        record(
            Some(PersonaSelector::Only {
                ids: vec![PersonaId(0), PersonaId(1), PersonaId(2), PersonaId(255)],
            }),
            None,
        ),
        record(
            Some(PersonaSelector::Except {
                ids: vec![PersonaId(1), PersonaId(7)],
            }),
            None,
        ),
        record(
            Some(PersonaSelector::Except { ids: vec![] }),
            Some(PersonasTagsSelector::Only {
                ids: BTreeSet::new(),
            }),
        ),
        record(
            None,
            Some(PersonasTagsSelector::Except {
                ids: BTreeSet::from([PersonaTag::new("custom")?]),
            }),
        ),
    ];
    for (n, entry) in variants.iter().enumerate() {
        insert(&db, id(50), id(n as u32), entry).await?;
        // Incoming filters deliberately disagree with ours.
        insert(
            &db,
            id(n as u32),
            id(50),
            &record(None, Some(PersonasTagsSelector::default())),
        )
        .await?;
    }
    insert(&db, id(49), id(50), &record(None, None)).await?;
    let following = db
        .get_self_relationships_page(RelationshipDirection::Following, None, bounds(100))
        .await?;
    assert_eq!(following.stop, RelationshipStop::End);
    assert_eq!(following.rows.len(), variants.len());
    assert_eq!(following.point_reads, 0);
    for (n, row) in following.rows.iter().enumerate() {
        assert_eq!(row.identity, id(n as u32));
        let expected = active(variants[n].effective_tags_selector());
        assert_eq!(row.own_follow, expected);
        assert_eq!(db.get_self_relationship(row.identity).await?, expected);
    }
    let followers = db
        .get_self_relationships_page(RelationshipDirection::Followers, None, bounds(100))
        .await?;
    assert_eq!(followers.point_reads, 6);
    assert_eq!(followers.rows[..5], following.rows);
    assert_eq!(
        followers.rows[5].own_follow,
        RelationshipState::NotFollowing
    );
    assert_eq!(
        db.get_self_relationship(id(999)).await?,
        RelationshipState::NotFollowing
    );
    // The exact point API never decodes unrelated damaged follow records.
    db.write_with(|tx| {
        let key = bincode::encode_to_vec((id(50), id(1000)), redb_bincode::BINCODE_CONFIG).unwrap();
        tx.as_raw()
            .open_table(ids_followees::TABLE.as_raw())?
            .insert(key.as_slice(), &[255][..])?;
        Ok(())
    })
    .await?;
    assert_eq!(
        db.get_self_relationship(id(0)).await?,
        following.rows[0].own_follow
    );
    assert!(db.get_self_relationship(id(1000)).await.is_err());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relationship_deep_pages_and_minimum_byte_budget_progress() -> anyhow::Result<()> {
    let owner = id(8000);
    let db = Database::new_in_memory(owner).await?;
    // Dense direct membership plus many unrelated owners. Traversal must seek
    // directly, rather than repeatedly scanning and truncating a full snapshot.
    db.write_with(|tx| {
        let mut table = tx.open_table(&ids_followees::TABLE)?;
        for n in 0..5000 {
            table.insert(&(owner, id(n)), &record(None, None))?;
            table.insert(&(id(n), id(9000)), &record(None, None))?;
        }
        Ok(())
    })
    .await?;
    let mut cursor = None;
    let mut found = Vec::new();
    loop {
        let page = db
            .get_self_relationships_page(
                RelationshipDirection::Following,
                cursor.as_ref(),
                bounds(37),
            )
            .await?;
        assert!(page.raw_rows_scanned <= 37);
        assert_eq!(page.raw_rows_scanned, page.rows.len());
        assert!(page.bytes_charged <= RELATIONSHIP_BYTES_MAX);
        found.extend(page.rows.iter().map(|row| row.identity));
        cursor = page.next_cursor;
        if cursor.is_none() {
            assert_eq!(page.stop, RelationshipStop::End);
            break;
        }
    }
    assert_eq!(found, (0..5000).map(id).collect::<Vec<_>>());
    let tiny = RelationshipBounds {
        rows: 100,
        bytes: RELATIONSHIP_BYTES_MIN,
    };
    let first = db
        .get_self_relationships_page(RelationshipDirection::Following, None, tiny)
        .await?;
    assert_eq!(first.rows.len(), 1);
    assert_eq!(first.stop, RelationshipStop::ByteBudget);
    let second = db
        .get_self_relationships_page(
            RelationshipDirection::Following,
            first.next_cursor.as_ref(),
            tiny,
        )
        .await?;
    assert_eq!(second.rows[0].identity, id(1));
    for invalid in [
        bounds(0),
        bounds(101),
        RelationshipBounds {
            bytes: RELATIONSHIP_BYTES_MIN - 1,
            ..tiny
        },
        RelationshipBounds {
            bytes: RELATIONSHIP_BYTES_MAX + 1,
            ..tiny
        },
    ] {
        assert!(matches!(
            db.get_self_relationships_page(RelationshipDirection::Following, None, invalid)
                .await,
            Err(DbError::RelationshipBounds)
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relationship_oversize_preserves_membership_and_forward_progress() -> anyhow::Result<()> {
    let db = Database::new_in_memory(id(50)).await?;
    let at_limit = record(
        Some(PersonaSelector::Only {
            ids: vec![PersonaId(0); 256],
        }),
        None,
    );
    let beyond_limit = record(
        Some(PersonaSelector::Only {
            ids: vec![PersonaId(0); 257],
        }),
        None,
    );
    let combined_limit = record(
        Some(PersonaSelector::Only {
            ids: vec![PersonaId(0); 256],
        }),
        Some(PersonasTagsSelector::Only {
            ids: BTreeSet::from([PersonaTag::personal()]),
        }),
    );
    for (n, entry) in [&at_limit, &beyond_limit, &combined_limit]
        .iter()
        .enumerate()
    {
        insert(&db, id(50), id(n as u32), entry).await?;
        insert(&db, id(n as u32), id(50), &record(None, None)).await?;
    }
    let raw_key = bincode::encode_to_vec((id(50), id(3)), redb_bincode::BINCODE_CONFIG)?;
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(ids_followees::TABLE.as_raw())?
            .insert(
                raw_key.as_slice(),
                vec![0; RELATIONSHIP_RECORD_BYTES_MAX + 1].as_slice(),
            )?;
        Ok(())
    })
    .await?;
    insert(&db, id(3), id(50), &record(None, None)).await?;
    insert(&db, id(50), id(4), &record(None, None)).await?;
    assert_eq!(
        db.get_self_relationship(id(0)).await?,
        active(at_limit.effective_tags_selector())
    );
    for n in 1..=3 {
        assert_eq!(db.get_self_relationship(id(n)).await?, unavailable());
    }
    for direction in [
        RelationshipDirection::Following,
        RelationshipDirection::Followers,
    ] {
        let mut cursor = None;
        let mut rows = Vec::new();
        loop {
            let page = db
                .get_self_relationships_page(direction, cursor.as_ref(), bounds(1))
                .await?;
            rows.extend(page.rows);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            rows[0].own_follow,
            active(at_limit.effective_tags_selector())
        );
        for row in &rows[1..4] {
            assert_eq!(row.own_follow, unavailable());
        }
        assert_eq!(
            rows.len(),
            if direction == RelationshipDirection::Following {
                5
            } else {
                4
            }
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relationship_raw_corruption_is_fallible_and_never_absence() -> anyhow::Result<()> {
    for direction in [
        RelationshipDirection::Following,
        RelationshipDirection::Followers,
    ] {
        for corruption in 0..5 {
            let db = Database::new_in_memory(id(50)).await?;
            insert(&db, id(50), id(1), &record(None, None)).await?;
            insert(&db, id(1), id(50), &record(None, None)).await?;
            let mut key = bincode::encode_to_vec((id(50), id(1)), redb_bincode::BINCODE_CONFIG)?;
            let mut value = if direction == RelationshipDirection::Following {
                bincode::encode_to_vec(record(None, None), redb_bincode::BINCODE_CONFIG)?
            } else {
                vec![]
            };
            match corruption {
                0 => {
                    key.pop();
                }
                1 => key.push(0),
                2 => value.push(0),
                3 => value = vec![255],
                // Malformed shortest owner-prefix key must not be skipped.
                4 => key.truncate(32),
                _ => unreachable!(),
            }
            db.write_with(|tx| {
                let table = if direction == RelationshipDirection::Following {
                    ids_followees::TABLE.as_raw()
                } else {
                    ids_followers::TABLE.as_raw()
                };
                tx.as_raw()
                    .open_table(table)?
                    .insert(key.as_slice(), value.as_slice())?;
                Ok(())
            })
            .await?;
            assert!(
                db.get_self_relationships_page(direction, None, bounds(100))
                    .await
                    .is_err()
            );
            if direction == RelationshipDirection::Following && (2..=3).contains(&corruption) {
                assert!(db.get_self_relationship(id(1)).await.is_err());
            }
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relationship_cursor_live_mutation_scope_and_reopen() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("relationships.redb");
    let owner = id(50);
    let db = Database::open(&path, owner).await?;
    for n in [1, 3, 5] {
        insert(&db, owner, id(n), &record(None, None)).await?;
    }
    let first = db
        .get_self_relationships_page(RelationshipDirection::Following, None, bounds(1))
        .await?;
    let cursor = first.next_cursor.unwrap();
    let cursor = serde_json::from_str(&serde_json::to_string(&cursor)?)?;
    insert(&db, owner, id(0), &record(None, None)).await?;
    insert(&db, owner, id(2), &record(None, None)).await?;
    db.write_with(|tx| {
        tx.open_table(&ids_followees::TABLE)?
            .remove(&(owner, id(3)))?;
        Ok(())
    })
    .await?;
    let next = db
        .get_self_relationships_page(RelationshipDirection::Following, Some(&cursor), bounds(100))
        .await?;
    assert_eq!(
        next.rows.iter().map(|r| r.identity).collect::<Vec<_>>(),
        vec![id(2), id(5)]
    );
    assert_eq!(next.stop, RelationshipStop::End);
    let refreshed = db
        .get_self_relationships_page(RelationshipDirection::Following, None, bounds(100))
        .await?;
    assert_eq!(refreshed.rows[0].identity, id(0));
    assert!(matches!(
        db.get_self_relationships_page(RelationshipDirection::Followers, Some(&cursor), bounds(1))
            .await,
        Err(DbError::RelationshipCursorStale)
    ));
    let other = Database::new_in_memory(id(51)).await?;
    assert!(matches!(
        other
            .get_self_relationships_page(RelationshipDirection::Following, Some(&cursor), bounds(1))
            .await,
        Err(DbError::RelationshipCursorStale)
    ));
    // Rebuild invalidates even if the same open Database instance survives.
    db.write_with(|tx| {
        tx.open_table(&crate::social_notification_epoch::TABLE)?
            .insert(&(), &[7; 16])?;
        Ok(())
    })
    .await?;
    assert!(matches!(
        db.get_self_relationships_page(RelationshipDirection::Following, Some(&cursor), bounds(1))
            .await,
        Err(DbError::RelationshipCursorStale)
    ));
    let fresh_cursor = db
        .get_self_relationships_page(RelationshipDirection::Following, None, bounds(1))
        .await?
        .next_cursor;
    drop(db);
    let reopened = Database::open(&path, owner).await?;
    assert!(matches!(
        reopened
            .get_self_relationships_page(
                RelationshipDirection::Following,
                fresh_cursor.as_ref(),
                bounds(1)
            )
            .await,
        Err(DbError::RelationshipCursorStale)
    ));
    assert_eq!(
        reopened.get_self_relationship(id(0)).await?,
        active(PersonasTagsSelector::default())
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relationship_followback_and_membership_share_read_snapshot() -> anyhow::Result<()> {
    let owner = id(50);
    let db = Database::new_in_memory(owner).await?;
    let old = PersonasTagsSelector::Only {
        ids: BTreeSet::new(),
    };
    insert(&db, id(1), owner, &record(None, None)).await?;
    insert(&db, owner, id(1), &record(None, Some(old.clone()))).await?;
    let tx = db.inner.begin_read()?;
    // Commit both membership and followback changes after the reader opened.
    insert(&db, owner, id(1), &record(None, None)).await?;
    insert(&db, id(2), owner, &record(None, None)).await?;
    let previous =
        db.relationship_page_tx(&tx, RelationshipDirection::Followers, None, bounds(100))?;
    assert_eq!(previous.rows.len(), 1);
    assert_eq!(previous.rows[0].own_follow, active(old));
    let current = db
        .get_self_relationships_page(RelationshipDirection::Followers, None, bounds(100))
        .await?;
    assert_eq!(current.rows.len(), 2);
    assert_eq!(
        current.rows[0].own_follow,
        active(PersonasTagsSelector::default())
    );
    assert_eq!(current.rows[1].own_follow, RelationshipState::NotFollowing);
    Ok(())
}

/// Encode the stored record layout with borrowed strings, bypassing
/// PersonaTag's constructor to exercise historical strings and adversarial
/// length encodings.
fn raw_tags(tags: &[&str]) -> Vec<u8> {
    bincode::encode_to_vec(
        (
            Timestamp::from(3),
            ShortEventId::from_bytes([1; 16]),
            Timestamp::from(2),
            None::<PersonaSelector>,
            Some((0_u32, tags)),
        ),
        redb_bincode::BINCODE_CONFIG,
    )
    .unwrap()
}

async fn raw_own(db: &Database, bytes: &[u8]) -> anyhow::Result<()> {
    let key = bincode::encode_to_vec((id(50), id(1)), redb_bincode::BINCODE_CONFIG)?;
    db.write_with(|tx| {
        tx.as_raw()
            .open_table(ids_followees::TABLE.as_raw())?
            .insert(key.as_slice(), bytes)?;
        Ok(())
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relationship_encoded_byte_and_tag_element_boundaries() -> anyhow::Result<()> {
    let db = Database::new_in_memory(id(50)).await?;
    insert(&db, id(1), id(50), &record(None, None)).await?;
    // Record limit applies before any owned String decode. Legacy stored strings
    // are preserved exactly, not silently normalized or dropped by this reader.
    let overhead = raw_tags(&[""]).len() + 2; // varint string length grows to u16.
    let tag = "X".repeat(RELATIONSHIP_RECORD_BYTES_MAX - overhead);
    let at_limit = raw_tags(&[&tag]);
    assert_eq!(at_limit.len(), RELATIONSHIP_RECORD_BYTES_MAX);
    raw_own(&db, &at_limit).await?;
    let exact = db.get_self_relationship(id(1)).await?;
    let RelationshipState::Following {
        selector: RelationshipSelector::Available(PersonasTagsSelector::Only { ids }),
    } = exact
    else {
        panic!("at-limit historical selector must be available");
    };
    assert_eq!(ids.first().unwrap().as_str(), tag);
    let followers = db
        .get_self_relationships_page(
            RelationshipDirection::Followers,
            None,
            RelationshipBounds {
                rows: 100,
                bytes: RELATIONSHIP_BYTES_MIN,
            },
        )
        .await?;
    assert_eq!(followers.bytes_charged, RELATIONSHIP_BYTES_MIN);
    assert_eq!(followers.stop, RelationshipStop::ByteBudget);
    raw_own(&db, &raw_tags(&[&(tag + "X")])).await?;
    assert_eq!(db.get_self_relationship(id(1)).await?, unavailable());

    // Bound encoded elements, even if BTreeSet decoding would deduplicate them.
    raw_own(&db, &raw_tags(&["a"; 256])).await?;
    assert_eq!(
        db.get_self_relationship(id(1)).await?,
        active(PersonasTagsSelector::Only {
            ids: BTreeSet::from([PersonaTag::new("a")?]),
        })
    );
    raw_own(&db, &raw_tags(&["a"; 257])).await?;
    assert_eq!(db.get_self_relationship(id(1)).await?, unavailable());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relationship_untrusted_selector_lengths_and_discriminants() -> anyhow::Result<()> {
    let db = Database::new_in_memory(id(50)).await?;
    insert(&db, id(1), id(50), &record(None, None)).await?;
    let empty = raw_tags(&[]);
    let count_offset = empty.len() - 1;
    let mut huge_count = empty[..count_offset].to_vec();
    huge_count.extend(bincode::encode_to_vec(
        u64::MAX,
        redb_bincode::BINCODE_CONFIG,
    )?);
    raw_own(&db, &huge_count).await?;
    // Policy limits are checked without trying to allocate or certify the tail.
    assert_eq!(db.get_self_relationship(id(1)).await?, unavailable());
    let mut huge_string = empty[..count_offset].to_vec();
    huge_string.push(1);
    huge_string.extend(bincode::encode_to_vec(
        u64::MAX,
        redb_bincode::BINCODE_CONFIG,
    )?);
    let mut bad_option = empty.clone();
    bad_option[count_offset - 2] = 2;
    let mut bad_variant = empty.clone();
    bad_variant[count_offset - 1] = 2;
    let mut bad_utf8 = raw_tags(&["a"]);
    *bad_utf8.last_mut().unwrap() = 255;
    let mut trailing = empty.clone();
    trailing.push(0);
    let mut truncated = raw_tags(&["a"]);
    truncated.pop();
    for bad in [
        huge_string,
        bad_option,
        bad_variant,
        bad_utf8,
        trailing,
        truncated,
    ] {
        raw_own(&db, &bad).await?;
        assert!(db.get_self_relationship(id(1)).await.is_err());
        for direction in [
            RelationshipDirection::Following,
            RelationshipDirection::Followers,
        ] {
            assert!(
                db.get_self_relationships_page(direction, None, bounds(100))
                    .await
                    .is_err()
            );
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn relationship_full_identity_boundaries_and_adjacent_owner_corruption() -> anyhow::Result<()>
{
    for owner in [RostraId::ZERO, RostraId::MAX] {
        let db = Database::new_in_memory(owner).await?;
        for target in [RostraId::ZERO, RostraId::MAX] {
            insert(&db, owner, target, &record(None, None)).await?;
        }
        let foreign = if owner == RostraId::ZERO {
            id(1)
        } else {
            id(49)
        };
        db.write_with(|tx| {
            let key =
                bincode::encode_to_vec((foreign, id(1)), redb_bincode::BINCODE_CONFIG).unwrap();
            tx.as_raw()
                .open_table(ids_followees::TABLE.as_raw())?
                .insert(key.as_slice(), &[255][..])?;
            Ok(())
        })
        .await?;
        let first = db
            .get_self_relationships_page(RelationshipDirection::Following, None, bounds(1))
            .await?;
        assert_eq!(first.rows[0].identity, RostraId::ZERO);
        let last = db
            .get_self_relationships_page(
                RelationshipDirection::Following,
                first.next_cursor.as_ref(),
                bounds(1),
            )
            .await?;
        assert_eq!(last.rows[0].identity, RostraId::MAX);
        let end = db
            .get_self_relationships_page(
                RelationshipDirection::Following,
                last.next_cursor.as_ref(),
                bounds(1),
            )
            .await?;
        assert_eq!(end.stop, RelationshipStop::End);
        assert!(end.rows.is_empty());
        assert!(end.next_cursor.is_none());
    }
    Ok(())
}
