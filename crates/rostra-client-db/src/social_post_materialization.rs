use std::num::NonZeroUsize;

use bincode::{Decode, Encode};
use rostra_core::event::{EventExt as _, EventKind, content_kind};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use serde::{Deserialize, Serialize};
use snafu::OptionExt as _;

use crate::event::ContentStoreRecord;
use crate::{
    Database, DbResult, EventContentState, OverflowSnafu, WriteTransactionCtx, content_store,
    events, events_content_state, social_post_materializations, social_posts_replaced_by,
};

/// Maximum number of materialization rows resolved by one scan.
pub const SOCIAL_POST_MATERIALIZATION_SCAN_MAX: usize = 4_096;

/// Opaque durable position in one database lineage's SocialPost materialization
/// feed.
///
/// Persist this value only after durably handling every item in its page, then
/// pass it back to resume. A cursor belongs to the database lineage that issued
/// it. A faithful backup carrying the same feed prefix retains its meaning;
/// divergent copies and unrelated databases do not. Restoring a database older
/// than the cursor fails as out of range.
#[derive(
    Clone, Copy, Debug, Decode, Deserialize, Encode, Eq, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct SocialPostMaterializationCursor(u64);

/// Current high-level state of one logged ordinary SocialPost materialization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SocialPostMaterialization {
    /// The materialized post remains current and its validated content is
    /// available.
    Present {
        /// Immutable author and event identity.
        post_id: ExternalEventId,
        /// Author-provided creation timestamp.
        authored_at: Timestamp,
        /// Currently available validated post content.
        content: Box<content_kind::SocialPost>,
    },
    /// The post was later deleted, pruned, or replaced.
    Removed {
        /// Immutable author and event identity of the original occurrence.
        post_id: ExternalEventId,
    },
}

/// One bounded snapshot page from the ordinary SocialPost materialization feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocialPostMaterializationPage {
    /// Occurrences resolved against current state in the page's read snapshot.
    pub items: Vec<SocialPostMaterialization>,
    /// Position acknowledging every occurrence before this cursor.
    pub scanned_through: SocialPostMaterializationCursor,
    /// Whether the same read snapshot contained no later occurrence.
    pub exhausted: bool,
}

impl Database {
    pub(crate) fn append_social_post_materialization_tx(
        event_id: ShortEventId,
        tx: &WriteTransactionCtx,
    ) -> DbResult<()> {
        if !tx.materialization_emission_enabled() {
            return Ok(());
        }

        let mut table = tx.open_table(&social_post_materializations::TABLE)?;
        let next = table
            .last()?
            .map(|entry| entry.0.value_try())
            .transpose()?
            .map(|last| last.checked_add(1).context(OverflowSnafu))
            .transpose()?
            .unwrap_or(0);
        if next == u64::MAX {
            return OverflowSnafu.fail();
        }
        table.insert(&next, &event_id)?;
        Ok(())
    }

