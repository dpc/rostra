//! Generation-bound traversal without retaining database snapshots between
//! calls.

use std::ops::Bound;

use rostra_core::EventId;
use rostra_core::event::{EventExt as _, EventKind, SocialPost};
use rostra_core::id::{RostraId, ToShort as _};
use serde::{Deserialize, Serialize};

use super::{NewsPostRecord, NewsRankPaginationCursor};
use crate::{Database, DbError, DbResult};

/// Opaque process-scoped continuation. Serialization is for transport, not
/// durable storage; reopen/rebuild invalidate it. Do not edit its fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewsFeedCursor {
    /// Database-open incarnation.
    instance: [u8; 16],
    /// Rebuild incarnation and checked monotonic rank revision.
    generation: ([u8; 16], u64),
    /// Last consumed raw row, exclusive in descending rank order.
    position: NewsRankPaginationCursor,
}

/// Fully bound news row and metadata sampled in this page's read transaction.
#[derive(Clone, Debug)]
pub struct NewsFeedRow {
    /// Exact retained full author and event ID, suitable for a vote request.
    pub target: (RostraId, EventId),
    /// Supported content and current rank/vote-count metadata.
    pub post: NewsPostRecord<SocialPost>,
    /// Current account's up/down choice; absent and neutral both mean clear.
    pub self_vote: Option<bool>,
}

/// Why a bounded scan page stopped; only End means exhaustion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NewsFeedStop {
    /// No raw index entries remain in this read transaction.
    End,
    /// The requested number of supported rows was returned.
    PageLimit,
    /// The raw-entry budget was consumed; continue even with zero rows.
    ScanBudget,
    /// The byte budget cannot fit the next row; it remains unconsumed.
    PayloadBudget,
}

/// One bounded scan page; no cursor means true exhaustion in this transaction.
#[derive(Clone, Debug)]
pub struct NewsFeedPage {
    /// Rows in descending `(score, ExternalEventId)` order.
    pub rows: Vec<NewsFeedRow>,
    /// More raw scanning remains, not necessarily another supported row.
    pub next_cursor: Option<NewsFeedCursor>,
    /// Why scanning stopped; empty non-End pages still make progress.
    pub stop: NewsFeedStop,
    /// Consumed raw entries, including unsupported entries but excluding the
    /// single unconsumed boundary probe.
    pub raw_rows_scanned: usize,
    /// Entries outside this interface: missing/unavailable, identity mismatch,
    /// unsupported shape, or oversized payload.
    pub unsupported_rows: usize,
    /// Subset of unsupported_rows whose signed encoded size exceeds 1 MB.
    pub oversized_rows: usize,
}

impl Database {
    pub(crate) fn advance_news_generation_tx(tx: &crate::WriteTransactionCtx) -> DbResult<()> {
        let mut table = tx.open_table(&crate::social_news_generation::TABLE)?;
        let (epoch, revision) = table
            .get(&())?
            .map(|row| row.value())
            .unwrap_or_else(|| (rand::random(), 0));
        let revision = revision
            .checked_add(1)
            .ok_or(DbError::NewsGenerationExhausted)?;
        table.insert(&(), &(epoch, revision))?;
        Ok(())
    }

