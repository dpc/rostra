use std::collections::BTreeSet;

use rostra_core::event::EventExt as _;
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{EventId, ShortEventId, Timestamp};

use crate::reaction_read::{self, Budget};
use crate::reaction_summary::{ReactionCursor, ReactionRow, ReactionSummary};
use crate::{Database, DbError, DbResult};

impl Database {
    /// Count all supported materialized reaction rows across canonical target
    /// history in one snapshot, then return an independently bounded page.
    ///
    /// This is not a globally-known or distinct-author count. Unavailable and
    /// unsupported rows are omitted, never inferred from stale aggregates.
    /// Work is bounded by 10000 raw rows, 4096 total lineage edges (1024 per
    /// direction), 16 MiB examined payload and 1 MB per payload. Any exceeded
    /// budget returns an error without partial counts or rows. Page size is
    /// 1..=100. Separate pages may observe concurrent changes.
    pub async fn reaction_summary(
        &self,
        target: (RostraId, EventId),
        cursor: Option<ReactionCursor>,
        limit: usize,
    ) -> DbResult<ReactionSummary> {
        if !(1..=100).contains(&limit) {
            return Err(DbError::ReactionPageLimit);
        }
        self.read_with(|tx| {
            let events = tx.open_table(&crate::events::TABLE)?;
            let states = tx.open_table(&crate::events_content_state::TABLE)?;
            let store = tx.open_table(&crate::content_store::TABLE)?;
            let replaces = tx.open_table(&crate::social_posts_replaces::TABLE)?;
            let replaced_by = tx.open_table(&crate::social_posts_replaced_by::TABLE)?;
            let index = tx.open_table(&crate::social_posts_reactions::TABLE)?;
            reaction_read::bound_event(target, &events)?;
            let mut budget = Budget::default();
            let versions = reaction_read::versions(
                target.0,
                target.1.to_short(),
                &events,
                &replaces,
                &replaced_by,
                &mut budget,
            )?;
            let mut result = ReactionSummary {
                total: 0,
                self_count: 0,
                rows: Vec::new(),
                next_cursor: None,
                raw_rows_scanned: 0,
            };
            let mut seen = BTreeSet::new();
            for version in versions {
                for entry in index.range(
                    &(version, Timestamp::ZERO, ShortEventId::ZERO)
                        ..=&(version, Timestamp::MAX, ShortEventId::MAX),
                )? {
                    result.raw_rows_scanned += 1;
                    if result.raw_rows_scanned > 10_000 {
                        return Err(DbError::ReactionScanLimit);
                    }
                    let (_, timestamp, id) = entry?.0.value();
                    let Some(event) = Self::get_event_tx(id, &events)? else {
                        continue;
                    };
                    if event.timestamp() != timestamp {
                        continue;
                    }
                    let full_id = event.event().compute_id();
                    match reaction_read::bound_event((event.author(), full_id), &events) {
                        Ok(_) => {}
                        Err(DbError::ReactionTarget) => continue,
                        Err(error) => return Err(error),
                    }
                    let post =
                        match reaction_read::payload(&event, &states, &store, &mut budget, true) {
                            Ok(post) => post,
                            Err(DbError::ReactionUnavailable | DbError::ReactionUnsupported) => {
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
                    if !post.reply_to.is_some_and(|reply| {
                        reply.rostra_id() == target.0 && reply.event_id() == version
                    }) {
                        continue;
                    }
                    let row_versions = match reaction_read::versions(
                        event.author(),
                        id,
                        &events,
                        &replaces,
                        &replaced_by,
                        &mut budget,
                    ) {
                        Ok(versions) => versions,
                        Err(
                            DbError::ReactionConflict
                            | DbError::ReactionTarget
                            | DbError::ReactionUnavailable,
                        ) => continue,
                        Err(error) => return Err(error),
                    };
                    if row_versions.last() != Some(&id) || !seen.insert(full_id) {
                        continue;
                    }
                    result.total += 1;
                    if event.author() == self.self_id {
                        result.self_count += 1;
                    }
                    let row_cursor = ReactionCursor {
                        timestamp,
                        event_id: full_id,
                    };
                    if cursor.is_none_or(|cursor| row_cursor < cursor) {
                        result.rows.push(ReactionRow {
                            author: event.author(),
                            cursor: row_cursor,
                            reaction: post
                                .reaction
                                .expect("Strict reaction decoder checked field"),
                        });
                    }
                }
            }
            result
                .rows
                .sort_unstable_by_key(|row| std::cmp::Reverse(row.cursor));
            if result.rows.len() > limit {
                result.rows.truncate(limit);
                result.next_cursor = result.rows.last().map(|row| row.cursor);
            }
            Ok(result)
        })
        .await
    }
}
