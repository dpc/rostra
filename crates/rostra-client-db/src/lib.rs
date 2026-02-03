mod events_content_missing_ops;
mod id_nodes_ops;
mod migration_ops;
mod models;
mod paginate;
mod process_event_content_ops;
mod process_event_ops;
pub mod social;
mod table_ops;
mod tables;
mod tx_ops;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::{io, ops, result};

use event::ContentStoreRecord;
pub use ids::{IdsFolloweesRecord, IdsFollowersRecord};
use itertools::Itertools as _;
use process_event_content_ops::ProcessEventError;
use redb_bincode::{ReadTransaction, ReadableTable, WriteTransaction};
use rostra_core::event::{
    EventAuxKey, EventContentRaw, EventExt as _, EventKind, IrohNodeId, PersonaSelector,
    VerifiedEvent, VerifiedEventContent, content_kind,
};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::{ShortEventId, Timestamp};
use rostra_util_error::{BoxedError, FmtCompact as _};
use snafu::{Location, ResultExt as _, Snafu};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinError;
use tracing::{debug, info, instrument};

pub use self::tables::*;

const LOG_TARGET: &str = "rostra::db";

pub struct WriteTransactionCtx {
    dbtx: WriteTransaction,
    on_commit: std::sync::Mutex<Vec<Box<dyn FnOnce() + 'static>>>,
}

impl From<WriteTransaction> for WriteTransactionCtx {
    fn from(dbtx: WriteTransaction) -> Self {
        Self {
            dbtx,
            on_commit: std::sync::Mutex::new(vec![]),
        }
    }
}
impl ops::Deref for WriteTransactionCtx {
    type Target = WriteTransaction;

    fn deref(&self) -> &Self::Target {
        &self.dbtx
    }
}

impl ops::DerefMut for WriteTransactionCtx {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.dbtx
    }
}

impl WriteTransactionCtx {
    pub fn on_commit(&self, f: impl FnOnce() + 'static) {
        self.on_commit
            .lock()
            .expect("Locking failed")
            .push(Box::new(f));
    }

    fn commit(self) -> result::Result<(), redb::CommitError> {
        let Self { dbtx, on_commit } = self;

        dbtx.commit()?;

        for hook in on_commit.lock().expect("Locking failed").drain(..) {
            hook();
        }
        Ok(())
    }
}

#[derive(Debug, Snafu)]
pub enum TableDumpError {
    #[snafu(display("Unknown table `{name}`"))]
    UnknownTable { name: String },
}
pub type TableDumpResult<T> = std::result::Result<T, TableDumpError>;