    /// Traverse arbitrarily deep ranked news while membership/order is stable.
    /// Each call validates the cursor and reads rows in one transaction. A rank
    /// change, materialization, deletion, pruning, rebuild or reopen can return
    /// `NewsCursorStale` with no rows. Keep previously loaded pages and refresh
    /// explicitly; a busy feed can indefinitely prevent completing traversal.
    ///
    /// This is not a frozen content/count snapshot across pages. Unrelated
    /// writes and metadata-only updates do not expire cursors. Bounds are
    /// 1..=100 output rows, 10000 consumed raw rows plus one boundary probe,
    /// 1 MB per supported payload, and 16 MiB encoded payload per call.
    /// Work-budget stops return a progressing raw cursor, possibly with zero
    /// rows. Oversized historical payloads are outside this interface's
    /// supported policy and explicitly counted.
    pub async fn paginate_news_ranked(
        &self,
        cursor: Option<NewsFeedCursor>,
        limit: usize,
    ) -> DbResult<NewsFeedPage> {
        if !(1..=100).contains(&limit) {
            return Err(DbError::NewsPageLimit);
        }
        self.read_with(|tx| {
            let generation = tx
                .open_table(&crate::social_news_generation::TABLE)?
                .get(&())?
                .map(|row| row.value())
                .unwrap_or_default();
            if cursor.as_ref().is_some_and(|cursor| {
                cursor.instance != self.news_instance || cursor.generation != generation
            }) {
                return Err(DbError::NewsCursorStale);
            }
            let ranks = tx.open_table(&crate::social_news_rank_by_score::TABLE)?;
            let events = tx.open_table(&crate::events::TABLE)?;
            let posts = tx.open_table(&crate::social_posts::TABLE)?;
            let states = tx.open_table(&crate::events_content_state::TABLE)?;
            let store = tx.open_table(&crate::content_store::TABLE)?;
            let sums = tx.open_table(&crate::social_vote_sums::TABLE)?;
            let by_id = tx.open_table(&crate::social_news_rank_by_post_id::TABLE)?;
            let singletons = tx.open_table(&crate::events_singletons_new::TABLE)?;
            let end = cursor.as_ref().map_or(Bound::Unbounded, |cursor| {
                Bound::Excluded((
                    Self::news_score_key(cursor.position.score),
                    cursor.position.post_id,
                ))
            });
            let mut rows = Vec::new();
            let mut raw_rows_scanned = 0;
            let mut unsupported_rows = 0;
            let mut oversized_rows = 0;
            let mut bytes = 0;
            let mut last = None;
            let mut stop = NewsFeedStop::End;
            for entry in ranks.range((Bound::Unbounded, end))?.rev() {
                if rows.len() == limit {
                    stop = NewsFeedStop::PageLimit;
                    break;
                }
                if raw_rows_scanned == 10000 {
                    stop = NewsFeedStop::ScanBudget;
                    break;
                }
                let (score_key, post_id) = entry?.0.value();
                let score = Self::news_score_from_key(score_key);
                let event = Self::get_event_tx(post_id.event_id(), &events)?;
                if event.as_ref().is_some_and(|event| {
                    event.content_len() <= 1_000_000
                        && bytes + event.content_len() as usize > 16 * 1024 * 1024
                }) {
                    stop = NewsFeedStop::PayloadBudget;
                    break;
                }
                raw_rows_scanned += 1;
                last = Some(NewsFeedCursor {
                    instance: self.news_instance,
                    generation,
                    position: NewsRankPaginationCursor { score, post_id },
                });
                let Some(event) = event else {
                    unsupported_rows += 1;
                    continue;
                };
                if event.content_len() > 1_000_000 {
                    unsupported_rows += 1;
                    oversized_rows += 1;
                    continue;
                }
                if event.author() != post_id.rostra_id()
                    || event.event().compute_id().to_short() != post_id.event_id()
                {
                    unsupported_rows += 1;
                    continue;
                }
                let content = match Self::supported_news_tx(&event, &states, &store, &mut bytes) {
                    Ok(post) => post,
                    Err(
                        DbError::NewsTarget | DbError::NewsUnavailable | DbError::NewsUnsupported,
                    ) => {
                        unsupported_rows += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let Some(rank) = by_id.get(&post_id)?.map(|row| row.value()) else {
                    unsupported_rows += 1;
                    continue;
                };
                if rank.score != score {
                    return Err(DbError::NewsCandidate);
                }
                let vote_sum = sums
                    .get(&post_id)?
                    .map(|row| row.value().current_sum)
                    .unwrap_or(0);
                let reply_count = posts
                    .get(&post_id.event_id())?
                    .map(|row| row.value().reply_count)
                    .unwrap_or(0);
                let aux_key = Self::social_vote_aux_key(post_id);
                let self_vote = singletons
                    .get(&(self.self_id, EventKind::SOCIAL_VOTE, aux_key))?
                    .map(|row| Self::validate_social_vote_projection(&row.value().inner, aux_key))
                    .transpose()?
                    .filter(|vote| vote.target == post_id)
                    .and_then(|vote| vote.value.as_upvote());
                rows.push(NewsFeedRow {
                    target: (event.author(), event.event().compute_id()),
                    post: NewsPostRecord {
                        post: crate::social::SocialPostRecord {
                            ts: event.timestamp(),
                            event_id: post_id.event_id(),
                            author: event.author(),
                            reply_to: content.reply_to,
                            content,
                            reply_count,
                        },
                        post_id,
                        creation_ts: rank.creation_ts,
                        score,
                        vote_sum,
                    },
                    self_vote,
                });
            }
            Ok(NewsFeedPage {
                rows,
                next_cursor: if stop == NewsFeedStop::End {
                    None
                } else {
                    last
                },
                stop,
                raw_rows_scanned,
                unsupported_rows,
                oversized_rows,
            })
        })
        .await
    }
}
