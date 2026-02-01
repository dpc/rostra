use std::ops::Not as _;

use redb::ReadableTable as _;
use rostra_core::event::PersonaSelector;
use tracing::{debug, info};

use crate::ids::IdsFolloweesRecordV0;
use crate::{
    ContentStoreRecordOwned, Database, DbResult, DbVersionTooHighSnafu, EventContentStateNew,
    IdSocialProfileRecord, IdsDataUsageRecord, IdsFolloweesRecord, LOG_TARGET, Latest,
    SocialPostRecord, WriteTransactionCtx, content_rc, content_store, db_version, events,
    events_by_time, events_content, events_content_missing, events_content_state, events_heads,
    events_missing, events_self, events_singletons, events_singletons_new, ids_data_usage,
    ids_followees, ids_followees_v0, ids_followers, ids_full, ids_personas, ids_self,
    ids_unfollowed, social_posts, social_posts_by_time, social_posts_reactions,
    social_posts_replies, social_posts_v0, social_profiles, social_profiles_v0,
};

impl Database {
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
        tx.open_table(&events_singletons::TABLE)?;
        tx.open_table(&events_singletons_new::TABLE)?;
        tx.open_table(&events_missing::TABLE)?;
        tx.open_table(&events_by_time::TABLE)?;
        tx.open_table(&events_content::TABLE)?;
        tx.open_table(&events_content_missing::TABLE)?;
        tx.open_table(&events_self::TABLE)?;
        tx.open_table(&events_heads::TABLE)?;

        tx.open_table(&content_store::TABLE)?;
        tx.open_table(&content_rc::TABLE)?;
        tx.open_table(&events_content_state::TABLE)?;

