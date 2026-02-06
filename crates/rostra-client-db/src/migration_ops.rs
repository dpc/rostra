//! Database migration operations.
//!
//! This module handles schema versioning and migrations. The approach is
//! simple: if the database version is older than the current schema version, we
//! perform a "total migration" that re-derives all state from the
//! source-of-truth tables.

use redb::{ReadableTable as _, TableHandle as _};
use rostra_core::event::content_kind;
use tracing::{debug, info};

use crate::id_self::IdSelfAccountRecord;
use crate::{
    ContentStoreRecordOwned, Database, DbResult, DbVersionTooHighSnafu, IdSocialProfileRecord,
    LOG_TARGET, SocialPostRecord, WriteTransactionCtx, content_rc, content_store, db_version,
    events, events_by_time, events_content_missing, events_content_state, events_heads,
    events_missing, events_received_at, events_self, events_singletons_new, ids_data_usage,
    ids_followees, ids_followers, ids_full, ids_personas, ids_self, ids_unfollowed, social_posts,
    social_posts_by_received_at, social_posts_by_time, social_posts_reactions,
    social_posts_replies, social_profiles,
};

impl Database {
    /// Initialize all current schema tables.
    pub(crate) fn init_tables_tx(tx: &WriteTransactionCtx) -> DbResult<()> {
        tx.open_table(&db_version::TABLE)?;

        tx.open_table(&ids_self::TABLE)?;
        tx.open_table(&ids_full::TABLE)?;
        tx.open_table(&ids_followers::TABLE)?;
        tx.open_table(&ids_followees::TABLE)?;
        tx.open_table(&ids_unfollowed::TABLE)?;
        tx.open_table(&ids_personas::TABLE)?;
        tx.open_table(&ids_data_usage::TABLE)?;

        tx.open_table(&events::TABLE)?;
        tx.open_table(&events_singletons_new::TABLE)?;
        tx.open_table(&events_missing::TABLE)?;
        tx.open_table(&events_by_time::TABLE)?;
        tx.open_table(&events_content_missing::TABLE)?;
        tx.open_table(&events_self::TABLE)?;
        tx.open_table(&events_heads::TABLE)?;

        tx.open_table(&content_store::TABLE)?;
        tx.open_table(&content_rc::TABLE)?;
        tx.open_table(&events_content_state::TABLE)?;
        tx.open_table(&events_received_at::TABLE)?;

        tx.open_table(&social_profiles::TABLE)?;
        tx.open_table(&social_posts::TABLE)?;
        tx.open_table(&social_posts_by_time::TABLE)?;
        tx.open_table(&social_posts_by_received_at::TABLE)?;
        tx.open_table(&social_posts_replies::TABLE)?;
        tx.open_table(&social_posts_reactions::TABLE)?;
        Ok(())
    }

    /// Handle database version check and migrations.
    ///
    /// Migration strategy:
    /// - Versions older than `DB_VER_REQUIRES_TOTAL_MIGRATION` get a total
    ///   migration that rebuilds all derived state from source-of-truth tables.
    /// - Newer versions get incremental migrations one step at a time.
    pub(crate) fn handle_db_ver_migrations(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        /// Current schema version.
        ///
        /// Increment this when making schema changes that require migration.
        const DB_VER: u64 = 10;

        /// Versions older than this require a total migration.
        ///
        /// This should be set to the version where we last did a major schema
        /// overhaul. Older databases get rebuilt from scratch.
        const DB_VER_REQUIRES_TOTAL_MIGRATION: u64 = 10;

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
        }

        // Drop the db_version table handle before migrations
        drop(table_db_ver);

        if cur_db_ver < DB_VER_REQUIRES_TOTAL_MIGRATION {
            // Old database - do total migration
            info!(
                target: LOG_TARGET,
                from_ver = cur_db_ver,
                to_ver = DB_VER,
                "Database schema very old, performing total migration"
            );

            Self::total_migration(dbtx)?;
        }
        // Incremental migrations from cur_db_ver to DB_VER
        info!(
            target: LOG_TARGET,
            from_ver = cur_db_ver,
            to_ver = DB_VER,
            "Performing incremental migrations"
        );

        // Future incremental migrations go here, e.g.:
        // if cur_db_ver < 8 {
        //     Self::migrate_v7_to_v8(dbtx)?;
        // }
        // if cur_db_ver < 9 {
        //     Self::migrate_v8_to_v9(dbtx)?;
        // }

        // Re-open and update version
        let mut table_db_ver = dbtx.open_table(&db_version::TABLE)?;
        table_db_ver.insert(&(), &DB_VER)?;

        debug!(target: LOG_TARGET, db_ver = DB_VER, "New database version");

