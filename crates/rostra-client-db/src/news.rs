use std::collections::BTreeSet;

use rostra_core::event::{EventAuxKey, EventExt as _, EventKind, content_kind};
use rostra_core::id::RostraId;
use rostra_core::{ExternalEventId, Timestamp};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::event::{EventSingletonRecord, SocialVoteProjection};
use crate::event_order::EventOrder;
use crate::social::SocialPostRecord;
use crate::{
    Database, DbResult, InvalidVoteSingletonProjectionSnafu, LOG_TARGET,
    MissingVoteSingletonProjectionSnafu, SocialNewsRankRecord, SocialVoteScore,
    SocialVoteSumRecord, content_store, events, events_content_state, events_singletons_new,
    social_news_rank_by_post_id, social_news_rank_by_score, social_news_rank_by_time, social_posts,
    social_vote_sums,
};

pub(crate) const NEWS_MAX_AGE_SECS: u64 = 4 * 365 * 24 * 60 * 60;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NewsRankPaginationCursor {
    pub score: SocialVoteScore,
    pub post_id: ExternalEventId,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NewsTimePaginationCursor {
    pub ts: Timestamp,
    pub post_id: ExternalEventId,
}

#[derive(Clone, Debug)]
pub struct NewsPostRecord<C> {
    pub post: SocialPostRecord<C>,
    pub post_id: ExternalEventId,
    pub creation_ts: Timestamp,
    pub score: SocialVoteScore,
    pub vote_sum: i64,
}

impl Database {
    pub(crate) fn notify_news_score_update_on_commit(
        &self,
        tx: &crate::WriteTransactionCtx,
        post_id: ExternalEventId,
    ) {
        let mut sender = self.news_score_updates_tx.clone();
        tx.on_commit(move || {
            sender.send(post_id);
        });
    }

    pub(crate) fn cap_creation_timestamp(ts: Timestamp, now: Timestamp) -> Timestamp {
        ts.min(now)
    }

    pub(crate) fn social_vote_aux_key(reply_to: ExternalEventId) -> EventAuxKey {
        EventAuxKey::from_bytes(reply_to.event_id().to_bytes())
    }

    fn validate_social_vote_projection(
        record: &EventSingletonRecord,
        aux_key: EventAuxKey,
    ) -> DbResult<SocialVoteProjection> {
        let Some(vote) = record.social_vote else {
            return MissingVoteSingletonProjectionSnafu {
                event_id: record.event_id,
            }
            .fail();
        };
        if Self::social_vote_aux_key(vote.target) != aux_key {
            return InvalidVoteSingletonProjectionSnafu {
                event_id: record.event_id,
            }
            .fail();
        }
        Ok(vote)
    }

    pub(crate) fn calculate_news_score(
        creation_ts: Timestamp,
        vote_sum: i64,
        now: Timestamp,
    ) -> SocialVoteScore {
        let hours_passed = now.secs_since(creation_ts) / 3600;
        let numerator = 10i128 + i128::from(vote_sum).saturating_mul(10_000);
        let denominator = i128::from(hours_passed) + 1;
        let score = numerator / denominator;
        score.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    /// Apply the aggregate half of a validated social-vote winner update.
    ///
    /// The caller must validate the singleton header/payload relationship and
    /// persist this exact projection as the paired winner later in the same
    /// transaction. Transaction rollback keeps both halves atomic on failure.
    pub(crate) fn process_social_vote_tx(
        &self,
        vote: SocialVoteProjection,
        author: RostraId,
        event_order: EventOrder,
        tx: &crate::WriteTransactionCtx,
    ) -> DbResult<()> {
        let singleton_key = (
            author,
            EventKind::SOCIAL_VOTE,
            Self::social_vote_aux_key(vote.target),
        );
        let singletons_table = tx.open_table(&events_singletons_new::TABLE)?;
        let existing_singleton = singletons_table.get(&singleton_key)?.map(|g| g.value());
        if existing_singleton.as_ref().is_some_and(|existing| {
            event_order <= EventOrder::new(existing.ts, existing.inner.event_id)
        }) {
            return Ok(());
        }

        let previous_vote = if let Some(existing) = existing_singleton {
            Some(Self::validate_social_vote_projection(
                &existing.inner,
                singleton_key.2,
            )?)
        } else {
            None
        };

        let adjustments = match previous_vote {
            Some(previous) if previous.target == vote.target => [
                Some((vote.target, vote.value.score() - previous.value.score())),
                None,
            ],
            Some(previous) => [
                Some((previous.target, -previous.value.score())),
                Some((vote.target, vote.value.score())),
            ],
            None => [Some((vote.target, vote.value.score())), None],
        };
        let mut changed_sum = false;
        for (target, delta) in adjustments.into_iter().flatten() {
            if delta == 0 {
                continue;
            }
            changed_sum = true;
            let mut vote_sums_table = tx.open_table(&social_vote_sums::TABLE)?;
            let mut record =
                vote_sums_table
                    .get(&target)?
                    .map(|g| g.value())
                    .unwrap_or(SocialVoteSumRecord {
                        last_vote_time: Timestamp::ZERO,
                        current_sum: 0,
                    });
            record.last_vote_time = record.last_vote_time.max(event_order.timestamp());
            record.current_sum = record.current_sum.saturating_add(delta);
            vote_sums_table.insert(&target, &record)?;
        }

        match previous_vote {
            Some(previous) if previous.target != vote.target => {
                self.notify_news_score_update_on_commit(tx, previous.target);
                self.notify_news_score_update_on_commit(tx, vote.target);
            }
            _ if changed_sum => self.notify_news_score_update_on_commit(tx, vote.target),
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn upsert_social_news_rank_tx(
        &self,
        post_id: ExternalEventId,
        creation_ts: Timestamp,
        score: SocialVoteScore,
        tx: &crate::WriteTransactionCtx,
    ) -> DbResult<()> {
        let mut rank_by_post_id_table = tx.open_table(&social_news_rank_by_post_id::TABLE)?;
        let mut rank_by_score_table = tx.open_table(&social_news_rank_by_score::TABLE)?;
        let mut rank_by_time_table = tx.open_table(&social_news_rank_by_time::TABLE)?;

        if let Some(existing) = rank_by_post_id_table.get(&post_id)?.map(|g| g.value()) {
            rank_by_score_table.remove(&(existing.score, post_id))?;
            rank_by_time_table.remove(&(existing.creation_ts, post_id))?;
        }

        let record = SocialNewsRankRecord { creation_ts, score };
        rank_by_post_id_table.insert(&post_id, &record)?;
        rank_by_score_table.insert(&(score, post_id), &())?;
        rank_by_time_table.insert(&(creation_ts, post_id), &())?;
        Ok(())
    }

    pub(crate) fn remove_social_news_rank_tx(
        post_id: ExternalEventId,
        tx: &crate::WriteTransactionCtx,
    ) -> DbResult<()> {
        let mut rank_by_post_id_table = tx.open_table(&social_news_rank_by_post_id::TABLE)?;
        let Some(existing) = rank_by_post_id_table.remove(&post_id)?.map(|g| g.value()) else {
            return Ok(());
        };

        let mut rank_by_score_table = tx.open_table(&social_news_rank_by_score::TABLE)?;
        let mut rank_by_time_table = tx.open_table(&social_news_rank_by_time::TABLE)?;
        rank_by_score_table.remove(&(existing.score, post_id))?;
        rank_by_time_table.remove(&(existing.creation_ts, post_id))?;
        Ok(())
    }

    pub async fn recalculate_news_post_score(&self, post_id: ExternalEventId) -> bool {
        let now = Timestamp::now();
        self.write_with(|tx| Self::recalculate_news_post_score_tx(post_id, now, tx))
            .await
            .expect("Storage error")
    }

    pub(crate) fn recalculate_news_post_score_tx(
        post_id: ExternalEventId,
        now: Timestamp,
        tx: &crate::WriteTransactionCtx,
    ) -> DbResult<bool> {
        let mut rank_by_post_id_table = tx.open_table(&social_news_rank_by_post_id::TABLE)?;
        let Some(existing) = rank_by_post_id_table.get(&post_id)?.map(|g| g.value()) else {
            return Ok(false);
        };

        if NEWS_MAX_AGE_SECS < now.secs_since(existing.creation_ts) {
            rank_by_post_id_table.remove(&post_id)?;
            let mut rank_by_score_table = tx.open_table(&social_news_rank_by_score::TABLE)?;
            let mut rank_by_time_table = tx.open_table(&social_news_rank_by_time::TABLE)?;
            rank_by_score_table.remove(&(existing.score, post_id))?;
            rank_by_time_table.remove(&(existing.creation_ts, post_id))?;
            return Ok(false);
        }

        let vote_sum = tx
            .open_table(&social_vote_sums::TABLE)?
            .get(&post_id)?
            .map(|g| g.value().current_sum)
            .unwrap_or(0);
        let score = Self::calculate_news_score(existing.creation_ts, vote_sum, now);
        if score == existing.score {
            return Ok(true);
        }

        let mut rank_by_score_table = tx.open_table(&social_news_rank_by_score::TABLE)?;
        let mut rank_by_time_table = tx.open_table(&social_news_rank_by_time::TABLE)?;
        rank_by_score_table.remove(&(existing.score, post_id))?;
        rank_by_time_table.remove(&(existing.creation_ts, post_id))?;

        let updated = SocialNewsRankRecord {
            creation_ts: existing.creation_ts,
            score,
        };
        rank_by_post_id_table.insert(&post_id, &updated)?;
        rank_by_score_table.insert(&(score, post_id), &())?;
        rank_by_time_table.insert(&(existing.creation_ts, post_id), &())?;
        Ok(true)
    }

    pub async fn get_random_news_post_ids(&self, limit: usize) -> Vec<ExternalEventId> {
        self.read_with(|tx| {
            let table = tx.open_table(&social_news_rank_by_post_id::TABLE)?;
            let mut post_ids = BTreeSet::new();
            let max_attempts = limit.saturating_mul(4).max(limit);

            for _ in 0..max_attempts {
                let Some(post_id) = Self::get_random_table_key(&table)? else {
                    break;
                };
                post_ids.insert(post_id);
                if limit <= post_ids.len() {
                    break;
                }
            }

            Ok(post_ids.into_iter().collect())
        })
        .await
        .expect("Storage error")
    }

    pub async fn get_social_vote_sum(&self, post_id: ExternalEventId) -> i64 {
        self.read_with(|tx| {
            Ok(tx
                .open_table(&social_vote_sums::TABLE)?
                .get(&post_id)?
                .map(|g| g.value().current_sum)
                .unwrap_or(0))
        })
        .await
        .expect("Storage error")
    }

    /// Return a voter's current projected vote for one full post identity.
    ///
    /// The outer `None` means this post is not the winner for its shortened
    /// singleton key. The inner `None` is a retained neutral vote. This query
    /// reads the inline current projection and does not require retained source
    /// payload bytes.
    ///
    /// # Panics
    ///
    /// Panics on storage failure or a vote singleton with a missing or invalid
    /// inline projection.
    pub async fn get_social_vote(
        &self,
        voter: RostraId,
        post_id: ExternalEventId,
    ) -> Option<Option<bool>> {
        self.read_with(|tx| {
            let singleton = tx
                .open_table(&events_singletons_new::TABLE)?
                .get(&(
                    voter,
                    EventKind::SOCIAL_VOTE,
                    Self::social_vote_aux_key(post_id),
                ))?
                .map(|g| g.value());
            let Some(singleton) = singleton else {
                return Ok(None);
            };
            let vote = Self::validate_social_vote_projection(
                &singleton.inner,
                Self::social_vote_aux_key(post_id),
            )?;
            if vote.target != post_id {
                return Ok(None);
            }
            Ok(Some(vote.value.as_upvote()))
        })
        .await
        .expect("Storage error")
    }

    /// Paginate news posts by rank score, newest cursor positions moving from
    /// highest score toward lower scores.
    pub async fn paginate_news_posts_by_rank_rev(
        &self,
        cursor: Option<NewsRankPaginationCursor>,
        limit: usize,
    ) -> (
        Vec<NewsPostRecord<content_kind::SocialPost>>,
        Option<NewsRankPaginationCursor>,
    ) {
        self.read_with(|tx| {
            let rank_by_score_table = tx.open_table(&social_news_rank_by_score::TABLE)?;
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_table = tx.open_table(&social_posts::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;
            let vote_sums_table = tx.open_table(&social_vote_sums::TABLE)?;
            let rank_by_post_id_table = tx.open_table(&social_news_rank_by_post_id::TABLE)?;

            let (ret, cursor) = Self::paginate_table_rev(
                &rank_by_score_table,
                cursor.map(|c| (c.score, c.post_id)),
                limit,
                move |(_score, post_id), _| {
                    Self::news_post_record_for_id_tx(
                        post_id,
                        &events_table,
                        &social_posts_table,
                        &events_content_state_table,
                        &content_store_table,
                        &vote_sums_table,
                        &rank_by_post_id_table,
                    )
                },
            )?;

            Ok((
                ret,
                cursor.map(|(score, post_id)| NewsRankPaginationCursor { score, post_id }),
            ))
        })
        .await
        .expect("Storage error")
    }

    /// Paginate news posts by capped creation timestamp, newest first.
    pub async fn paginate_news_posts_by_time_rev(
        &self,
        cursor: Option<NewsTimePaginationCursor>,
        limit: usize,
    ) -> (
        Vec<NewsPostRecord<content_kind::SocialPost>>,
        Option<NewsTimePaginationCursor>,
    ) {
        self.read_with(|tx| {
            let rank_by_time_table = tx.open_table(&social_news_rank_by_time::TABLE)?;
            let events_table = tx.open_table(&events::TABLE)?;
            let social_posts_table = tx.open_table(&social_posts::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;
            let vote_sums_table = tx.open_table(&social_vote_sums::TABLE)?;
            let rank_by_post_id_table = tx.open_table(&social_news_rank_by_post_id::TABLE)?;

            let (ret, cursor) = Self::paginate_table_rev(
                &rank_by_time_table,
                cursor.map(|c| (c.ts, c.post_id)),
                limit,
                move |(_ts, post_id), _| {
                    Self::news_post_record_for_id_tx(
                        post_id,
                        &events_table,
                        &social_posts_table,
                        &events_content_state_table,
                        &content_store_table,
                        &vote_sums_table,
                        &rank_by_post_id_table,
                    )
                },
            )?;

            Ok((
                ret,
                cursor.map(|(ts, post_id)| NewsTimePaginationCursor { ts, post_id }),
            ))
        })
        .await
        .expect("Storage error")
    }

    fn news_post_record_for_id_tx(
        post_id: ExternalEventId,
        events_table: &impl events::ReadableTable,
        social_posts_table: &impl social_posts::ReadableTable,
        events_content_state_table: &impl events_content_state::ReadableTable,
        content_store_table: &impl content_store::ReadableTable,
        vote_sums_table: &impl social_vote_sums::ReadableTable,
        rank_by_post_id_table: &impl social_news_rank_by_post_id::ReadableTable,
    ) -> DbResult<Option<NewsPostRecord<content_kind::SocialPost>>> {
        let Some(rank) = rank_by_post_id_table.get(&post_id)?.map(|g| g.value()) else {
            return Ok(None);
        };

        let Some((social_post, event, social_post_record)) = Self::get_social_post_record_tx(
            events_table,
            social_posts_table,
            events_content_state_table,
            content_store_table,
            post_id.event_id(),
        )?
        else {
            warn!(target: LOG_TARGET, %post_id, "Missing social post for news rank record");
            return Ok(None);
        };

        if !social_post.news {
            return Ok(None);
        }

        let vote_sum = vote_sums_table
            .get(&post_id)?
            .map(|g| g.value().current_sum)
            .unwrap_or(0);

        Ok(Some(NewsPostRecord {
            post: SocialPostRecord {
                ts: event.timestamp(),
                event_id: post_id.event_id(),
                author: event.author(),
                reply_to: social_post.reply_to,
                content: social_post,
                reply_count: social_post_record.reply_count,
            },
            post_id,
            creation_ts: rank.creation_ts,
            score: rank.score,
            vote_sum,
        }))
    }
}
