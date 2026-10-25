//! Raw graph keys, bounded nested traversal, and exact full-identity evidence.

use std::ops::Bound;

use redb_bincode::ReadTransaction;
use rostra_core::id::RostraId;

use super::{
    GRAPH_STEPS_MAX, GraphMembership, KnownPeopleCursor, KnownPeoplePage, KnownPerson,
    KnownPersonLookup, KnownPersonLookupCursor, PROFILES_MAX, Position, ProfileField, ProfileIssue,
    QUERY_BYTES_MAX, RESULTS_MAX, Scope, SearchStop, fuzzy_match, profile,
};
use crate::{Database, DbError, DbResult, ids_followees};

pub(super) fn encoded<T: bincode::Encode>(value: T) -> DbResult<Vec<u8>> {
    bincode::encode_to_vec(value, redb_bincode::BINCODE_CONFIG)
        .map_err(|_| DbError::KnownPeopleInvariant)
}

type Graph = redb_bincode::ReadOnlyTable<ids_followees::Key, ids_followees::Value>;

fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    let index = end.iter().rposition(|byte| *byte != u8::MAX)?;
    end[index] += 1;
    end.truncate(index + 1);
    Some(end)
}

fn next(graph: &Graph, owner: RostraId, after: Option<RostraId>) -> DbResult<Option<RostraId>> {
    let prefix = encoded(owner)?;
    let end = prefix_end(&prefix);
    let after = after.map(|id| encoded((owner, id))).transpose()?;
    let start = after
        .as_deref()
        .map_or(Bound::Included(prefix.as_slice()), Bound::Excluded);
    let mut range = graph.as_raw().range::<&[u8]>((
        start,
        end.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
    ))?;
    let Some(entry) = range.next() else {
        return Ok(None);
    };
    let (key, _value) = entry?;
    if key.value().len() != 64 {
        return Err(DbError::KnownPeopleInvariant);
    }
    let ((actual_owner, identity), consumed): ((RostraId, RostraId), _) =
        bincode::decode_from_slice(key.value(), redb_bincode::BINCODE_CONFIG)?;
    if consumed != 64 || actual_owner != owner || encoded((owner, identity))? != key.value() {
        return Err(DbError::KnownPeopleInvariant);
    }
    Ok(Some(identity))
}

fn follows(graph: &Graph, owner: RostraId, target: RostraId) -> DbResult<bool> {
    // Active-index key presence defines membership. Selector values (potentially
    // huge) do not change membership and are deliberately neither read nor decoded.
    Ok(graph
        .as_raw()
        .get(encoded((owner, target))?.as_slice())?
        .is_some())
}

impl Database {
    fn people_scope(&self, tx: &ReadTransaction) -> DbResult<Scope> {
        let epoch = tx
            .open_table(&crate::social_notification_epoch::TABLE)?
            .get(&())?
            .ok_or(DbError::KnownPeopleInvariant)?
            .value_try()?;
        Ok(Scope {
            account: self.self_id,
            instance: self.news_instance,
            epoch,
        })
    }