    /// Scans ordinary SocialPost materializations in commit order.
    ///
    /// `after` is an opaque position acknowledging every earlier occurrence.
    /// `None` starts at this database lineage's cutover position zero. The
    /// schema upgrade does not backfill posts materialized by older code.
    ///
    /// Each logged identity is resolved against one current-state read
    /// snapshot. Later deletion, pruning, or replacement yields
    /// [`SocialPostMaterialization::Removed`]; a currently available post
    /// yields [`SocialPostMaterialization::Present`]. Log rows remain
    /// append-only either way. `limit` bounds rows resolved, and the
    /// implementation performs at most one additional key lookahead to
    /// determine snapshot-relative exhaustion.
    ///
    /// A replaced post is still required to have a coherent lifecycle. If that
    /// lifecycle claims the content remains processed, retained bytes must be
    /// present and decodable. Corruption takes precedence over `Removed`.
    ///
    /// # Errors
    ///
    /// Returns no page or acknowledgment on storage, decode, cursor, density,
    /// or current-state invariant errors.
    pub async fn scan_social_post_materializations(
        &self,
        after: Option<SocialPostMaterializationCursor>,
        limit: NonZeroUsize,
    ) -> DbResult<SocialPostMaterializationPage> {
        if SOCIAL_POST_MATERIALIZATION_SCAN_MAX < limit.get() {
            return Err(crate::DbError::SocialPostMaterializationScanLimitTooHigh {
                requested: limit.get(),
                maximum: SOCIAL_POST_MATERIALIZATION_SCAN_MAX,
            });
        }
        self.read_with(|tx| {
            let feed = tx.open_table(&social_post_materializations::TABLE)?;
            let durable_next = feed
                .last()?
                .map(|entry| entry.0.value_try())
                .transpose()?
                .map(|last| last.checked_add(1).context(OverflowSnafu))
                .transpose()?
                .unwrap_or(0);
            let mut expected = after.map_or(0, |cursor| cursor.0);
            if durable_next < expected {
                return crate::SocialPostMaterializationCursorOutOfRangeSnafu {
                    position: expected,
                    durable_next,
                }
                .fail();
            }

            let events_table = tx.open_table(&events::TABLE)?;
            let states = tx.open_table(&events_content_state::TABLE)?;
            let content = tx.open_table(&content_store::TABLE)?;
            let replacements = tx.open_table(&social_posts_replaced_by::TABLE)?;
            let mut range = feed.range(expected..)?;
            let mut items = Vec::with_capacity(limit.get());

            for _ in 0..limit.get() {
                let Some(entry) = range.next() else {
                    return Ok(SocialPostMaterializationPage {
                        items,
                        scanned_through: SocialPostMaterializationCursor(expected),
                        exhausted: true,
                    });
                };
                let (sequence, event_id) = entry?;
                let actual = sequence.value_try()?;
                if actual != expected {
                    return crate::SocialPostMaterializationLogGapSnafu { expected, actual }.fail();
                }
                let event_id = event_id.value_try()?;
                items.push(Self::resolve_social_post_materialization_tx(
                    event_id,
                    &events_table,
                    &states,
                    &content,
                    &replacements,
                )?);
                expected = expected.checked_add(1).context(OverflowSnafu)?;
            }

            let exhausted = match range.next().transpose()? {
                None => true,
                Some((sequence, _)) => {
                    let actual = sequence.value_try()?;
                    if actual != expected {
                        return crate::SocialPostMaterializationLogGapSnafu { expected, actual }
                            .fail();
                    }
                    false
                }
            };
            Ok(SocialPostMaterializationPage {
                items,
                scanned_through: SocialPostMaterializationCursor(expected),
                exhausted,
            })
        })
        .await
    }

    fn resolve_social_post_materialization_tx(
        event_id: ShortEventId,
        events_table: &impl events::ReadableTable,
        states: &impl events_content_state::ReadableTable,
        content: &impl content_store::ReadableTable,
        replacements: &impl social_posts_replaced_by::ReadableTable,
    ) -> DbResult<SocialPostMaterialization> {
        let event = events_table
            .get(&event_id)?
            .map(|entry| entry.value_try())
            .transpose()?
            .context(crate::MissingSocialPostMaterializationEventSnafu { event_id })?;
        if event.kind() != EventKind::SOCIAL_POST {
            return crate::InvalidSocialPostMaterializationKindSnafu {
                event_id,
                kind: event.kind(),
            }
            .fail();
        }
        let post_id = ExternalEventId::new(event.author(), event_id);

        match Self::get_event_content_state_tx(event_id, states)? {
            Some(EventContentState::Deleted { .. } | EventContentState::Pruned) => {
                return Ok(SocialPostMaterialization::Removed { post_id });
            }
            Some(EventContentState::Missing { .. }) => {
                return crate::MissingSocialPostMaterializationStateSnafu { event_id }.fail();
            }
            Some(EventContentState::Invalid) => {
                return crate::InvalidSocialPostMaterializationStateSnafu { event_id }.fail();
            }
            None => {}
        }

        let ContentStoreRecord(raw) = content
            .get(&event.content_hash())?
            .map(|entry| entry.value_try())
            .transpose()?
            .context(crate::MissingSocialPostMaterializationContentSnafu { event_id })?;
        let content = raw
            .deserialize_cbor::<content_kind::SocialPost>()
            .map_err(
                |source| crate::DbError::InvalidSocialPostMaterializationContent {
                    event_id,
                    source: Box::new(source),
                },
            )?;
        if Self::is_social_post_replaced_tx(event.author(), event_id, replacements)? {
            return Ok(SocialPostMaterialization::Removed { post_id });
        }
        Ok(SocialPostMaterialization::Present {
            post_id,
            authored_at: event.timestamp(),
            content: Box::new(content),
        })
    }
}
