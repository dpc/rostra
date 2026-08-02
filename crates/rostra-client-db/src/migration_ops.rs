//! Database migration operations.
//!
//! This module handles schema versioning and migrations. The approach is
//! simple: if the database version is older than the current schema version, we
//! perform a "total migration" that re-derives all disposable state from the
//! source-of-truth tables.

use std::borrow::Cow;

use bincode::{Decode, Encode};
use redb::{ReadableTable as _, ReadableTableMetadata as _, TableHandle as _};
use rostra_core::event::{
    EventContentRaw, EventContentUnsized, EventExt as _, SignedEvent, VerifiedEvent,
    VerifiedEventContent,
};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use tracing::{debug, info};

use crate::id_self::IdSelfAccountRecord;
use crate::{
    Database, DbResult, DbVersionTooHighSnafu, EventReceivedSource, LOG_TARGET,
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

/// Legacy content store record from before ContentStoreRecord became a tuple
/// struct. Old databases stored this as an enum with a `Present` variant,
/// which has a different bincode encoding (variant discriminant byte).
#[derive(Debug, Encode, Decode, Clone)]
pub enum LegacyContentStoreRecord<'a> {
    Present(Cow<'a, EventContentUnsized>),
}

pub type LegacyContentStoreRecordOwned = LegacyContentStoreRecord<'static>;

/// Reception record used by schema versions 6 through 12.
///
/// Versions 6 through 11 keyed this value by `(Timestamp, ShortEventId)`;
/// version 12 added a sequence to make the key
/// `(Timestamp, u64, ShortEventId)`. Version 13 moved the event ID into the
/// value and changed the key to `(Timestamp, u64)`.
#[derive(Debug, Encode, Decode, Clone)]
pub(crate) struct LegacyEventReceivedRecord {
    pub(crate) source: EventReceivedSource,
}

/// Current schema version.
///
/// Increment this when making schema changes that require migration.
///
/// Version 25 performs the single final rebuild for the stacked version-24
/// schema changes. Version 26 adds the empty append-only SocialPost
/// materialization feed without backfill.
const DB_VER: u64 = 26;

/// Versions older than this require a total migration.
///
/// This should be set to the version where we last did a major schema
/// overhaul. Older databases get rebuilt from scratch.
const DB_VER_REQUIRES_TOTAL_MIGRATION: u64 = 25;

/// Last DB version that used the legacy enum `ContentStoreRecord::Present(...)`
/// format.
///
/// Versions at or below this used an enum wrapper. Version 17+ switched to
/// the tuple struct `ContentStoreRecord(Cow<...>)` format. During total
/// migration, we use `LegacyContentStoreRecord` for databases from these
/// older versions.
const DB_VER_LEGACY_CONTENT_STORE_FORMAT: u64 = 16;

/// First version with the reception-source table.
const DB_VER_EVENT_RECEIPTS: u64 = 6;

/// Last version whose reception key contained the event ID.
const DB_VER_EVENT_RECEIPT_ID_IN_KEY: u64 = 12;

/// Last version whose reception key did not contain a sequence.
const DB_VER_EVENT_RECEIPT_WITHOUT_SEQUENCE: u64 = 11;

/// Prefix used for temporary tables during total migration.
const MIGRATION_TEMP_PREFIX: &str = "_total_migration_";

/// Name of the temp events table used during total migration.
/// If this table exists, reprocessing is pending.
const MIGRATION_EVENTS_TEMP_TABLE: &str = "_total_migration_events";

/// Name of the temp table storing the source DB version during migration.
const MIGRATION_SOURCE_VER_TEMP_TABLE: &str = "_total_migration_source_ver";

/// Name of the temp table retaining per-event acquisition provenance.
const MIGRATION_EVENT_SOURCES_TEMP_TABLE: &str = "_total_migration_event_sources";

/// Name of the temp table preserving the append-only post materialization feed.
const MIGRATION_SOCIAL_POST_MATERIALIZATIONS_TEMP_TABLE: &str =
    "_total_migration_social_post_materializations";

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
        tx.open_table(&crate::db_init_time::TABLE)?;
        tx.open_table(&crate::reception_order_next::TABLE)?;

        tx.open_table(&crate::ids_self::TABLE)?;
        crate::ids_full::init(tx)?;
        tx.open_table(&crate::ids_followers::TABLE)?;
        tx.open_table(&crate::ids_followees::TABLE)?;
        tx.open_table(&crate::ids_follow_events::TABLE)?;
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
        tx.open_table(&crate::social_post_materializations::TABLE)?;
        tx.open_table(&crate::social_posts_by_received_at::TABLE)?;
        tx.open_table(&crate::social_posts_received_at_keys::TABLE)?;
        tx.open_table(&crate::social_posts_replies::TABLE)?;
        tx.open_table(&crate::social_posts_reactions::TABLE)?;
        tx.open_table(&crate::social_posts_replaced_by::TABLE)?;
        tx.open_table(&crate::social_posts_replaces::TABLE)?;
        tx.open_table(&crate::social_vote_sums::TABLE)?;
        tx.open_table(&crate::social_news_rank_by_post_id::TABLE)?;
        tx.open_table(&crate::social_news_rank_by_score::TABLE)?;
        tx.open_table(&crate::social_news_rank_by_time::TABLE)?;
        tx.open_table(&crate::social_posts_self_mention::TABLE)?;

        tx.open_table(&crate::shoutbox_posts_by_received_at::TABLE)?;
        Ok(())
    }

    /// Handle database version check and migrations.
    ///
    /// If total migration is needed, this function:
    /// 1. Copies events, content_store, ids_self, db_init_time, event
    ///    acquisition sources, and canonical social-post replacement rows to
    ///    temp tables
    /// 2. Deletes built-in tables except temp and db_version, preserving
    ///    caller-owned extension tables
    /// 3. Initializes fresh tables with current schema
    /// 4. Restores stable metadata and canonical replacement rows from temp
    ///
    /// The actual reprocessing of events happens later via
    /// `reprocess_migration_stash`. Use `has_pending_migration_stash` to check
    /// if reprocessing is needed (this allows retrying after failures).
    pub(crate) fn handle_db_ver_migrations(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        let mut table_db_ver = dbtx.open_table(&db_version::TABLE)?;

        let Some(cur_db_ver) = table_db_ver.first()?.map(|g| g.1.value_try()).transpose()? else {
            info!(target: LOG_TARGET, "Initializing new database");
            table_db_ver.insert(&(), &DB_VER)?;
            drop(table_db_ver);
            let mut init_time_table = dbtx.open_table(&crate::db_init_time::TABLE)?;
            init_time_table.insert(&(), &Timestamp::now())?;
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
                "Database schema requires total migration"
            );
            if Self::has_pending_migration_stash(dbtx)? {
                // A committed stash is authoritative across binary/schema
                // upgrades. In particular, its source-version discriminator
                // selects the content decoder. Never restash or overwrite it.
                Self::adopt_pending_migration_stash(dbtx)?;
            } else {
                Self::prepare_total_migration(dbtx, cur_db_ver)?;
            }
        }

        // Run incremental migrations
        if cur_db_ver < DB_VER {
            info!(
                target: LOG_TARGET,
                from_ver = cur_db_ver,
                to_ver = DB_VER,
                "Running incremental migrations"
            );

            if cur_db_ver < 21 {
                // Set db_init_time for existing databases that don't have it yet
                let mut init_time_table = dbtx.open_table(&crate::db_init_time::TABLE)?;
                if init_time_table.get(&())?.is_none() {
                    init_time_table.insert(&(), &Timestamp::now())?;
                }
            }
        }

        // Update version
        let mut table_db_ver = dbtx.open_table(&db_version::TABLE)?;
        table_db_ver.insert(&(), &DB_VER)?;
        debug!(target: LOG_TARGET, db_ver = DB_VER, "Database version updated");

        Ok(())
    }

    /// Prepare for total migration by stashing source-of-truth tables.
    ///
    /// This copies events, content_store, stable metadata, and canonical
    /// social-post replacement rows to temp tables, deletes other built-in
    /// tables, and initializes fresh schema. Caller-owned extension tables are
    /// preserved byte-for-byte. Stable metadata and replacement rows are
    /// restored immediately so the Database can be created normally.
    pub(crate) fn prepare_total_migration(
        dbtx: &WriteTransactionCtx,
        source_ver: u64,
    ) -> DbResult<()> {
        // Define temp table definitions
        let events_temp: redb_bincode::TableDefinition<
            '_,
            rostra_core::ShortEventId,
            crate::EventRecord,
        > = redb_bincode::TableDefinition::new("_total_migration_events");
        // Type param doesn't matter for raw copy — encoding is preserved as-is
        let content_store_temp: redb_bincode::TableDefinition<
            '_,
            rostra_core::ContentHash,
            LegacyContentStoreRecordOwned,
        > = redb_bincode::TableDefinition::new("_total_migration_content_store");
        let ids_self_temp: redb_bincode::TableDefinition<'_, (), IdSelfAccountRecord> =
            redb_bincode::TableDefinition::new("_total_migration_ids_self");
        let db_init_time_temp: redb_bincode::TableDefinition<'_, (), Timestamp> =
            redb_bincode::TableDefinition::new("_total_migration_db_init_time");
        let source_ver_temp: redb_bincode::TableDefinition<'_, (), u64> =
            redb_bincode::TableDefinition::new(MIGRATION_SOURCE_VER_TEMP_TABLE);
        let event_sources_temp: redb_bincode::TableDefinition<
            '_,
            ShortEventId,
            EventReceivedSource,
        > = redb_bincode::TableDefinition::new(MIGRATION_EVENT_SOURCES_TEMP_TABLE);
        let replaced_by_temp: redb_bincode::TableDefinition<
            '_,
            (RostraId, ShortEventId, ShortEventId),
            (),
        > = redb_bincode::TableDefinition::new("_total_migration_social_posts_replaced_by");
        let materializations_temp: redb_bincode::TableDefinition<'_, u64, ShortEventId> =
            redb_bincode::TableDefinition::new(MIGRATION_SOCIAL_POST_MATERIALIZATIONS_TEMP_TABLE);

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
        Self::copy_table_raw(dbtx, &crate::db_init_time::TABLE, &db_init_time_temp)?;
        if 26 <= source_ver {
            Self::copy_table_raw(
                dbtx,
                &crate::social_post_materializations::TABLE,
                &materializations_temp,
            )?;
        }

        // Receipt timestamps and allocator values are disposable, but acquisition
        // provenance is stable local source metadata. Re-key it by event ID for
        // bounded lookup while rebuilding receipt indexes.
        if DB_VER_EVENT_RECEIPTS <= source_ver {
            let mut event_sources = dbtx.open_table(&event_sources_temp)?;
            if source_ver <= DB_VER_EVENT_RECEIPT_WITHOUT_SEQUENCE {
                let legacy_receipts: redb_bincode::TableDefinition<
                    '_,
                    (Timestamp, ShortEventId),
                    LegacyEventReceivedRecord,
                > = redb_bincode::TableDefinition::new("events_received_at");
                let receipts = dbtx.open_table(&legacy_receipts)?;
                for entry in receipts.range(..)? {
                    let (key, receipt) = entry?;
                    let event_id = key.value_try()?.1;
                    if event_sources.get(&event_id)?.is_none() {
                        event_sources.insert(&event_id, &receipt.value_try()?.source)?;
                    }
                }
            } else if source_ver <= DB_VER_EVENT_RECEIPT_ID_IN_KEY {
                let legacy_receipts: redb_bincode::TableDefinition<
                    '_,
                    (Timestamp, u64, ShortEventId),
                    LegacyEventReceivedRecord,
                > = redb_bincode::TableDefinition::new("events_received_at");
                let receipts = dbtx.open_table(&legacy_receipts)?;
                for entry in receipts.range(..)? {
                    let (key, receipt) = entry?;
                    let event_id = key.value_try()?.2;
                    if event_sources.get(&event_id)?.is_none() {
                        event_sources.insert(&event_id, &receipt.value_try()?.source)?;
                    }
                }
            } else {
                let receipts = dbtx.open_table(&crate::events_received_at::TABLE)?;
                for entry in receipts.range(..)? {
                    let (_, receipt) = entry?;
                    let receipt = receipt.value_try()?;
                    if event_sources.get(&receipt.event_id)?.is_none() {
                        event_sources.insert(&receipt.event_id, &receipt.source)?;
                    }
                }
            }
        }

        // Preserve only canonical immutable lineage that remains eligible under
        // the final exclusive content-size boundary. Filtering here avoids
        // collecting keys or mutating a table while iterating it after replay.
        {
            let source = dbtx.open_table(&crate::social_posts_replaced_by::TABLE)?;
            let source_events = dbtx.open_table(&events::TABLE)?;
            let mut destination = dbtx.open_table(&replaced_by_temp)?;
            for entry in source.range(..)? {
                let (key, _) = entry?;
                let key = key.value_try()?;
                if source_events
                    .get(&key.2)?
                    .map(|event| event.value_try())
                    .transpose()?
                    .is_some_and(|event| Self::MAX_CONTENT_LEN <= event.content_len())
                {
                    continue;
                }
                destination.insert(&key, &())?;
            }
        }

        // Try to copy legacy events_content table if it exists
        if Self::copy_table_raw_if_exists(
            dbtx,
            &legacy_events_content,
            &legacy_events_content_temp,
        )? {
            info!(target: LOG_TARGET, "Copied legacy events_content table to temp");
        }

        // Store source DB version so reprocessing knows which format to use
        {
            let mut ver_table = dbtx.open_table(&source_ver_temp)?;
            ver_table.insert(&(), &source_ver)?;
        }

        Self::install_current_schema_from_stash(dbtx)?;

        info!(target: LOG_TARGET, "Total migration prepared, events stashed for reprocessing");
        Ok(())
    }

    /// Adopt a stash committed by an older binary without rewriting its source
    /// version or source bytes.
    fn adopt_pending_migration_stash(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        let table_names = dbtx
            .as_raw()
            .list_tables()?
            .map(|table| table.name().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        for required in [
            MIGRATION_EVENTS_TEMP_TABLE,
            "_total_migration_content_store",
            "_total_migration_ids_self",
            MIGRATION_SOURCE_VER_TEMP_TABLE,
        ] {
            if !table_names.contains(required) {
                return crate::MissingMigrationStashTableSnafu {
                    table: required.to_owned(),
                }
                .fail();
            }
        }

        let db_init_time_temp: redb_bincode::TableDefinition<'_, (), Timestamp> =
            redb_bincode::TableDefinition::new("_total_migration_db_init_time");
        if !table_names.contains("_total_migration_db_init_time") {
            let current = dbtx
                .open_table(&crate::db_init_time::TABLE)?
                .get(&())?
                .map(|value| value.value_try())
                .transpose()?;
            let mut destination = dbtx.open_table(&db_init_time_temp)?;
            if let Some(current) = current {
                destination.insert(&(), &current)?;
            }
        }

        let event_sources_temp: redb_bincode::TableDefinition<
            '_,
            ShortEventId,
            EventReceivedSource,
        > = redb_bincode::TableDefinition::new(MIGRATION_EVENT_SOURCES_TEMP_TABLE);
        // Production version 24 did not preserve provenance separately. Creating
        // an empty table gives those receipts the documented Migration fallback.
        dbtx.open_table(&event_sources_temp)?;
        Self::install_current_schema_from_stash(dbtx)?;
        info!(target: LOG_TARGET, "Adopted pending migration stash without restashing");
        Ok(())
    }

    /// Replace every reserved derived table with current schema and restore
    /// stable metadata already held in the migration stash.
    fn install_current_schema_from_stash(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        let ids_self_temp: redb_bincode::TableDefinition<'_, (), IdSelfAccountRecord> =
            redb_bincode::TableDefinition::new("_total_migration_ids_self");
        let db_init_time_temp: redb_bincode::TableDefinition<'_, (), Timestamp> =
            redb_bincode::TableDefinition::new("_total_migration_db_init_time");
        let replaced_by_temp: redb_bincode::TableDefinition<
            '_,
            (RostraId, ShortEventId, ShortEventId),
            (),
        > = redb_bincode::TableDefinition::new("_total_migration_social_posts_replaced_by");
        let materializations_temp: redb_bincode::TableDefinition<'_, u64, ShortEventId> =
            redb_bincode::TableDefinition::new(MIGRATION_SOCIAL_POST_MATERIALIZATIONS_TEMP_TABLE);

        // Delete built-in tables except temp and db_version.
        info!(target: LOG_TARGET, "Deleting old tables...");
        let table_names: Vec<String> = dbtx
            .as_raw()
            .list_tables()?
            .map(|h| h.name().to_string())
            .collect();
        let source_ver = dbtx
            .open_table(&redb_bincode::TableDefinition::<(), u64>::new(
                MIGRATION_SOURCE_VER_TEMP_TABLE,
            ))?
            .get(&())?
            .map(|value| value.value_try())
            .transpose()?
            .unwrap_or(0);
        if 26 <= source_ver
            && !table_names
                .iter()
                .any(|name| name == MIGRATION_SOCIAL_POST_MATERIALIZATIONS_TEMP_TABLE)
        {
            return crate::MissingMigrationStashTableSnafu {
                table: MIGRATION_SOCIAL_POST_MATERIALIZATIONS_TEMP_TABLE.to_owned(),
            }
            .fail();
        }

        for name in &table_names {
            if name.starts_with(MIGRATION_TEMP_PREFIX)
                || name == "db_version"
                || !crate::extension::is_reserved_extension_table(name)
            {
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

        // Step 4: Restore stable database metadata.
        {
            let temp_table = dbtx.open_table(&ids_self_temp)?;
            let mut ids_self_table = dbtx.open_table(&ids_self::TABLE)?;
            if let Some(record) = temp_table.get(&())?.map(|g| g.value_try()).transpose()? {
                ids_self_table.insert(&(), &record)?;
            }
        }
        {
            let temp_table = dbtx.open_table(&db_init_time_temp)?;
            let mut db_init_time_table = dbtx.open_table(&crate::db_init_time::TABLE)?;
            if let Some(timestamp) = temp_table.get(&())?.map(|g| g.value_try()).transpose()? {
                db_init_time_table.insert(&(), &timestamp)?;
            }
        }
        Self::copy_table_raw(
            dbtx,
            &replaced_by_temp,
            &crate::social_posts_replaced_by::TABLE,
        )?;
        if 26 <= source_ver {
            Self::copy_table_raw(
                dbtx,
                &materializations_temp,
                &crate::social_post_materializations::TABLE,
            )?;
        }

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
        let ids_self_temp: redb_bincode::TableDefinition<'_, (), IdSelfAccountRecord> =
            redb_bincode::TableDefinition::new("_total_migration_ids_self");
        let db_init_time_temp: redb_bincode::TableDefinition<'_, (), Timestamp> =
            redb_bincode::TableDefinition::new("_total_migration_db_init_time");
        let replaced_by_temp: redb_bincode::TableDefinition<
            '_,
            (RostraId, ShortEventId, ShortEventId),
            (),
        > = redb_bincode::TableDefinition::new("_total_migration_social_posts_replaced_by");
        let materializations_temp: redb_bincode::TableDefinition<'_, u64, ShortEventId> =
            redb_bincode::TableDefinition::new(MIGRATION_SOCIAL_POST_MATERIALIZATIONS_TEMP_TABLE);

        // Read source DB version to determine content store format
        let source_ver_temp: redb_bincode::TableDefinition<'_, (), u64> =
            redb_bincode::TableDefinition::new(MIGRATION_SOURCE_VER_TEMP_TABLE);
        let event_sources_temp: redb_bincode::TableDefinition<
            '_,
            ShortEventId,
            EventReceivedSource,
        > = redb_bincode::TableDefinition::new(MIGRATION_EVENT_SOURCES_TEMP_TABLE);
        let source_ver = dbtx
            .open_table(&source_ver_temp)?
            .get(&())?
            .map(|g| g.value_try())
            .transpose()?
            // If no source version stored, assume legacy (pre-tuple-struct)
            .unwrap_or(0);
        let use_legacy_content_store = source_ver <= DB_VER_LEGACY_CONTENT_STORE_FORMAT;

        // Content store temp tables — open whichever format matches
        let legacy_content_store_temp: redb_bincode::TableDefinition<
            '_,
            rostra_core::ContentHash,
            LegacyContentStoreRecordOwned,
        > = redb_bincode::TableDefinition::new("_total_migration_content_store");
        let new_content_store_temp: redb_bincode::TableDefinition<
            '_,
            rostra_core::ContentHash,
            crate::ContentStoreRecordOwned,
        > = redb_bincode::TableDefinition::new("_total_migration_content_store");

        let legacy_content_store_table = use_legacy_content_store
            .then(|| dbtx.open_table(&legacy_content_store_temp))
            .transpose()?;
        let new_content_store_table = (!use_legacy_content_store)
            .then(|| dbtx.open_table(&new_content_store_temp))
            .transpose()?;

        // Legacy temp table for old event-id-based content store
        let legacy_events_content_temp: redb_bincode::TableDefinition<
            '_,
            ShortEventId,
            LegacyEventContentStateOwned,
        > = redb_bincode::TableDefinition::new("_total_migration_events_content_legacy");

        // A total replay happens before the Database is returned to subscribers.
        // Incremental publication closures would otherwise retain one or more
        // heap allocations per event for the full transaction.
        dbtx.discard_commit_hooks();
        dbtx.suppress_materialization_emission();

        let events_temp_table = dbtx.open_table(&events_temp)?;
        let event_sources = dbtx.open_table(&event_sources_temp)?;

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

        info!(target: LOG_TARGET, "Re-processing event envelopes...");

        let mut processed_count = 0u64;
        let mut content_count = 0u64;
        let mut legacy_content_used = 0u64;
        let mut invalid_content_count = 0u64;

        // Establish complete graph and lifecycle state before processing any
        // payload. This is the only required phase order: within this pass,
        // corrected graph reducers converge without parent topology.
        for entry in events_temp_table.range(..)? {
            let (event_id, event_record) = entry?;
            let event_id = event_id.value_try()?;
            let event_record = event_record.value_try()?;
            let timestamp = event_record.timestamp();
            let source = event_sources
                .get(&event_id)?
                .map(|source| source.value_try())
                .transpose()?
                .unwrap_or(EventReceivedSource::Migration);
            let verified_event = VerifiedEvent::assume_verified_from_signed(SignedEvent {
                event: event_record.signed.event,
                sig: event_record.signed.sig,
            });
            let (insert_outcome, _) =
                self.process_event_tx_with_source(&verified_event, timestamp, source, dbtx)?;
            debug!(
                target: LOG_TARGET,
                kind = %event_record.signed.event.kind,
                author = %event_record.signed.event.author.to_short(),
                event_id = %event_id,
                ?insert_outcome,
                "Migration: processed event envelope"
            );
            processed_count += 1;
            if processed_count.is_multiple_of(10000) {
                debug!(
                    target: LOG_TARGET,
                    processed_count,
                    "Migration envelope progress"
                );
            }
            if processed_count.is_multiple_of(100_000) {
                info!(
                    target: LOG_TARGET,
                    processed_count,
                    "Migration envelope progress"
                );
            }
        }

        info!(target: LOG_TARGET, "Re-processing available event content...");

        for entry in events_temp_table.range(..)? {
            let (event_id, event_record) = entry?;
            let event_id = event_id.value_try()?;
            let event_record = event_record.value_try()?;
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

            // Events with content_len==0 have no content to process —
            // insert_event_tx already applied their processed or predeleted
            // lifecycle bookkeeping.
            if event_record.content_len() == 0 {
                continue;
            }
            // Envelope replay has already applied the exclusive size boundary.
            // Do not decode or own a retained payload that normal ingestion
            // would reject.
            if Self::MAX_CONTENT_LEN <= event_record.content_len() {
                continue;
            }
            content_count += 1;

            // Look up content - first try hash-based store, then legacy event-id store
            let content_from_store = if let Some(table) = legacy_content_store_table.as_ref() {
                table
                    .get(&content_hash)?
                    .map(|entry| {
                        let LegacyContentStoreRecord::Present(content) = entry.value_try()?;
                        Ok::<_, bincode::error::DecodeError>(content.into_owned())
                    })
                    .transpose()?
            } else if let Some(table) = new_content_store_table.as_ref() {
                table
                    .get(&content_hash)?
                    .map(|entry| {
                        let crate::event::ContentStoreRecord(content) = entry.value_try()?;
                        Ok::<_, bincode::error::DecodeError>(content.into_owned())
                    })
                    .transpose()?
            } else {
                unreachable!("one migration content-store format is open")
            };

            // Helper to get content from legacy table
            let legacy_content = || -> DbResult<Option<EventContentRaw>> {
                let Some(legacy_table) = legacy_content_temp_table.as_ref() else {
                    return Ok(None);
                };
                let Some(legacy_record) = legacy_table
                    .get(&event_id)?
                    .map(|g| g.value_try())
                    .transpose()?
                else {
                    return Ok(None);
                };
                Ok(match legacy_record {
                    LegacyEventContentState::Present(cow) => Some(cow.as_ref().to_owned()),
                    LegacyEventContentState::Invalid(cow) => {
                        debug!(
                            target: LOG_TARGET,
                            kind = %event_kind,
                            author = %author.to_short(),
                            "Migration: skipping legacy Invalid content"
                        );
                        let _ = cow;
                        None
                    }
                    LegacyEventContentState::Deleted { .. } | LegacyEventContentState::Pruned => {
                        None
                    }
                })
            };

            match content_from_store {
                Some(content_raw) => {
                    match VerifiedEventContent::verify(verified_event, content_raw) {
                        Ok(verified_content) => {
                            // Process content using the same function as normal operation.
                            // Use event timestamp as "now" for migration.
                            self.process_event_content_tx(&verified_content, timestamp, dbtx)?;
                        }
                        Err(err) => {
                            invalid_content_count += 1;
                            debug!(
                                target: LOG_TARGET,
                                kind = %event_kind,
                                author = %author.to_short(),
                                ?err,
                                "Migration: current content-store hash mismatch, skipping"
                            );
                        }
                    }
                }
                None => {
                    // Try legacy table
                    if let Some(content_raw) = legacy_content()? {
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

            if content_count.is_multiple_of(10000) {
                debug!(
                    target: LOG_TARGET,
                    content_count,
                    "Migration content progress"
                );
            }
            if content_count.is_multiple_of(100_000) {
                info!(
                    target: LOG_TARGET,
                    content_count,
                    invalid_content_count,
                    "Migration content progress"
                );
            }
        }

        drop(events_temp_table);
        drop(event_sources);
        drop(legacy_content_store_table);
        drop(new_content_store_table);
        drop(legacy_content_temp_table);

        let replaced_by = dbtx.open_table(&crate::social_posts_replaced_by::TABLE)?;
        let mut replaces = dbtx.open_table(&crate::social_posts_replaces::TABLE)?;
        for entry in replaced_by.range(..)? {
            let (key, _) = entry?;
            let (author, old_event_id, new_event_id) = key.value_try()?;
            replaces.insert(&(author, new_event_id, old_event_id), &())?;
        }
        drop(replaced_by);
        drop(replaces);

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
        // Both legacy and new temp defs have the same table name
        dbtx.as_raw()
            .delete_table(new_content_store_temp.as_raw())?;
        dbtx.as_raw().delete_table(ids_self_temp.as_raw())?;
        dbtx.as_raw().delete_table(db_init_time_temp.as_raw())?;
        dbtx.as_raw().delete_table(replaced_by_temp.as_raw())?;
        dbtx.as_raw().delete_table(materializations_temp.as_raw())?;
        dbtx.as_raw().delete_table(source_ver_temp.as_raw())?;
        dbtx.as_raw().delete_table(event_sources_temp.as_raw())?;
        // Try to delete legacy temp table (may not exist)
        let _ = dbtx
            .as_raw()
            .delete_table(legacy_events_content_temp.as_raw());

        info!(
            target: LOG_TARGET,
            processed_count,
            content_count,
            legacy_content_used,
            invalid_content_count,
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
