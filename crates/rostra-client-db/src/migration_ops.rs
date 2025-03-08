use std::ops::Not as _;

use redb::ReadableTable as _;
use tracing::{debug, info};

use crate::{
    db_version, events, events_by_time, events_content, events_content_missing, events_heads,
    events_missing, events_self, ids_followees, ids_followers, ids_full, ids_personas, ids_self,
    ids_unfollowed, social_posts, social_posts_by_time, social_posts_reactions,
    social_posts_replies, social_posts_v0, social_profiles, social_profiles_v0, Database, DbResult,
    DbVersionTooHighSnafu, IdSocialProfileRecord, Latest, SocialPostRecord, WriteTransactionCtx,
    LOG_TARGET,
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

        tx.open_table(&events::TABLE)?;
        tx.open_table(&events_missing::TABLE)?;
        tx.open_table(&events_by_time::TABLE)?;
        tx.open_table(&events_content::TABLE)?;
        tx.open_table(&events_content_missing::TABLE)?;
        tx.open_table(&events_self::TABLE)?;
        tx.open_table(&events_heads::TABLE)?;

        tx.open_table(&social_profiles::TABLE)?;
        tx.open_table(&social_posts::TABLE)?;
        tx.open_table(&social_posts_by_time::TABLE)?;
        tx.open_table(&social_posts_replies::TABLE)?;
        tx.open_table(&social_posts_reactions::TABLE)?;
        Ok(())
    }

    pub(crate) fn handle_db_ver_migrations(dbtx: &WriteTransactionCtx) -> DbResult<()> {
        const DB_VER: u64 = 2;

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
}
