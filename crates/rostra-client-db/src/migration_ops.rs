//! Database migration operations.
//!
//! This module handles schema versioning and migrations. The approach is
//! simple: if the database version is older than the current schema version, we
//! perform a "total migration" that re-derives all state from the
//! source-of-truth tables.

use std::borrow::Cow;

use bincode::{Decode, Encode};
use redb::{ReadableTable as _, ReadableTableMetadata as _, TableHandle as _};
use rostra_core::ShortEventId;
use rostra_core::event::{
    EventContentRaw, EventContentUnsized, EventExt as _, SignedEvent, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::ToShort as _;
use tracing::{debug, info};

use crate::event::ContentStoreRecord;
use crate::id_self::IdSelfAccountRecord;
use crate::{
    ContentStoreRecordOwned, Database, DbResult, DbVersionTooHighSnafu, LOG_TARGET,
    WriteTransactionCtx, content_store, db_version, events, ids_self,
};

/// Legacy content state from old event-id-based content store.
///
/// This type is kept for migration compatibility only. Old databases stored
/// content in `events_content` table keyed by ShortEventId with this type.
/// New databases use `content_store` keyed by ContentHash instead.
#[derive(Debug, Encode, Decode, Clone)]
pub enum LegacyEventContentState<'a> {
    /// Content is present and was successfully processed.
    Present(Cow<'a, EventContentUnsized>),

    /// Content was deleted by the author.
    Deleted {
        /// The event that requested this content be deleted
        deleted_by: ShortEventId,
    },

    /// Content was pruned (removed to save space).
    Pruned,

    /// Content is present but was invalid during processing.
    Invalid(Cow<'a, EventContentUnsized>),
}

/// Owned version of legacy content state.
pub type LegacyEventContentStateOwned = LegacyEventContentState<'static>;

/// Current schema version.
///
/// Increment this when making schema changes that require migration.
const DB_VER: u64 = 14;

/// Versions older than this require a total migration.
///
/// This should be set to the version where we last did a major schema
/// overhaul. Older databases get rebuilt from scratch.
const DB_VER_REQUIRES_TOTAL_MIGRATION: u64 = 14;

/// Prefix used for temporary tables during total migration.
const MIGRATION_TEMP_PREFIX: &str = "_total_migration_";

/// Name of the temp events table used during total migration.
/// If this table exists, reprocessing is pending.
const MIGRATION_EVENTS_TEMP_TABLE: &str = "_total_migration_events";

impl Database {
    /// Check if there's a pending migration stash that needs reprocessing.
    ///
    /// This checks for the existence of temp tables from a previous migration
    /// that was interrupted before completing. Returns true if reprocessing
    /// is needed.
    pub(crate) fn has_pending_migration_stash(dbtx: &WriteTransactionCtx) -> DbResult<bool> {
        let has_stash = dbtx
            .as_raw()
            .list_tables()?
            .any(|h| h.name() == MIGRATION_EVENTS_TEMP_TABLE);

        if has_stash {
            info!(
                target: LOG_TARGET,
                "Found pending migration stash from interrupted migration"
            );
        }

        Ok(has_stash)
    }

    /// Initialize all current schema tables.
    pub(crate) fn init_tables_tx(tx: &WriteTransactionCtx) -> DbResult<()> {
        tx.open_table(&db_version::TABLE)?;

        tx.open_table(&crate::ids_self::TABLE)?;
        tx.open_table(&crate::ids_full::TABLE)?;
        tx.open_table(&crate::ids_followers::TABLE)?;
        tx.open_table(&crate::ids_followees::TABLE)?;
        tx.open_table(&crate::ids_unfollowed::TABLE)?;
        tx.open_table(&crate::ids_personas::TABLE)?;
        tx.open_table(&crate::ids_data_usage::TABLE)?;
        tx.open_table(&crate::ids_nodes::TABLE)?;

        tx.open_table(&crate::events::TABLE)?;
        tx.open_table(&crate::events_singletons_new::TABLE)?;
        tx.open_table(&crate::events_missing::TABLE)?;
        tx.open_table(&crate::events_by_time::TABLE)?;
        tx.open_table(&crate::events_content_missing::TABLE)?;
        tx.open_table(&crate::events_self::TABLE)?;
        tx.open_table(&crate::events_heads::TABLE)?;

        tx.open_table(&crate::content_store::TABLE)?;
        tx.open_table(&crate::content_rc::TABLE)?;
        tx.open_table(&crate::events_content_state::TABLE)?;
        tx.open_table(&crate::events_received_at::TABLE)?;

        tx.open_table(&crate::social_profiles::TABLE)?;
        tx.open_table(&crate::social_posts::TABLE)?;
        tx.open_table(&crate::social_posts_by_time::TABLE)?;
        tx.open_table(&crate::social_posts_by_received_at::TABLE)?;
        tx.open_table(&crate::social_posts_replies::TABLE)?;
        tx.open_table(&crate::social_posts_reactions::TABLE)?;
        tx.open_table(&crate::social_posts_self_mention::TABLE)?;

        tx.open_table(&crate::shoutbox_posts_by_received_at::TABLE)?;
        Ok(())
    }

    /// Handle database version check and migrations.
    ///
    /// If total migration is needed, this function:
    /// 1. Copies events, content_store, ids_self to temp tables
    /// 2. Deletes all tables except temp and db_version
    /// 3. Initializes fresh tables with current schema
    /// 4. Restores ids_self from temp
    ///
    /// The actual reprocessing of events happens later via
    /// `reprocess_migration_stash`. Use `has_pending_migration_stash` to check
    /// if reprocessing is needed (this allows retrying after failures).
    pub(crate) fn handle_db_ver_migrations(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        let mut table_db_ver = dbtx.open_table(&db_version::TABLE)?;

        let Some(cur_db_ver) = table_db_ver.first()?.map(|g| g.1.value()) else {
            info!(target: LOG_TARGET, "Initializing new database");
            table_db_ver.insert(&(), &DB_VER)?;
            return Ok(());
        };

        if DB_VER < cur_db_ver {
            return DbVersionTooHighSnafu {
                db_ver: cur_db_ver,
                code_ver: DB_VER,
            }
            .fail();
        }

        if cur_db_ver == DB_VER {
            debug!(target: LOG_TARGET, db_ver = DB_VER, "Database version up to date");
            return Ok(());
        }

        // Drop the db_version table handle before migrations
        drop(table_db_ver);

        if cur_db_ver < DB_VER_REQUIRES_TOTAL_MIGRATION {
            info!(
                target: LOG_TARGET,
                from_ver = cur_db_ver,
                to_ver = DB_VER,
                "Database schema very old, preparing total migration"
            );
            Self::prepare_total_migration(dbtx)?;
        }

        // Run incremental migrations
        if cur_db_ver < DB_VER {
            info!(
                target: LOG_TARGET,
                from_ver = cur_db_ver,
                to_ver = DB_VER,
                "Running incremental migrations"
            );

            // Future incremental migrations go here, e.g.:
            // if cur_db_ver < 8 {
            //     Self::migrate_v7_to_v8(dbtx)?;
            // }
        }

        // Update version
        let mut table_db_ver = dbtx.open_table(&db_version::TABLE)?;
        table_db_ver.insert(&(), &DB_VER)?;
        debug!(target: LOG_TARGET, db_ver = DB_VER, "Database version updated");

        Ok(())
    }

    /// Prepare for total migration by stashing source-of-truth tables.
    ///
    /// This copies events/content_store/ids_self to temp tables, deletes all
    /// other tables, and initializes fresh schema. The ids_self is restored
    /// immediately so the Database can be created normally.
    fn prepare_total_migration(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        // Define temp table definitions
        let events_temp: redb_bincode::TableDefinition<
            '_,
            rostra_core::ShortEventId,
            crate::EventRecord,
        > = redb_bincode::TableDefinition::new("_total_migration_events");
        let content_store_temp: redb_bincode::TableDefinition<
            '_,
            rostra_core::ContentHash,
            ContentStoreRecordOwned,
        > = redb_bincode::TableDefinition::new("_total_migration_content_store");
        let ids_self_temp: redb_bincode::TableDefinition<'_, (), IdSelfAccountRecord> =
            redb_bincode::TableDefinition::new("_total_migration_ids_self");

        // Legacy table definition for old event-id-based content store
        let legacy_events_content: redb_bincode::TableDefinition<
            '_,
            ShortEventId,
            LegacyEventContentStateOwned,
        > = redb_bincode::TableDefinition::new("events_content");
        let legacy_events_content_temp: redb_bincode::TableDefinition<
            '_,
            ShortEventId,
            LegacyEventContentStateOwned,
        > = redb_bincode::TableDefinition::new("_total_migration_events_content_legacy");

        // Step 1: Copy preserved tables to temp
        info!(target: LOG_TARGET, "Copying preserved tables to temp...");
        Self::copy_table_raw(dbtx, &events::TABLE, &events_temp)?;
        Self::copy_table_raw(dbtx, &content_store::TABLE, &content_store_temp)?;
        Self::copy_table_raw(dbtx, &ids_self::TABLE, &ids_self_temp)?;

        // Try to copy legacy events_content table if it exists
        if Self::copy_table_raw_if_exists(
            dbtx,
            &legacy_events_content,
            &legacy_events_content_temp,
        )? {
            info!(target: LOG_TARGET, "Copied legacy events_content table to temp");
        }

        // Step 2: Delete all tables except temp and db_version
        info!(target: LOG_TARGET, "Deleting old tables...");
        let table_names: Vec<String> = dbtx
            .as_raw()
            .list_tables()?
            .map(|h| h.name().to_string())
            .collect();

        for name in &table_names {
            if name.starts_with(MIGRATION_TEMP_PREFIX) || name == "db_version" {
                continue;
            }
            let raw_def = redb::TableDefinition::<&[u8], &[u8]>::new(name);
            if dbtx.as_raw().delete_table(raw_def)? {
                debug!(target: LOG_TARGET, table = %name, "Deleted table");
            }
        }

        // Step 3: Initialize fresh tables with current schema
        info!(target: LOG_TARGET, "Initializing fresh tables...");
        Self::init_tables_tx(dbtx)?;

        // Step 4: Restore ids_self from temp (needed for Database creation)
        {
            let temp_table = dbtx.open_table(&ids_self_temp)?;
            let mut ids_self_table = dbtx.open_table(&ids_self::TABLE)?;
            if let Some(record) = temp_table.get(&())?.map(|g| g.value()) {
                ids_self_table.insert(&(), &record)?;
            }
        }

        info!(target: LOG_TARGET, "Total migration prepared, events stashed for reprocessing");
        Ok(())
    }

    /// Reprocess events stashed during total migration.
    ///
    /// This reads from temp tables, processes each event using the normal
    /// processing functions, then cleans up the temp tables.
    pub(crate) fn reprocess_migration_stash(&self, dbtx: &WriteTransactionCtx) -> DbResult<()> {
        info!(target: LOG_TARGET, "Reprocessing stashed events...");

        // Define temp table definitions
        let events_temp: redb_bincode::TableDefinition<
            '_,
            rostra_core::ShortEventId,
            crate::EventRecord,
        > = redb_bincode::TableDefinition::new("_total_migration_events");
        let content_store_temp: redb_bincode::TableDefinition<
            '_,
            rostra_core::ContentHash,
            ContentStoreRecordOwned,
        > = redb_bincode::TableDefinition::new("_total_migration_content_store");
        let ids_self_temp: redb_bincode::TableDefinition<'_, (), IdSelfAccountRecord> =
            redb_bincode::TableDefinition::new("_total_migration_ids_self");

        // Legacy temp table for old event-id-based content store
        let legacy_events_content_temp: redb_bincode::TableDefinition<
            '_,
            ShortEventId,
            LegacyEventContentStateOwned,
        > = redb_bincode::TableDefinition::new("_total_migration_events_content_legacy");

        // Open temp tables for iteration
        let events_temp_table = dbtx.open_table(&events_temp)?;
        let content_temp_table = dbtx.open_table(&content_store_temp)?;

        // Try to open legacy content table (may not exist in newer databases)
        let legacy_content_table_exists = dbtx
            .as_raw()
            .list_tables()?
            .any(|h| h.name() == legacy_events_content_temp.as_raw().name());
        let legacy_content_temp_table = if legacy_content_table_exists {
            info!(target: LOG_TARGET, "Found legacy events_content table, will use for fallback");
            Some(dbtx.open_table(&legacy_events_content_temp)?)
        } else {
            None
        };

        info!(target: LOG_TARGET, "Re-processing events...");

        let mut processed_count = 0u64;
        let mut legacy_content_used = 0u64;

        for result in events_temp_table.range(..)? {
            let (event_id, event_record) = result?;
            let event_id = event_id.value();
            let event_record = event_record.value();
            let content_hash = event_record.content_hash();
            let timestamp = event_record.timestamp();
            let event_kind = event_record.signed.event.kind;
            let author = event_record.signed.event.author;

            // Create VerifiedEvent from stored SignedEvent
            let signed_event = SignedEvent {
                event: event_record.signed.event,
                sig: event_record.signed.sig,
            };
            let verified_event = VerifiedEvent::assume_verified_from_signed(signed_event);

            // Process event using the same function as normal operation.
            // Use event timestamp as "now" since we don't have original received_at.
            let (insert_outcome, _content_state) =
                self.process_event_tx(&verified_event, timestamp, dbtx)?;

            // Log event insertion result for debugging
            debug!(
                target: LOG_TARGET,
                kind = %event_kind,
                author = %author.to_short(),
                event_id = %event_id,
                ?insert_outcome,
                "Migration: processed event"
            );

            // Look up content - first try hash-based store, then legacy event-id store
            let content_record = content_temp_table.get(&content_hash)?.map(|g| g.value());

            // Helper to get content from legacy table
            let legacy_content = || -> Option<EventContentRaw> {
                let legacy_table = legacy_content_temp_table.as_ref()?;
                let legacy_record = legacy_table.get(&event_id).ok()?.map(|g| g.value())?;
                match legacy_record {
                    LegacyEventContentState::Present(cow) => {
                        // Convert Cow<EventContentUnsized> to EventContentRaw
                        Some(cow.as_ref().to_owned())
                    }
                    LegacyEventContentState::Invalid(cow) => {
                        // We could try to reprocess invalid content, but skip for now
                        debug!(
                            target: LOG_TARGET,
                            kind = %event_kind,
                            author = %author.to_short(),
                            "Migration: skipping legacy Invalid content"
                        );
                        let _ = cow; // Suppress unused warning
                        None
                    }
                    LegacyEventContentState::Deleted { .. } | LegacyEventContentState::Pruned => {
                        None
                    }
                }
            };

            match content_record {
                Some(ContentStoreRecord::Present(content_data)) => {
                    let content_raw = content_data.into_owned();

                    // Create VerifiedEventContent and process it
                    let verified_content =
                        VerifiedEventContent::assume_verified(verified_event, content_raw);

                    // Process content using the same function as normal operation.
                    // Use event timestamp as "now" for migration.
                    self.process_event_content_tx(&verified_content, timestamp, dbtx)?;
                }
                Some(ContentStoreRecord::Invalid(_)) => {
                    debug!(
                        target: LOG_TARGET,
                        kind = %event_kind,
                        author = %author.to_short(),
                        "Migration: skipping event with Invalid content"
                    );
                }
                None => {
                    // Try legacy table
                    if let Some(content_raw) = legacy_content() {
                        legacy_content_used += 1;

                        // Verify content hash matches what's in the event envelope
                        match VerifiedEventContent::verify(verified_event, content_raw) {
                            Ok(verified_content) => {
                                debug!(
                                    target: LOG_TARGET,
                                    kind = %event_kind,
                                    author = %author.to_short(),
                                    "Migration: using content from legacy events_content table"
                                );
                                self.process_event_content_tx(&verified_content, timestamp, dbtx)?;
                            }
                            Err(err) => {
                                // Content hash mismatch - the legacy content doesn't match the
                                // event envelope. This can happen if data was corrupted.
                                debug!(
                                    target: LOG_TARGET,
                                    kind = %event_kind,
                                    author = %author.to_short(),
                                    ?err,
                                    "Migration: legacy content hash mismatch, skipping"
                                );
                            }
                        }
                    } else {
                        debug!(
                            target: LOG_TARGET,
                            kind = %event_kind,
                            author = %author.to_short(),
                            "Migration: no content found for event in either store"
                        );
                    }
                }
            }

            processed_count += 1;
            if processed_count % 10000 == 0 {
                debug!(
                    target: LOG_TARGET,
                    processed_count,
                    "Migration progress"
                );
            }
        }

        drop(events_temp_table);
        drop(content_temp_table);
        drop(legacy_content_temp_table);

        // Verify migration results by counting entries in key tables
        let events_count = dbtx
            .as_raw()
            .open_table(crate::events::TABLE.as_raw())?
            .len()?;
        let events_by_time_count = dbtx
            .as_raw()
            .open_table(crate::events_by_time::TABLE.as_raw())?
            .len()?;
        let social_posts_by_time_count = dbtx
            .as_raw()
            .open_table(crate::social_posts_by_time::TABLE.as_raw())?
            .len()?;
        let followees_count = dbtx
            .as_raw()
            .open_table(crate::ids_followees::TABLE.as_raw())?
            .len()?;
        let followers_count = dbtx
            .as_raw()
            .open_table(crate::ids_followers::TABLE.as_raw())?
            .len()?;

        info!(
            target: LOG_TARGET,
            events_count,
            events_by_time_count,
            social_posts_by_time_count,
            followees_count,
            followers_count,
            "Migration: table counts after reprocessing"
        );

        // Clean up temp tables
        info!(target: LOG_TARGET, "Cleaning up temp tables...");
        dbtx.as_raw().delete_table(events_temp.as_raw())?;
        dbtx.as_raw().delete_table(content_store_temp.as_raw())?;
        dbtx.as_raw().delete_table(ids_self_temp.as_raw())?;
        // Try to delete legacy temp table (may not exist)
        let _ = dbtx
            .as_raw()
            .delete_table(legacy_events_content_temp.as_raw());

        info!(
            target: LOG_TARGET,
            processed_count,
            legacy_content_used,
            "Total migration complete"
        );

        Ok(())
    }

    /// Copy a table's contents to another table (both must have compatible raw
    /// types).
    fn copy_table_raw<KS, VS, KD, VD>(
        dbtx: &WriteTransactionCtx,
        src: &redb_bincode::TableDefinition<'_, KS, VS>,
        dst: &redb_bincode::TableDefinition<'_, KD, VD>,
    ) -> DbResult<()> {
        let mut dst_tbl = dbtx.as_raw().open_table(dst.as_raw())?;
        let src_table = dbtx.as_raw().open_table(src.as_raw())?;
        for record in src_table.range::<&[u8]>(..)? {
            let (k, v) = record?;
            dst_tbl.insert(k.value(), v.value())?;
        }
        Ok(())
    }

    /// Copy a table's contents to another table if the source table exists.
    /// Returns true if the table existed and was copied, false if it didn't
    /// exist.
    fn copy_table_raw_if_exists<KS, VS, KD, VD>(
        dbtx: &WriteTransactionCtx,
        src: &redb_bincode::TableDefinition<'_, KS, VS>,
        dst: &redb_bincode::TableDefinition<'_, KD, VD>,
    ) -> DbResult<bool> {
        // Check if source table exists by listing tables
        let table_exists = dbtx
            .as_raw()
            .list_tables()?
            .any(|h| h.name() == src.as_raw().name());

        if !table_exists {
            return Ok(false);
        }

        let mut dst_tbl = dbtx.as_raw().open_table(dst.as_raw())?;
        let src_table = dbtx.as_raw().open_table(src.as_raw())?;
        for record in src_table.range::<&[u8]>(..)? {
            let (k, v) = record?;
            dst_tbl.insert(k.value(), v.value())?;
        }
        Ok(true)
    }
}