        tx.open_table(&social_profiles::TABLE)?;
        tx.open_table(&social_posts::TABLE)?;
        tx.open_table(&social_posts_by_time::TABLE)?;
        tx.open_table(&social_posts_replies::TABLE)?;
        tx.open_table(&social_posts_reactions::TABLE)?;
        Ok(())
    }

    pub(crate) fn handle_db_ver_migrations(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        const DB_VER: u64 = 5;

        let mut table_db_ver = dbtx.open_table(&db_version::TABLE)?;

        let Some(mut cur_db_ver) = table_db_ver.first()?.map(|g| g.1.value()) else {
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

        while cur_db_ver < DB_VER {
            debug!(target: LOG_TARGET, db_ver=%cur_db_ver, "Running migration");
            match cur_db_ver {
                0 => Self::migrate_v0(dbtx)?,
                1 => Self::migrate_v1(dbtx)?,
                2 => Self::migrate_v2(dbtx)?,
                3 => Self::migrate_v3(dbtx)?,
                4 => Self::migrate_v4(dbtx)?,
                DB_VER => { /* ensures we didn't forget to increment DB_VER */ }
                x => panic!("Unexpected db ver: {x}"),
            }

            cur_db_ver += 1;
        }

        table_db_ver.insert(&(), &cur_db_ver)?;
        debug!(target: LOG_TARGET, db_ver = cur_db_ver, "Db version");

        Ok(())
    }

    pub(crate) fn rename_table<KS, VS, KD, VD>(
        dbtx: &WriteTransactionCtx,
        src: &redb_bincode::TableDefinition<'_, KS, VS>,
        dst: &redb_bincode::TableDefinition<'_, KD, VD>,
    ) -> DbResult<()> {
        let mut dst_tbl = dbtx.as_raw().open_table(dst.as_raw())?;
        let mut src_table = dbtx.as_raw().open_table(src.as_raw())?;
        for record in src_table.range::<&[u8]>(..)? {
            let (k, v) = record?;
            dst_tbl.insert(k.value(), v.value())?;
        }
        src_table.retain(|_, _| false)?;
        Ok(())
    }

    pub(crate) fn migrate_v0(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        Self::rename_table(dbtx, &social_profiles::TABLE, &social_profiles_v0::TABLE)?;

        let table_v0 = dbtx.open_table(&social_profiles_v0::TABLE)?;
        let mut table = dbtx.open_table(&social_profiles::TABLE)?;

        for g in table_v0.range(..)? {
            let (k, v_v0) = g?;
            let v_v0 = v_v0.value();
            table.insert(
                &k.value(),
                &Latest {
                    ts: v_v0.ts,
                    inner: IdSocialProfileRecord {
                        event_id: v_v0.inner.event_id,
                        display_name: v_v0.inner.display_name,
                        bio: v_v0.inner.bio,
                        avatar: if !v_v0.inner.img.is_empty() && v_v0.inner.img_mime.is_empty() {
                            Some((v_v0.inner.img_mime, v_v0.inner.img))
                        } else {
                            None
                        },
                    },
                },
            )?;
        }

        drop(table);
        drop(table_v0);

        dbtx.as_raw()
            .delete_table(social_profiles_v0::TABLE.as_raw())?
            .not()
            .then(|| panic!("Expected to delete the table"));
        Ok(())
    }

    pub(crate) fn migrate_v1(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        Self::rename_table(dbtx, &social_posts::TABLE, &social_posts_v0::TABLE)?;

        let table_v0 = dbtx.open_table(&social_posts_v0::TABLE)?;
        let mut table = dbtx.open_table(&social_posts::TABLE)?;

        for g in table_v0.range(..)? {
            let (k, v_v0) = g?;
            let crate::SocialPostRecordV0 { reply_count } = v_v0.value();
            table.insert(
                &k.value(),
                &SocialPostRecord {
                    reply_count,
                    reaction_count: 0,
                },
            )?;
        }

        drop(table);
        drop(table_v0);

        dbtx.as_raw()
            .delete_table(social_posts_v0::TABLE.as_raw())?
            .not()
            .then(|| panic!("Expected to delete the table"));
        Ok(())
    }

    pub(crate) fn migrate_v2(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        Self::rename_table(dbtx, &ids_followees::TABLE, &ids_followees_v0::TABLE)?;

        let table_v0 = dbtx.open_table(&ids_followees_v0::TABLE)?;
        let mut table = dbtx.open_table(&ids_followees::TABLE)?;

        for g in table_v0.range(..)? {
            let (k, v_v0) = g?;
            let IdsFolloweesRecordV0 { ts, persona: _ } = v_v0.value();
            table.insert(
                &k.value(),
                &IdsFolloweesRecord {
                    ts,
                    selector: Some(PersonaSelector::Except { ids: vec![] }),
                },
            )?;
        }

        drop(table);
        drop(table_v0);

        dbtx.as_raw()
            .delete_table(ids_followees_v0::TABLE.as_raw())?
            .not()
            .then(|| panic!("Expected to delete the table"));
        Ok(())
    }

    /// Migrate from events_content (inline content) to content_store (by hash).
    ///
    /// This migration:
    /// 1. For each event with content, stores the content by its content_hash
    ///    in content_store (enabling deduplication)
    /// 2. Tracks reference counts in content_rc
    /// 3. Stores per-event state in events_content_state
    /// 4. Deletes the old events_content and events_content_rc_count tables
    pub(crate) fn migrate_v3(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        use rostra_core::event::EventExt;

        use crate::event::EventContentState;

        info!(target: LOG_TARGET, "Migrating content to content_store (may take a while)...");

        let events_table = dbtx.open_table(&events::TABLE)?;
        let old_content_table = dbtx.open_table(&events_content::TABLE)?;
        let mut store_table = dbtx.open_table(&content_store::TABLE)?;
        let mut rc_table = dbtx.open_table(&content_rc::TABLE)?;
        let mut state_table = dbtx.open_table(&events_content_state::TABLE)?;

        let mut migrated_count = 0u64;
        let mut deduplicated_count = 0u64;

        for entry in old_content_table.range(..)? {
            let (event_id_guard, content_state_guard) = entry?;
            let event_id = event_id_guard.value();
            let content_state = content_state_guard.value();

            // Get the event to find its content_hash
            let Some(event_record_guard) = events_table.get(&event_id)? else {
                // Event not found - skip this orphaned content
                debug!(target: LOG_TARGET, ?event_id, "Orphaned content entry, skipping");
                continue;
            };
            let content_hash = event_record_guard.value().content_hash();

            match content_state {
                EventContentState::Present(content) => {
                    // Check if content already exists in store (deduplication)
                    let existing_rc = rc_table.get(&content_hash)?.map(|g| g.value());

                    if existing_rc.is_none() {
                        // First time seeing this content hash - store it
                        store_table
                            .insert(&content_hash, &ContentStoreRecordOwned::Present(content))?;
                    } else {
                        deduplicated_count += 1;
                    }

                    // Increment reference count
                    let new_rc = existing_rc.unwrap_or(0) + 1;
                    rc_table.insert(&content_hash, &new_rc)?;

                    // Set per-event state
                    state_table.insert(&event_id, &EventContentStateNew::Available)?;
                }
                EventContentState::Invalid(content) => {
                    // Same as Present but marked Invalid
                    let existing_rc = rc_table.get(&content_hash)?.map(|g| g.value());

                    if existing_rc.is_none() {
                        store_table
                            .insert(&content_hash, &ContentStoreRecordOwned::Invalid(content))?;
                    } else {
                        deduplicated_count += 1;
                    }

                    let new_rc = existing_rc.unwrap_or(0) + 1;
                    rc_table.insert(&content_hash, &new_rc)?;
                    state_table.insert(&event_id, &EventContentStateNew::Available)?;
                }
                EventContentState::Deleted { deleted_by } => {
                    // No content to store, just record the state
                    state_table.insert(&event_id, &EventContentStateNew::Deleted { deleted_by })?;
                }
                EventContentState::Pruned => {
                    // No content to store, just record the state
                    state_table.insert(&event_id, &EventContentStateNew::Pruned)?;
                }
            }

            migrated_count += 1;
            if migrated_count % 10000 == 0 {
                debug!(target: LOG_TARGET, migrated_count, deduplicated_count, "Migration progress");
            }
        }

        info!(
            target: LOG_TARGET,
            migrated_count,
            deduplicated_count,
            "Content migration complete"
        );

        // Drop table references before deleting
        drop(events_table);
        drop(old_content_table);
        drop(store_table);
        drop(rc_table);
        drop(state_table);

        // Delete old tables
        if dbtx.as_raw().delete_table(events_content::TABLE.as_raw())? {
            info!(target: LOG_TARGET, "Deleted old events_content table");
        }

        // The old rc table may or may not exist
        if dbtx.as_raw().delete_table(
            redb_bincode::TableDefinition::<rostra_core::ShortEventId, u64>::new(
                "events_content_rc_count",
            )
            .as_raw(),
        )? {
            info!(target: LOG_TARGET, "Deleted old events_content_rc_count table");
        }

        Ok(())
    }

    /// Migration v4: Calculate initial data usage per identity.
    ///
    /// Iterates all events to calculate:
    /// 1. metadata_size: count of events × EVENT_METADATA_SIZE (192 bytes)
    /// 2. content_size: sum of content_len for events in Available state
    pub(crate) fn migrate_v4(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        use std::collections::HashMap;

        use rostra_core::event::EventExt as _;

        /// Size of event metadata in bytes (Event struct + signature).
        /// See rostra_core::event::Event documentation.
        const EVENT_METADATA_SIZE: u64 = 192;

        let events_table = dbtx.open_table(&events::TABLE)?;
        let state_table = dbtx.open_table(&events_content_state::TABLE)?;
        let mut usage_table = dbtx.open_table(&ids_data_usage::TABLE)?;

        // Collect usage per author
        let mut usage_map: HashMap<rostra_core::id::RostraId, IdsDataUsageRecord> = HashMap::new();

        for entry in events_table.range(..)? {
            let (event_id, record) = entry?;
            let event_id = event_id.value();
            let record = record.value();
            let author = record.author();

            let usage = usage_map.entry(author).or_default();

            // Every event contributes to metadata size
            usage.metadata_size += EVENT_METADATA_SIZE;

            // Content only counts if state is Available
            if let Some(state) = state_table.get(&event_id)?.map(|g| g.value()) {
                if matches!(state, EventContentStateNew::Available) {
                    usage.content_size += u64::from(record.content_len());
                }
            }
        }

        // Write aggregated usage to table
        let mut count = 0u64;
        for (author, usage) in usage_map {
            usage_table.insert(&author, &usage)?;
            count += 1;
        }

        info!(
            target: LOG_TARGET,
            "Calculated data usage for {} identities",
            count
        );

        Ok(())
    }
}