#[derive(Debug, Snafu)]
pub enum DbError {
    Database {
        source: redb::DatabaseError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    Table {
        source: redb::TableError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    Storage {
        source: redb::StorageError,
        #[snafu(implicit)]
        location: Location,
    },
    Transaction {
        #[snafu(source(from(redb::TransactionError, Box::new)))]
        source: Box<redb::TransactionError>,
        #[snafu(implicit)]
        location: Location,
    },
    Commit {
        source: redb::CommitError,
        #[snafu(implicit)]
        location: Location,
    },
    DbVersionTooHigh {
        db_ver: u64,
        code_ver: u64,
        #[snafu(implicit)]
        location: Location,
    },
    Join {
        source: JoinError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    DbTxLogic {
        source: BoxedError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(visibility(pub))]
    #[snafu(display("Provided Id does not match one used previously"))]
    DbIdMismatch {
        #[snafu(implicit)]
        location: Location,
    },
    Overflow,
}
pub type DbResult<T> = std::result::Result<T, DbError>;

#[derive(Debug)]
pub struct Database {
    inner: redb_bincode::Database,
    self_id: RostraId,
    iroh_secret: iroh::SecretKey,

    self_followees_updated: watch::Sender<HashMap<RostraId, IdsFolloweesRecord>>,
    self_followers_updated: watch::Sender<HashMap<RostraId, IdsFollowersRecord>>,
    self_head_updated: watch::Sender<Option<ShortEventId>>,
    new_content_tx: broadcast::Sender<VerifiedEventContent>,
    new_posts_tx: broadcast::Sender<(VerifiedEventContent, content_kind::SocialPost)>,
    ids_with_missing_events_tx: dedup_chan::Sender<RostraId>,
}

impl Database {
    const MAX_CONTENT_LEN: u32 = 10_000_000u32;
    pub async fn mk_db_path(
        data_dir: &Path,
        self_id: RostraId,
    ) -> std::result::Result<PathBuf, io::Error> {
        tokio::fs::create_dir_all(&data_dir).await?;

        let legacy_path_unprefixed_z32 =
            data_dir.join(format!("{}.redb", self_id.to_unprefixed_z32_string()));
        if legacy_path_unprefixed_z32.exists() {
            return Ok(legacy_path_unprefixed_z32);
        }
        let legacy_path_bech32 = data_dir.join(format!("{}.redb", self_id.to_bech32_string()));
        if legacy_path_bech32.exists() {
            return Ok(legacy_path_bech32);
        }
        Ok(data_dir.join(format!("{self_id}.redb")))
    }

    pub async fn new_in_memory(self_id: RostraId) -> DbResult<Database> {
        debug!(target: LOG_TARGET, id = %self_id, "Opening in-memory database");
        let inner = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .context(DatabaseSnafu)?;
        Self::open_inner(inner, self_id).await
    }

    pub async fn open(path: impl Into<PathBuf>, self_id: RostraId) -> DbResult<Database> {
        let path = path.into();
        debug!(target: LOG_TARGET, id = %self_id, path = %path.display(), "Opening database");

        let inner = tokio::task::spawn_blocking(move || redb::Database::create(path))
            .await
            .context(JoinSnafu)?
            .context(DatabaseSnafu)?;

        Self::open_inner(inner, self_id).await
    }

    #[instrument(skip_all)]
    async fn open_inner(inner: redb::Database, self_id: RostraId) -> DbResult<Database> {
        let inner = redb_bincode::Database::from(inner);
        Self::write_with_inner(&inner, |tx| {
            Self::init_tables_tx(tx)?;
            Self::verify_self_tx(self_id, &mut tx.open_table(&ids_self::TABLE)?)?;
            Self::handle_db_ver_migrations(tx)?;
            Ok(())
        })
        .await?;

        let (self_head, iroh_secret, self_followees, self_followers) =
            Self::read_with_inner(&inner, |tx| {
                Ok((
                    Self::read_head_tx(self_id, &tx.open_table(&events_heads::TABLE)?)?,
                    Self::read_iroh_secret_tx(&tx.open_table(&ids_self::TABLE)?)?,
                    Self::read_followees_tx(self_id, &tx.open_table(&ids_followees::TABLE)?)?,
                    Self::read_followers_tx(self_id, &tx.open_table(&ids_followers::TABLE)?)?,
                ))
            })
            .await?;

        let (self_followees_updated, _) = watch::channel(self_followees);
        let (self_followers_updated, _) = watch::channel(self_followers);
        let (self_head_updated, _) = watch::channel(self_head);
        let (new_content_tx, _) = broadcast::channel(100);
        let (new_posts_tx, _) = broadcast::channel(100);

        let s = Self {
            inner,
            self_id,
            iroh_secret,
            self_followees_updated,
            self_followers_updated,
            self_head_updated,
            new_content_tx,
            new_posts_tx,
            ids_with_missing_events_tx: dedup_chan::Sender::new(),
        };

        Ok(s)
    }

    pub async fn compact(&mut self) -> Result<bool, redb::CompactionError> {
        tokio::task::block_in_place(|| self.inner.as_raw_mut().compact())
    }

    pub async fn dump_table(&self, name: &str) -> TableDumpResult<()> {
        self.read_with(|tx| {
            match name {
                "events" => Self::dump_table_dbtx(tx, &tables::events::TABLE)?,
                "events_singletons" => {
                    Self::dump_table_dbtx(tx, &tables::events_singletons::TABLE)?
                }
                "content_store" => Self::dump_table_dbtx(tx, &tables::content_store::TABLE)?,
                "events_content_state" => {
                    Self::dump_table_dbtx(tx, &tables::events_content_state::TABLE)?
                }
                "events_content_missing" => {
                    Self::dump_table_dbtx(tx, &tables::events_content_missing::TABLE)?
                }
                "social_posts" => Self::dump_table_dbtx(tx, &tables::social_posts::TABLE)?,
                "social_posts_replies" => {
                    Self::dump_table_dbtx(tx, &tables::social_posts_replies::TABLE)?
                }
                "social_posts_reactions" => {
                    Self::dump_table_dbtx(tx, &tables::social_posts_reactions::TABLE)?
                }
                _ => {
                    return Ok(Err(UnknownTableSnafu {
                        name: name.to_string(),
                    }
                    .build()));
                }
            }
            Ok(Ok(()))
        })
        .await
        .expect("Database panic")
    }

    pub fn self_followees_subscribe(
        &self,
    ) -> watch::Receiver<HashMap<RostraId, IdsFolloweesRecord>> {
        self.self_followees_updated.subscribe()
    }

    pub fn self_followers_subscribe(
        &self,
    ) -> watch::Receiver<HashMap<RostraId, IdsFollowersRecord>> {
        self.self_followers_updated.subscribe()
    }

    pub fn self_head_subscribe(&self) -> watch::Receiver<Option<ShortEventId>> {
        self.self_head_updated.subscribe()
    }

    pub fn new_content_subscribe(&self) -> broadcast::Receiver<VerifiedEventContent> {
        self.new_content_tx.subscribe()
    }
    pub fn new_posts_subscribe(
        &self,
    ) -> broadcast::Receiver<(VerifiedEventContent, content_kind::SocialPost)> {
        self.new_posts_tx.subscribe()
    }
    pub fn ids_with_missing_events_subscribe(
        &self,
        capacity: usize,
    ) -> dedup_chan::Receiver<RostraId> {
        self.ids_with_missing_events_tx.subscribe(capacity)
    }

    pub async fn has_event(&self, event_id: impl Into<ShortEventId>) -> bool {
        let event_id = event_id.into();
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE).expect("Storage error");
            Database::has_event_tx(event_id, &events_table)
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_missing_events_for_id(&self, id: RostraId) -> Vec<ShortEventId> {
        self.read_with(|tx| {
            let events_missing_tbl = tx.open_table(&events_missing::TABLE)?;
            Ok(
                Database::get_missing_events_for_id_tx(id, &events_missing_tbl)?
                    .into_iter()
                    .collect(),
            )
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_self_followees(&self) -> Vec<(RostraId, PersonaSelector)> {
        self.get_followees(self.self_id).await
    }

    pub async fn get_followees(&self, id: RostraId) -> Vec<(RostraId, PersonaSelector)> {
        self.read_with(|tx| {
            let ids_followees_table = tx.open_table(&ids_followees::TABLE)?;
            Ok(Database::read_followees_tx(id, &ids_followees_table)?
                .into_iter()
                .filter_map(|(id, record)| record.selector.map(|selector| (id, selector)))
                .collect())
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_followees_extended(
        &self,
        id: RostraId,
    ) -> (HashMap<RostraId, PersonaSelector>, HashSet<RostraId>) {
        self.read_with(|tx| {
            let ids_followees_table = tx.open_table(&ids_followees::TABLE)?;
            let followees: HashMap<RostraId, PersonaSelector> =
                Database::read_followees_tx_iter(id, &ids_followees_table)?
                    .filter_map_ok(|(id, record)| record.selector.map(|selector| (id, selector)))
                    .collect::<Result<_, _>>()?;

            let mut extended = HashSet::new();

            for followee in followees.keys() {
                for extended_followee in
                    Database::read_followees_tx_iter(*followee, &ids_followees_table)?
                        .map_ok(|(id, record)| (id, record.selector))
                {
                    let extended_followee = extended_followee?.0;
                    if !followees.contains_key(&extended_followee) {
                        extended.insert(extended_followee);
                    }
                }
            }
            Ok((followees, extended))
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_self_followers(&self) -> Vec<RostraId> {
        self.get_followers(self.self_id).await
    }

    pub async fn get_followers(&self, id: RostraId) -> Vec<RostraId> {
        self.read_with(|tx| {
            let ids_followers_table = tx.open_table(&ids_followers::TABLE)?;
            Ok(Database::read_followers_tx(id, &ids_followers_table)?
                .into_keys()
                .collect())
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_event(
        &self,
        event_id: impl Into<ShortEventId>,
    ) -> Option<crate::event::EventRecord> {
        let event_id = event_id.into();
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            Database::get_event_tx(event_id, &events_table)
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_event_content(
        &self,
        event_id: impl Into<ShortEventId>,
    ) -> Option<EventContentRaw> {
        let event_id = event_id.into();
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            // Get the event to find its content hash
            let Some(event_record) = Database::get_event_tx(event_id, &events_table)? else {
                return Ok(None);
            };
            let content_hash = event_record.content_hash();

            Ok(Database::get_event_content_full_tx(
                event_id,
                content_hash,
                &events_content_state_table,
                &content_store_table,
            )?
            .and_then(|result| result.content().cloned()))
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_self_current_head(&self) -> Option<ShortEventId> {
        self.read_with(|tx| {
            let events_heads_table = tx.open_table(&events_heads::TABLE)?;

            Database::read_head_tx(self.self_id, &events_heads_table)
        })
        .await
        .expect("Storage error")
    }

    pub async fn get_self_random_eventid(&self) -> Option<ShortEventId> {
        self.read_with(|tx| {
            let events_self_table = tx.open_table(&events_self::TABLE)?;

            Database::get_random_self_event(&events_self_table)
        })
        .await
        .expect("Storage error")
    }

    pub async fn process_event(
        &self,
        event: &VerifiedEvent,
    ) -> (InsertEventOutcome, ProcessEventState) {
        self.write_with(|tx| self.process_event_tx(event, tx))
            .await
            .expect("Storage error")
    }

    pub async fn process_event_with_content(
        &self,
        content: &VerifiedEventContent,
    ) -> (InsertEventOutcome, ProcessEventState) {
        self.write_with(|tx| {
            let res = self.process_event_tx(&content.event, tx)?;
            self.process_event_content_tx(content, tx)?;
            Ok(res)
        })
        .await
        .expect("Storage error")
    }

    /// Process event content
    ///
    /// Note: Must only be called for an event that was already processed
    pub async fn process_event_content(&self, event_content: &VerifiedEventContent) {
        self.write_with(|tx| self.process_event_content_tx(event_content, tx))
            .await
            .expect("Storage error")
    }

    pub fn process_event_content_tx(
        &self,
        event_content: &VerifiedEventContent,
        tx: &WriteTransactionCtx,
    ) -> DbResult<()> {
        let events_table = tx.open_table(&events::TABLE)?;
        let mut events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
        let mut content_rc_table = tx.open_table(&content_rc::TABLE)?;
        let mut events_content_missing_table =
            tx.open_table(&tables::events_content_missing::TABLE)?;
        let mut ids_data_usage_table = tx.open_table(&ids_data_usage::TABLE)?;

        debug_assert!(Database::has_event_tx(
            event_content.event.event_id,
            &events_table
        )?);

        events_content_missing_table.remove(&event_content.event_id().to_short())?;

        let can_insert = if u32::from(event_content.event.event.content_len) < Self::MAX_CONTENT_LEN
        {
            Database::can_insert_event_content_tx(
                event_content,
                &events_content_state_table,
                &content_store_table,
            )?
        } else {
            false
        };

        if can_insert {
            if let Some(content) = event_content.content.as_ref() {
                let content_hash = event_content.content_hash();

                // Check if this event was already claimed (content deduplicated) by
                // insert_event_tx. In that case, RC and content size were
                // already incremented, content is already in store - we just
                // need to run event-specific processing and update state.
                let was_claimed_early = events_content_state_table
                    .get(&event_content.event_id().to_short())?
                    .map(|g| matches!(g.value(), EventContentStateNew::ClaimedUnprocessed))
                    .unwrap_or(false);

                let is_valid = match self.process_event_content_inserted_tx(event_content, tx) {
                    Ok(()) => {
                        info!(target: LOG_TARGET,
                            kind = %event_content.kind(),
                            event_id = %event_content.event_id().to_short(),
                            author = %event_content.author().to_short(),
                            len = %event_content.content_len(),
                            "New event content inserted"
                        );
                        true
                    }
                    Err(ProcessEventError::Invalid { source, location }) => {
                        info!(
                            target: LOG_TARGET,
                            err = %source.as_ref().fmt_compact(),
                            %location,
                            "Invalid event content"
                        );
                        false
                    }
                    Err(ProcessEventError::Db { source }) => {
                        return Err(source);
                    }
                };

                // Only do content storage and RC tracking if not already claimed
                if !was_claimed_early {
                    // Check if content already exists in store (deduplication)
                    let content_exists = content_store_table.get(&content_hash)?.is_some();

                    if !content_exists {
                        // Store content in content_store
                        let store_record = if is_valid {
                            ContentStoreRecord::Present(Cow::Owned(content.clone()))
                        } else {
                            ContentStoreRecord::Invalid(Cow::Owned(content.clone()))
                        };
                        content_store_table.insert(&content_hash, &store_record)?;
                    }

                    // Increment RC for this event claiming the content
                    Database::increment_content_rc_tx(content_hash, &mut content_rc_table)?;

                    // Track content size for the author
                    Database::increment_content_size_tx(
                        event_content.author(),
                        event_content.content_len(),
                        &mut ids_data_usage_table,
                    )?;
                }

                // Mark per-event state as available (this event now holds an RC)
                events_content_state_table.insert(
                    &event_content.event_id().to_short(),
                    &EventContentStateNew::Available,
                )?;

                // Valid or not, we notify about new thing
                tx.on_commit({
                    let new_content_tx = self.new_content_tx.clone();
                    let event_content = event_content.clone();
                    move || {
                        let _ = new_content_tx.send(event_content);
                    }
                });
            }
        }
        Ok(())
    }

    pub async fn wants_content(
        &self,
        event_id: impl Into<ShortEventId>,
        process_state: ProcessEventState,
    ) -> bool {
        match process_state.wants_content() {
            ContentWantState::DoesNotWant => {
                return false;
            }
            ContentWantState::Wants => {
                return true;
            }
            ContentWantState::MaybeWants => {}
        }

        self.read_with(|tx| {
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;

            Database::has_event_content_tx(event_id.into(), &events_content_state_table)
        })
        .await
        .expect("Storage error")
    }

    pub fn iroh_secret(&self) -> iroh::SecretKey {
        self.iroh_secret.clone()
    }
}

impl Database {
    pub async fn write_with_inner<T>(
        inner: &redb_bincode::Database,
        f: impl FnOnce(&'_ WriteTransactionCtx) -> DbResult<T>,
    ) -> DbResult<T> {
        tokio::task::block_in_place(|| {
            let mut dbtx =
                WriteTransactionCtx::from(inner.begin_write().context(TransactionSnafu)?);
            let res = f(&mut dbtx)?;

            dbtx.commit().context(CommitSnafu)?;

            Ok(res)
        })
    }
    pub async fn write_with<T>(
        &self,
        f: impl FnOnce(&'_ WriteTransactionCtx) -> DbResult<T>,
    ) -> DbResult<T> {
        Self::write_with_inner(&self.inner, f).await
    }

    pub async fn read_with_inner<T>(
        inner: &redb_bincode::Database,
        f: impl FnOnce(&'_ ReadTransaction) -> DbResult<T>,
    ) -> DbResult<T> {
        tokio::task::block_in_place(|| {
            let mut dbtx = inner.begin_read().context(TransactionSnafu)?;

            f(&mut dbtx)
        })
    }

    pub async fn read_with<T>(
        &self,
        f: impl FnOnce(&'_ ReadTransaction) -> DbResult<T>,
    ) -> DbResult<T> {
        Self::read_with_inner(&self.inner, f).await
    }

    pub fn verify_self_tx(self_id: RostraId, ids_self_t: &mut ids_self::Table) -> DbResult<()> {
        match Self::read_self_id_tx(ids_self_t)? {
            Some(existing_self_id_record) => {
                if existing_self_id_record.rostra_id != self_id {
                    return DbIdMismatchSnafu.fail();
                }
            }
            _ => {
                Self::write_self_id_tx(self_id, ids_self_t)?;
            }
        };
        Ok(())
    }

    pub async fn get_head(&self, id: RostraId) -> Option<ShortEventId> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&events_heads::TABLE)?;

            Self::read_head_tx(id, &events_heads)
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_heads(&self, id: RostraId) -> HashSet<ShortEventId> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&events_heads::TABLE)?;

            Self::get_heads_tx(id, &events_heads)
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_heads_self(&self) -> HashSet<ShortEventId> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&events_heads::TABLE)?;

            Self::get_heads_tx(self.self_id, &events_heads)
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_social_profile(&self, id: RostraId) -> Option<IdSocialProfileRecord> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&social_profiles::TABLE)?;

            Self::get_social_profile_tx(id, &events_heads)
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_latest_singleton_event(
        &self,
        kind: EventKind,
        aux_key: EventAuxKey,
    ) -> Option<ShortEventId> {
        self.read_with(|tx| {
            let singletons_table = tx.open_table(&events_singletons::TABLE)?;

            Ok(singletons_table
                .get(&(kind, aux_key))?
                .map(|record| record.value().inner.event_id))
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_latest_singleton_event2(
        &self,
        rostra_id: RostraId,
        kind: EventKind,
        aux_key: EventAuxKey,
    ) -> Option<ShortEventId> {
        self.read_with(|tx| {
            let singletons_table = tx.open_table(&events_singletons_new::TABLE)?;

            Ok(singletons_table
                .get(&(rostra_id, kind, aux_key))?
                .map(|record| record.value().inner.event_id))
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_all_singleton_events(
        &self,
        kind: EventKind,
    ) -> HashMap<EventAuxKey, ShortEventId> {
        self.read_with(|tx| {
            let singletons_table = tx.open_table(&events_singletons::TABLE)?;

            let mut result = HashMap::new();
            let range_start = (kind, EventAuxKey::ZERO);
            let range_end = (kind, EventAuxKey::MAX);

            for record in singletons_table.range(range_start..range_end)? {
                let record = record?;
                let key = record.0.value();
                debug_assert_eq!(key.0, kind);

                let aux_key = key.1;
                let singleton = record.1.value();
                result.insert(aux_key, singleton.inner.event_id);
            }

            Ok(result)
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_id_endpoints(
        &self,
        id: RostraId,
    ) -> BTreeMap<(Timestamp, IrohNodeId), IrohNodeRecord> {
        self.write_with(|tx| {
            let mut table = tx.open_table(&ids_nodes::TABLE)?;

            Self::get_id_endpoints_tx(id, &mut table)
        })
        .await
        .expect("Database panic")
    }

    /// Get events for an identity, sorted by timestamp (most recent first).
    ///
    /// Returns a vector of (EventRecord, Timestamp, EventContentStateNew
    /// option) limited to the specified count.
    pub async fn get_events_for_id(
        &self,
        id: RostraId,
        limit: usize,
    ) -> Vec<(event::EventRecord, Timestamp, Option<EventContentStateNew>)> {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;

            let mut results = Vec::new();

            // Iterate events_by_time in reverse (newest first)
            for entry in events_by_time_table.range(..)?.rev() {
                if results.len() >= limit {
                    break;
                }

                let entry = entry?;
                let (ts, event_id) = entry.0.value();

                // Get the event record
                let Some(event_record) = events_table.get(&event_id)?.map(|g| g.value()) else {
                    continue;
                };

                // Check if this event belongs to the requested identity
                if event_record.signed.event.author != id {
                    continue;
                }

                // Get content state if available
                let content_state = events_content_state_table
                    .get(&event_id)?
                    .map(|g| g.value());

                results.push((event_record, ts, content_state));
            }

            Ok(results)
        })
        .await
        .expect("Database panic")
    }

    /// Get all known identities (from followees, followers, and events).
    pub async fn get_known_identities(&self) -> Vec<RostraId> {
        self.read_with(|tx| {
            let ids_full_table = tx.open_table(&ids_full::TABLE)?;

            let mut ids = HashSet::new();

            for entry in ids_full_table.range(..)? {
                let entry = entry?;
                let short_id = entry.0.value();
                let rest_id = entry.1.value();
                let full_id = RostraId::assemble(short_id, rest_id);
                ids.insert(full_id);
            }

            Ok(ids.into_iter().collect())
        })
        .await
        .expect("Database panic")
    }
}

fn get_first_in_range<K, V>(
    events_table: &impl ReadableTable<K, V>,
    range: impl ops::RangeBounds<K>,
) -> Result<Option<K>, DbError>
where
    K: bincode::Decode<()> + bincode::Encode,
    V: bincode::Decode<()> + bincode::Encode,
{
    Ok(events_table
        .range(range)?
        .next()
        .transpose()?
        .map(|(k, _)| k.value()))
}

fn get_last_in_range<K, V>(
    events_table: &impl ReadableTable<K, V>,
    range: impl ops::RangeBounds<K>,
) -> Result<Option<K>, DbError>
where
    K: bincode::Decode<()> + bincode::Encode,
    V: bincode::Decode<()> + bincode::Encode,
{
    Ok(events_table
        .range(range)?
        .next_back()
        .transpose()?
        .map(|(k, _)| k.value()))
}

#[derive(Debug, Clone)]
pub enum InsertEventOutcome {
    /// An event already existed, so it changed nothing
    AlreadyPresent,
    Inserted {
        /// An event already had a child reporting its existence.
        ///
        /// This also implies that the event can't be a "head event"
        /// as we already have a child of it.
        was_missing: bool,
        /// This event was already marked as deleted by some processed children
        /// event.
        ///
        /// This also implies that the event can't be a "head event"
        /// as we already have a child of it.
        is_deleted: bool,
        /// An existing parent event had its content marked as deleted by this
        /// event.
        ///
        /// Note, if the parent event was marked for deletion, but it was not
        /// processed yet, this will not be set, and instead `is_deleted` will
        /// be set to true, when the deleted parent is processed.
        deleted_parent: Option<ShortEventId>,
        /// Parent content to be reverted.
        ///
        /// If Some - deletion of the `deleted_parent` is cusing revertion of
        /// this content, which should be processed.
        reverted_parent_content: Option<EventContentRaw>,

        /// Ids of parents we don't have yet, so they are now marked
        /// as "missing".
        missing_parents: Vec<ShortEventId>,
    },
}

impl InsertEventOutcome {
    fn validate(self) -> Self {
        if let InsertEventOutcome::Inserted {
            deleted_parent,
            reverted_parent_content,
            ..
        } = &self
        {
            if reverted_parent_content.is_some() {
                assert!(deleted_parent.is_some());
            }
        }
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcessEventState {
    New,
    Existing,
    Pruned,
    Deleted,
    NoContent,
}

pub enum ContentWantState {
    Wants,
    MaybeWants,
    DoesNotWant,
}

impl ProcessEventState {
    pub fn wants_content(self) -> ContentWantState {
        match self {
            ProcessEventState::New => ContentWantState::Wants,
            ProcessEventState::Existing => ContentWantState::MaybeWants,
            ProcessEventState::Pruned => ContentWantState::DoesNotWant,
            ProcessEventState::Deleted => ContentWantState::DoesNotWant,
            ProcessEventState::NoContent => ContentWantState::DoesNotWant,
        }
    }
}
#[cfg(test)]
mod tests;