    /// Search self, direct followees and their direct followees, locally only.
    ///
    /// Rejects queries over 256 UTF-8 bytes before allocation, then applies the
    /// web's Unicode lowercase (no trimming, normalization or tokenization).
    /// Each call performs at most 128 graph advances and eight profile reads;
    /// each profile admits at most 2 MiB borrowed bytes plus a 1024-byte source
    /// envelope, and owns at most 1024 name bytes. Graph values are not
    /// decoded: active key presence defines membership regardless of
    /// persona selectors. Raw malformed graph keys fail explicitly rather
    /// than panic or skip.
    ///
    /// Duplicate paths are revisited, but only the fixed top ten is
    /// accumulated. In an unchanged graph with scorable profiles this
    /// yields exactly the web top ten on exhaustion, without an unbounded
    /// visited set or depth cap. See module documentation for live-mutation
    /// and coverage limitations.
    pub async fn search_known_people(
        &self,
        query: &str,
        cursor: Option<&KnownPeopleCursor>,
    ) -> DbResult<KnownPeoplePage> {
        if query.len() > QUERY_BYTES_MAX {
            return Err(DbError::KnownPeopleQuery);
        }
        let query = query.to_lowercase();
        self.read_with(|tx| {
            let scope = self.people_scope(tx)?;
            if let Some(cursor) = cursor
                && (cursor.scope != scope || cursor.query != query)
            {
                return Err(DbError::KnownPeopleCursorStale);
            }
            let mut cursor = cursor.cloned().unwrap_or(KnownPeopleCursor {
                scope,
                query,
                position: Position::SelfIdentity,
                best: Vec::new(),
                unscorable_profiles: 0,
            });
            let graph = tx.open_table(&ids_followees::TABLE)?;
            let mut graph_steps = 0;
            let mut profiles_read = 0;
            let mut stop = SearchStop::Incomplete;
            while graph_steps < GRAPH_STEPS_MAX && profiles_read < PROFILES_MAX {
                graph_steps += 1;
                let candidate = match cursor.position {
                    Position::SelfIdentity => {
                        cursor.position = Position::Outer(None);
                        Some(self.self_id)
                    }
                    Position::Outer(after) => match next(&graph, self.self_id, after)? {
                        Some(owner) => {
                            cursor.position = Position::Inner { owner, after: None };
                            Some(owner)
                        }
                        None => {
                            stop = SearchStop::Exhausted;
                            break;
                        }
                    },
                    Position::Inner { owner, after } => {
                        // If this outer edge disappeared between calls, do not
                        // traverse its remaining children. Previous observations
                        // remain explicitly live/stale until caller refreshes.
                        if !follows(&graph, self.self_id, owner)? {
                            cursor.position = Position::Outer(Some(owner));
                            continue;
                        }
                        match next(&graph, owner, after)? {
                            Some(identity) => {
                                cursor.position = Position::Inner {
                                    owner,
                                    after: Some(identity),
                                };
                                Some(identity)
                            }
                            None => {
                                cursor.position = Position::Outer(Some(owner));
                                None
                            }
                        }
                    }
                };
                let Some(identity) = candidate else { continue };
                profiles_read += 1;
                let profile = profile::read(tx, identity)?;
                if matches!(
                    profile.display_name,
                    ProfileField::Unavailable(issue) if issue != ProfileIssue::Missing
                ) {
                    cursor.unscorable_profiles = cursor.unscorable_profiles.saturating_add(1);
                }
                let id_string = identity.to_string();
                let display_name = match &profile.display_name {
                    ProfileField::Available(name) => name.clone(),
                    ProfileField::Unavailable(_) => format!("{}...", &id_string[..8]),
                };
                let score = fuzzy_match(&cursor.query, &display_name.to_lowercase())
                    .max(fuzzy_match(&cursor.query, &id_string.to_lowercase()));
                // Re-observation replaces that identity rather than accumulating
                // duplicates. Mutation can still invalidate previously discarded
                // candidates; no snapshot ranking claim is made.
                cursor.best.retain(|person| person.identity != identity);
                if score > 0 {
                    cursor.best.push(KnownPerson {
                        identity,
                        display_name,
                        profile,
                        score,
                    });
                    cursor.best.sort_by(|a, b| {
                        b.score
                            .cmp(&a.score)
                            .then_with(|| a.identity.cmp(&b.identity))
                    });
                    cursor.best.truncate(RESULTS_MAX);
                }
            }
            Ok(KnownPeoplePage {
                rows: cursor.best.clone(),
                unscorable_profiles: cursor.unscorable_profiles,
                next_cursor: (stop == SearchStop::Incomplete).then_some(cursor),
                stop,
                graph_steps,
                profiles_read,
            })
        })
        .await
    }

    /// Look up an exact full identity without conflating profile/index/graph
    /// state.
    ///
    /// Self and the exact direct edge are checked first, independent of the
    /// retained-author index. Then at most 128 direct followees are examined
    /// with one exact second-hop point lookup each; no whole collection clone.
    /// Unknown means absent from this completed live graph traversal, never
    /// nonexistent globally. Retained-author evidence outside the graph remains
    /// separate. Profile metadata uses the same independent bounded reader.
    pub async fn lookup_known_person(
        &self,
        identity: RostraId,
        cursor: Option<&KnownPersonLookupCursor>,
    ) -> DbResult<KnownPersonLookup> {
        self.read_with(|tx| {
            let scope = self.people_scope(tx)?;
            if let Some(cursor) = cursor
                && (cursor.scope != scope || cursor.target != identity)
            {
                return Err(DbError::KnownPeopleCursorStale);
            }
            let graph = tx.open_table(&ids_followees::TABLE)?;
            let mut after = cursor.and_then(|cursor| cursor.after);
            let mut membership = GraphMembership::Incomplete;
            let mut graph_steps = 0;
            if identity == self.self_id || follows(&graph, self.self_id, identity)? {
                membership = GraphMembership::Known;
            } else {
                while graph_steps < GRAPH_STEPS_MAX {
                    graph_steps += 1;
                    let Some(owner) = next(&graph, self.self_id, after)? else {
                        membership = GraphMembership::Unknown;
                        break;
                    };
                    after = Some(owner);
                    if follows(&graph, owner, identity)? {
                        membership = GraphMembership::Known;
                        break;
                    }
                }
            }
            Ok(KnownPersonLookup {
                identity,
                graph_membership: membership,
                retained_identity: crate::ids_full::contains_exact(tx, identity)?,
                profile: profile::read(tx, identity)?,
                next_cursor: (membership == GraphMembership::Incomplete).then_some(
                    KnownPersonLookupCursor {
                        scope,
                        target: identity,
                        after,
                    },
                ),
                graph_steps,
            })
        })
        .await
    }
}