        Ok(())
    }

    // ========================================================================
    // Total Migration
    // ========================================================================

    /// Prefix used for temporary tables during total migration.
    const MIGRATION_TEMP_PREFIX: &'static str = "_total_migration_";

    /// Performs a total migration by re-deriving all state from events and
    /// content.
    ///
    /// This re-processes all events to rebuild derived tables (follows, posts,
    /// profiles, indexes, etc.) from the source-of-truth data.
    ///
    /// # Process
    ///
    /// 1. Copies preserved tables (events, content_store, ids_self) to temp
    ///    names
    /// 2. Deletes ALL tables except temp ones and db_version
    /// 3. Initializes fresh tables with current schema
    /// 4. Restores ids_self from temp
    /// 5. Re-processes all events (sorted by timestamp) to rebuild derived
    ///    state
    /// 6. Deletes temp tables
    pub(crate) fn total_migration(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        use rostra_core::event::{EventExt as _, EventKind, SignedEvent};
        use rostra_core::id::ToShort as _;

        use crate::event::ContentStoreRecord;
        use crate::tables::{SocialPostsReactionsRecord, SocialPostsRepliesRecord};

        info!(target: LOG_TARGET, "Starting total migration - this may take a while...");

        // Define temp table definitions with same types as originals
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

        // Step 1: Copy preserved tables to temp
        info!(target: LOG_TARGET, "Copying preserved tables to temp...");
        Self::copy_table_raw(dbtx, &events::TABLE, &events_temp)?;
        Self::copy_table_raw(dbtx, &content_store::TABLE, &content_store_temp)?;
        Self::copy_table_raw(dbtx, &ids_self::TABLE, &ids_self_temp)?;

        // Step 2: Delete all tables except temp and db_version
        info!(target: LOG_TARGET, "Deleting old tables...");
        let table_names: Vec<String> = dbtx
            .as_raw()
            .list_tables()?
            .map(|h| h.name().to_string())
            .collect();

        for name in &table_names {
            // Keep temp tables and db_version
            if name.starts_with(Self::MIGRATION_TEMP_PREFIX) || name == "db_version" {
                continue;
            }
            // Delete by creating a raw table definition with the name
            let raw_def = redb::TableDefinition::<&[u8], &[u8]>::new(name);
            if dbtx.as_raw().delete_table(raw_def)? {
                debug!(target: LOG_TARGET, table = %name, "Deleted table");
            }
        }

        // Step 3: Initialize fresh tables with current schema
        info!(target: LOG_TARGET, "Initializing fresh tables...");
        Self::init_tables_tx(dbtx)?;

        // Step 4: Restore ids_self from temp
        {
            let temp_table = dbtx.open_table(&ids_self_temp)?;
            let mut ids_self_table = dbtx.open_table(&ids_self::TABLE)?;
            if let Some(record) = temp_table.get(&())?.map(|g| g.value()) {
                ids_self_table.insert(&(), &record)?;
            }
        }

        // Step 5: Collect events and sort by timestamp for ordered re-processing
        info!(target: LOG_TARGET, "Collecting events for re-processing...");
        let events_to_process: Vec<_> = {
            let events_temp_table = dbtx.open_table(&events_temp)?;
            events_temp_table
                .range(..)?
                .map(|r| r.map(|(k, v)| (k.value(), v.value())))
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut events_to_process = events_to_process;
        events_to_process.sort_by_key(|(_, record)| record.signed.event.timestamp);

        info!(
            target: LOG_TARGET,
            event_count = events_to_process.len(),
            "Re-processing events..."
        );

        // Open all required tables for event processing
        let mut ids_full_table = dbtx.open_table(&ids_full::TABLE)?;
        let mut events_table = dbtx.open_table(&events::TABLE)?;
        let mut events_missing_table = dbtx.open_table(&events_missing::TABLE)?;
        let mut events_heads_table = dbtx.open_table(&events_heads::TABLE)?;
        let mut events_by_time_table = dbtx.open_table(&events_by_time::TABLE)?;
        let mut events_content_state_table = dbtx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = dbtx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = dbtx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table = dbtx.open_table(&events_content_missing::TABLE)?;
        let mut ids_data_usage_table = dbtx.open_table(&ids_data_usage::TABLE)?;
        let content_temp_table = dbtx.open_table(&content_store_temp)?;

        // Tables for content side effects
        let mut social_profiles_table = dbtx.open_table(&social_profiles::TABLE)?;
        let mut social_posts_table = dbtx.open_table(&social_posts::TABLE)?;
        let mut social_posts_by_time_table = dbtx.open_table(&social_posts_by_time::TABLE)?;
        let mut social_posts_replies_table = dbtx.open_table(&social_posts_replies::TABLE)?;
        let mut social_posts_reactions_table = dbtx.open_table(&social_posts_reactions::TABLE)?;
        let mut followees_table = dbtx.open_table(&ids_followees::TABLE)?;
        let mut followers_table = dbtx.open_table(&ids_followers::TABLE)?;
        let mut unfollowed_table = dbtx.open_table(&ids_unfollowed::TABLE)?;

        let mut processed_count = 0u64;

        for (_event_id, event_record) in &events_to_process {
            let content_hash = event_record.content_hash();
            let author = event_record.author();
            let timestamp = event_record.timestamp();
            let event_id = event_record.signed.compute_short_id();

            // Check if content exists in temp store
            let content = content_temp_table.get(&content_hash)?.map(|g| g.value());

            // If content exists and not yet in new store, copy it first
            if let Some(ref content_record) = content {
                if content_store_table.get(&content_hash)?.is_none() {
                    content_store_table.insert(&content_hash, content_record)?;
                }
            }

            // Create VerifiedEvent from stored SignedEvent
            // Events in the database have already been verified when originally stored.
            let signed_event = SignedEvent {
                event: event_record.signed.event,
                sig: event_record.signed.sig,
            };
            let verified_event =
                rostra_core::event::VerifiedEvent::assume_verified_from_signed(signed_event);

            // Insert event (handles DAG, RC, missing tracking, etc.)
            Database::insert_event_tx(
                verified_event,
                &mut ids_full_table,
                &mut events_table,
                &mut events_missing_table,
                &mut events_heads_table,
                &mut events_by_time_table,
                &mut events_content_state_table,
                &content_store_table,
                &mut content_rc_table,
                &mut events_content_missing_table,
                Some(&mut ids_data_usage_table),
            )?;

            // If content exists and is valid, process side effects
            if let Some(ContentStoreRecord::Present(content_data)) = content {
                let content_raw = content_data.into_owned();

                // Remove from missing now that we have content
                events_content_missing_table.remove(&event_id)?;

                // Handle different event kinds
                match event_record.kind() {
                    EventKind::SOCIAL_PROFILE_UPDATE => {
                        if let Ok(profile) =
                            content_raw.deserialize_cbor::<content_kind::SocialProfileUpdate>()
                        {
                            Database::insert_latest_value_tx(
                                timestamp,
                                &author,
                                IdSocialProfileRecord {
                                    event_id,
                                    display_name: profile.display_name,
                                    bio: profile.bio,
                                    avatar: profile.avatar,
                                },
                                &mut social_profiles_table,
                            )?;
                        }
                    }
                    EventKind::SOCIAL_POST => {
                        if let Ok(post) = content_raw.deserialize_cbor::<content_kind::SocialPost>()
                        {
                            // Insert into social_posts if not exists
                            if social_posts_table.get(&event_id)?.is_none() {
                                social_posts_table
                                    .insert(&event_id, &SocialPostRecord::default())?;
                            }
                            social_posts_by_time_table.insert(&(timestamp, event_id), &())?;

                            // Handle reply (and reaction - reactions are a subset of replies)
                            if let Some(reply_to) = &post.reply_to {
                                let reply_to_id = reply_to.event_id().to_short();

                                // Check if this is a reaction (emoji reply)
                                if post.get_reaction().is_some() {
                                    social_posts_reactions_table.insert(
                                        &(reply_to_id, timestamp, event_id),
                                        &SocialPostsReactionsRecord,
                                    )?;

                                    // Increment reaction count on target
                                    let target_record = {
                                        let guard = social_posts_table.get(&reply_to_id)?;
                                        guard.map(|g| g.value())
                                    };
                                    if let Some(mut record) = target_record {
                                        record.reaction_count += 1;
                                        social_posts_table.insert(&reply_to_id, &record)?;
                                    }
                                } else {
                                    // Regular reply
                                    social_posts_replies_table.insert(
                                        &(reply_to_id, timestamp, event_id),
                                        &SocialPostsRepliesRecord,
                                    )?;

                                    // Increment reply count on parent
                                    let parent_record = {
                                        let guard = social_posts_table.get(&reply_to_id)?;
                                        guard.map(|g| g.value())
                                    };
                                    if let Some(mut record) = parent_record {
                                        record.reply_count += 1;
                                        social_posts_table.insert(&reply_to_id, &record)?;
                                    }
                                }
                            }
                        }
                    }
                    EventKind::FOLLOW => {
                        // Follow content handles both follow and unfollow via is_unfollow()
                        if let Ok(follow) = content_raw.deserialize_cbor::<content_kind::Follow>() {
                            if follow.is_unfollow() {
                                Database::insert_unfollow_tx(
                                    author,
                                    timestamp,
                                    follow.followee,
                                    &mut followees_table,
                                    &mut followers_table,
                                    &mut unfollowed_table,
                                )?;
                            } else {
                                Database::insert_follow_tx(
                                    author,
                                    timestamp,
                                    follow,
                                    &mut followees_table,
                                    &mut followers_table,
                                    &mut unfollowed_table,
                                )?;
                            }
                        }
                    }
                    _ => {
                        // Other event kinds don't have content side effects we
                        // track
                    }
                }
            }

            processed_count += 1;
            if processed_count % 10000 == 0 {
                debug!(
                    target: LOG_TARGET,
                    processed_count,
                    total = events_to_process.len(),
                    "Migration progress"
                );
            }
        }

        // Step 6: Delete temp tables
        info!(target: LOG_TARGET, "Cleaning up temp tables...");
        drop(content_temp_table);

        dbtx.as_raw().delete_table(events_temp.as_raw())?;
        dbtx.as_raw().delete_table(content_store_temp.as_raw())?;
        dbtx.as_raw().delete_table(ids_self_temp.as_raw())?;

        info!(
            target: LOG_TARGET,
            processed_count, "Total migration complete"
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
}
