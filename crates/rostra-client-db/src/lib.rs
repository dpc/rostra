mod current_state;
mod event_order;
mod events_content_missing_ops;
mod extension;
mod id_nodes_ops;
mod ids_full;
mod migration_ops;
mod models;
pub mod news;
mod paginate;
mod process_event_content_ops;
mod process_event_ops;
mod reception_order_ops;
pub mod social;
mod social_post_materialization;
mod table_ops;
mod tables;
mod tx_ops;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{io, ops, result};

use event::ContentStoreRecord;
pub use ids::{IdsFolloweesRecord, IdsFollowersRecord};
use itertools::Itertools as _;
use process_event_content_ops::ProcessEventError;
use redb_bincode::{ReadTransaction, ReadableTable, WriteTransaction};
use rostra_core::event::{
    EventAuxKey, EventContentRaw, EventExt as _, EventKind, IrohNodeId, PersonasTagsSelector,
    VerifiedEvent, VerifiedEventContent, content_kind,
};
use rostra_core::id::{RostraId, ShortRostraId, ToShort as _};
use rostra_core::{ExternalEventId, ShortEventId, Timestamp};
use rostra_util_error::{BoxedError, FmtCompact as _};
use snafu::{Location, ResultExt as _, Snafu};
use tokio::sync::{Notify, broadcast, watch};
use tokio::task::JoinError;
use tracing::{debug, error, info, instrument};

pub use self::current_state::{CurrentState, CurrentStateClosed};
pub use self::extension::{
    EXTENSION_RESERVED_TABLE_PREFIXES, ExtensionReadTransaction, ExtensionTableDefinition,
    ExtensionWriteTransaction,
};
pub use self::social_post_materialization::{
    SOCIAL_POST_MATERIALIZATION_SCAN_MAX, SocialPostMaterialization,
    SocialPostMaterializationCursor, SocialPostMaterializationPage,
};
pub(crate) use self::tables::*;
pub use self::tables::{
    ContentStoreRecordOwned, EventContentResult, EventContentState, EventReceivedRecord,
    EventReceivedSource, EventRecord, EventsHeadsTableRecord, IdSocialProfileRecord,
    IdsDataUsageRecord, IrohNodeRecord, IrohNodeStats, Latest, SocialNewsRankRecord,
    SocialPostRecord, SocialPostsReactionsRecord, SocialPostsRepliesRecord, SocialVoteScore,
    SocialVoteSumRecord,
};

/// Web of Trust data - contains direct followees and extended followees.
///
/// Extended followees are the followees of your direct followees, excluding
/// those you already follow directly.
#[derive(Debug, Clone, Default)]
pub struct WotData {
    /// Direct followees with their persona selectors
    pub followees: HashMap<RostraId, ids::IdsFolloweesRecord>,
    /// Extended followees (followees of followees), excluding direct followees
    pub extended: HashSet<RostraId>,
}

impl WotData {
    /// Returns true if the given id is in our web of trust (self, direct
    /// followee, or extended)
    pub fn contains(&self, id: RostraId, self_id: RostraId) -> bool {
        id == self_id || self.followees.contains_key(&id) || self.extended.contains(&id)
    }

    /// Returns the total number of IDs in the web of trust (excluding self)
    pub fn len(&self) -> usize {
        self.followees.len() + self.extended.len()
    }

    /// Returns true if there are no followees
    pub fn is_empty(&self) -> bool {
        self.followees.is_empty()
    }

    /// Returns an iterator over all IDs in the web of trust (direct +
    /// extended), excluding self
    pub fn iter_all(&self) -> impl Iterator<Item = RostraId> + '_ {
        self.followees
            .keys()
            .copied()
            .chain(self.extended.iter().copied())
    }
}

const LOG_TARGET: &str = "rostra::db";

type CommitHook = Box<dyn FnOnce() + 'static>;

pub(crate) struct WriteTransactionCtx {
    dbtx: WriteTransaction,
    on_commit: std::sync::Mutex<Option<Vec<CommitHook>>>,
    materialization_emission_enabled: std::sync::atomic::AtomicBool,
}

impl From<WriteTransaction> for WriteTransactionCtx {
    fn from(dbtx: WriteTransaction) -> Self {
        Self {
            dbtx,
            on_commit: std::sync::Mutex::new(Some(vec![])),
            materialization_emission_enabled: std::sync::atomic::AtomicBool::new(true),
        }
    }
}
impl std::ops::Deref for WriteTransactionCtx {
    type Target = WriteTransaction;

    fn deref(&self) -> &Self::Target {
        &self.dbtx
    }
}

impl std::ops::DerefMut for WriteTransactionCtx {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.dbtx
    }
}

impl WriteTransactionCtx {
    /// Registers an action to run in registration order after a successful
    /// commit.
    ///
    /// A hook panic does not suppress later hooks. After every hook has been
    /// attempted, the first panic resumes.
    pub(crate) fn on_commit(&self, f: impl FnOnce() + 'static) {
        if let Some(hooks) = self.on_commit.lock().expect("Locking failed").as_mut() {
            hooks.push(Box::new(f));
        }
    }

    /// Discard existing and future commit hooks for a bulk internal rebuild.
    ///
    /// Total replay reconstructs durable state before the database is exposed
    /// to subscribers. Retaining one publication closure per source event
    /// would make an otherwise streaming rebuild consume memory
    /// proportional to database size.
    pub(crate) fn discard_commit_hooks(&self) {
        *self.on_commit.lock().expect("Locking failed") = None;
    }

    /// Return whether this transaction publishes incremental commit hooks.
    pub(crate) fn commit_hooks_enabled(&self) -> bool {
        self.on_commit.lock().expect("Locking failed").is_some()
    }

    /// Suppress durable occurrence emission during a projection rebuild.
    pub(crate) fn suppress_materialization_emission(&self) {
        self.materialization_emission_enabled
            .store(false, std::sync::atomic::Ordering::Release);
    }

    /// Return whether durable materialization occurrences should be emitted.
    pub(crate) fn materialization_emission_enabled(&self) -> bool {
        self.materialization_emission_enabled
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn commit(self) -> result::Result<(), redb::CommitError> {
        let Self {
            dbtx,
            on_commit,
            materialization_emission_enabled: _,
        } = self;

        dbtx.commit()?;

        let mut first_panic = None;
        if let Some(hooks) = on_commit.lock().expect("Locking failed").as_mut() {
            for hook in hooks.drain(..) {
                if let Err(payload) = catch_unwind(AssertUnwindSafe(hook)) {
                    first_panic.get_or_insert(payload);
                }
            }
        }
        if let Some(payload) = first_panic {
            resume_unwind(payload);
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
    #[snafu(display("Database error"))]
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
    #[snafu(display("Database transaction error"))]
    Transaction {
        #[snafu(source(from(redb::TransactionError, Box::new)))]
        source: Box<redb::TransactionError>,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Database commit error"))]
    Commit {
        source: redb::CommitError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(transparent)]
    StoredDecode {
        source: bincode::error::DecodeError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Database version {db_ver} is newer than supported version {code_ver}"))]
    DbVersionTooHigh {
        db_ver: u64,
        code_ver: u64,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Pending migration stash is missing required table `{table}`"))]
    MissingMigrationStashTable {
        table: String,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Database task failed"))]
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
    #[snafu(display("Database file-format upgrade failed"))]
    Upgrade {
        source: redb::UpgradeError,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Integer overflow"))]
    Overflow,
    #[snafu(display(
        "Reception-order key ({received_at:?}, {reception_order}) already exists in table {table}"
    ))]
    ReceptionOrderCollision {
        table: String,
        received_at: Timestamp,
        reception_order: u64,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Social post {event_id} already has a reception-order key"))]
    SocialPostReceiptAlreadyIndexed {
        event_id: ShortEventId,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display(
        "Reception-order key for social post {event_id} references {actual_event_id:?}"
    ))]
    SocialPostReceiptMismatch {
        event_id: ShortEventId,
        actual_event_id: Option<ShortEventId>,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Deletion attribution references missing event {event_id}"))]
    MissingDeletionAttribution {
        event_id: ShortEventId,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display("Extension access to built-in table `{name}` is forbidden"))]
    ReservedExtensionTable { name: String },
    #[snafu(display(
        "SocialPost materialization cursor {position} exceeds durable position {durable_next}"
    ))]
    SocialPostMaterializationCursorOutOfRange { position: u64, durable_next: u64 },
    #[snafu(display(
        "SocialPost materialization scan limit {requested} exceeds maximum {maximum}"
    ))]
    SocialPostMaterializationScanLimitTooHigh { requested: usize, maximum: usize },
    #[snafu(display(
        "SocialPost materialization feed has a gap: expected {expected}, found {actual}"
    ))]
    SocialPostMaterializationLogGap { expected: u64, actual: u64 },
    #[snafu(display("SocialPost materialization {event_id} references a missing event"))]
    MissingSocialPostMaterializationEvent { event_id: ShortEventId },
    #[snafu(display("SocialPost materialization {event_id} references event kind {kind} instead"))]
    InvalidSocialPostMaterializationKind {
        event_id: ShortEventId,
        kind: EventKind,
    },
    #[snafu(display("SocialPost materialization {event_id} unexpectedly became Missing"))]
    MissingSocialPostMaterializationState { event_id: ShortEventId },
    #[snafu(display("SocialPost materialization {event_id} unexpectedly became Invalid"))]
    InvalidSocialPostMaterializationState { event_id: ShortEventId },
    #[snafu(display("SocialPost materialization {event_id} has no current content"))]
    MissingSocialPostMaterializationContent { event_id: ShortEventId },
    #[snafu(display("SocialPost materialization {event_id} has invalid processed content"))]
    InvalidSocialPostMaterializationContent {
        event_id: ShortEventId,
        source: BoxedError,
    },
    #[snafu(display("Stored vote singleton {event_id} has no inline vote projection"))]
    MissingVoteSingletonProjection {
        event_id: ShortEventId,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display(
        "Stored vote singleton {event_id} has a cached target outside its shortened key"
    ))]
    InvalidVoteSingletonProjection {
        event_id: ShortEventId,
        #[snafu(implicit)]
        location: Location,
    },
    #[snafu(display(
        "Shortened identity prefix {prefix} maps to both {existing_id} and {incoming_id}"
    ))]
    IdentityPrefixCollision {
        prefix: ShortRostraId,
        existing_id: RostraId,
        incoming_id: RostraId,
        #[snafu(implicit)]
        location: Location,
    },
}
pub type DbResult<T> = std::result::Result<T, DbError>;

/// The authoritative event store and projection boundary for one local
/// identity.
///
/// Built-in reducers and projection tables are not part of the external API:
///
/// ```compile_fail
/// let _ = rostra_client_db::Database::process_event_content_inserted_tx;
/// ```
///
/// ```compile_fail
/// let _ = rostra_client_db::events_singletons_new::TABLE;
/// ```
///
/// ```compile_fail
/// let _ = rostra_client_db::Database::write_with::<()>;
/// ```
///
/// ```compile_fail
/// let _ = rostra_client_db::Database::write_with_inner::<()>;
/// ```
///
/// ```compile_fail
/// let _ = rostra_client_db::Database::read_with::<()>;
/// ```
///
/// ```compile_fail
/// let _: Option<rostra_client_db::WriteTransactionCtx> = None;
/// ```
///
/// ```compile_fail
/// rostra_client_db::def_table!(forbidden: u64 => u64);
/// ```
#[derive(Debug)]
pub struct Database {
    inner: redb_bincode::Database,
    self_id: RostraId,
    iroh_secret: iroh::SecretKey,

    /// Timestamp when this database was first created.
    ///
    /// Used with the current follow epoch as a notification cutoff: posts older
    /// than both are historical syncs and should not appear as newly received.
    db_init_time: Timestamp,

    /// Serializes writes through their post-commit current-state publication.
    ///
    /// Acquiring it before transaction creation keeps redb writer order and
    /// publication order identical. Commit hooks run while it is held and must
    /// not synchronously re-enter `write_with`. A hook panic propagates after
    /// commit and poisons this mutex; the next write recovers the guard because
    /// the durable transaction is still valid.
    write_and_publish_lock: std::sync::Mutex<()>,

    self_followees_updated: watch::Sender<Arc<HashMap<RostraId, IdsFolloweesRecord>>>,
    self_followers_updated: watch::Sender<Arc<HashMap<RostraId, IdsFollowersRecord>>>,
    self_wot_updated: watch::Sender<Arc<WotData>>,
    self_head_updated: watch::Sender<Option<ShortEventId>>,
    new_content_tx: broadcast::Sender<VerifiedEventContent>,
    new_posts_tx: broadcast::Sender<(VerifiedEventContent, content_kind::SocialPost)>,
    new_shoutbox_tx: broadcast::Sender<(VerifiedEventContent, content_kind::Shoutbox)>,
    new_heads_tx: broadcast::Sender<(RostraId, ShortEventId)>,
    ids_with_missing_events_tx: dedup_chan::Sender<RostraId>,
    news_score_updates_tx: dedup_chan::Sender<ExternalEventId>,

    /// Notification for when new content is added to `events_content_missing`.
    ///
    /// The `MissingEventContentFetcher` task waits on this to wake up
    /// immediately when new missing content arrives, instead of polling.
    content_missing_notify: Arc<Notify>,
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
            .create_with_file_format_v3(true)
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .context(DatabaseSnafu)?;
        Self::open_inner(inner, self_id).await
    }

    /// Open an identity database, creating it when absent.
    ///
    /// Opening storage older than schema version 25 performs the total rebuild
    /// before returning. This can hold a long atomic write transaction and
    /// requires deployment-specific RAM and temporary disk headroom. See
    /// `specs/ARCH-client-database.md#total-rebuild`.
    pub async fn open(path: impl Into<PathBuf>, self_id: RostraId) -> DbResult<Database> {
        let path = path.into();
        debug!(target: LOG_TARGET, id = %self_id, path = %path.display(), "Opening database");

        let mut inner = tokio::task::spawn_blocking(move || {
            redb::Database::builder()
                .create_with_file_format_v3(true)
                .create(path)
        })
        .await
        .context(JoinSnafu)?
        .context(DatabaseSnafu)?;

        inner.upgrade().context(UpgradeSnafu)?;

        Self::open_inner(inner, self_id).await
    }

    #[instrument(skip_all)]
    async fn open_inner(inner: redb::Database, self_id: RostraId) -> DbResult<Database> {
        let inner = redb_bincode::Database::from(inner);

        // Run migrations (may stash tables for total migration reprocessing)
        Self::write_with_inner(&inner, |tx| {
            // Opening table definitions is raw and does not decode rows. Check
            // the schema version before reading typed identity bytes so a newer
            // database always fails with DbVersionTooHigh.
            Self::init_tables_tx(tx)?;
            Self::handle_db_ver_migrations(tx)?;
            Self::verify_self_tx(self_id, &mut tx.open_table(&ids_self::TABLE)?)?;
            Ok(())
        })
        .await?;

        // Check if there's a pending migration stash (either from this run or a
        // previous interrupted run). Using stash existence as the marker ensures
        // we retry if reprocessing fails/panics.
        let needs_reprocessing =
            Self::write_with_inner(&inner, Self::has_pending_migration_stash).await?;

        let (self_head, iroh_secret, self_followees, self_followers, self_wot, db_init_time) =
            Self::read_with_inner(&inner, |tx| {
                let ids_followees_table = tx.open_table(&ids_followees::TABLE)?;
                let self_followees = Self::read_followees_tx(self_id, &ids_followees_table)?;
                let self_wot =
                    Self::compute_wot_tx(self_id, &self_followees, &ids_followees_table)?;
                let db_init_time = tx
                    .open_table(&db_init_time::TABLE)?
                    .get(&())?
                    .map(|g| g.value())
                    .unwrap_or(Timestamp::ZERO);
                Ok((
                    Self::read_head_tx(self_id, &tx.open_table(&events_heads::TABLE)?)?,
                    Self::read_iroh_secret_tx(&tx.open_table(&ids_self::TABLE)?)?,
                    self_followees,
                    Self::read_followers_tx(self_id, &tx.open_table(&ids_followers::TABLE)?)?,
                    self_wot,
                    db_init_time,
                ))
            })
            .await?;

        let (self_followees_updated, _) = watch::channel(Arc::new(self_followees));
        let (self_followers_updated, _) = watch::channel(Arc::new(self_followers));
        let (self_wot_updated, _) = watch::channel(Arc::new(self_wot));
        let (self_head_updated, _) = watch::channel(self_head);
        let (new_content_tx, _) = broadcast::channel(100);
        let (new_posts_tx, _) = broadcast::channel(100);
        let (new_shoutbox_tx, _) = broadcast::channel(100);
        let (new_heads_tx, _) = broadcast::channel(100);

        let db = Self {
            inner,
            self_id,
            iroh_secret,
            db_init_time,
            write_and_publish_lock: std::sync::Mutex::new(()),
            self_followees_updated,
            self_followers_updated,
            self_wot_updated,
            self_head_updated,
            new_content_tx,
            new_posts_tx,
            new_shoutbox_tx,
            new_heads_tx,
            ids_with_missing_events_tx: dedup_chan::Sender::new(),
            news_score_updates_tx: dedup_chan::Sender::new(),
            content_missing_notify: Arc::new(Notify::new()),
        };

        // If total migration stashed events, reprocess them now using the real
        // Database. The stash existence check ensures we retry if this
        // fails/panics.
        if needs_reprocessing {
            db.write_with(|tx| db.reprocess_migration_stash(tx)).await?;
            db.refresh_current_state_after_replay().await?;
        }

        Ok(db)
    }

    /// Publish one coherent current-state snapshot after silent total replay.
    async fn refresh_current_state_after_replay(&self) -> DbResult<()> {
        let (self_head, self_followees, self_followers, self_wot) = self
            .read_with(|tx| {
                let followees = tx.open_table(&ids_followees::TABLE)?;
                let self_followees = Self::read_followees_tx(self.self_id, &followees)?;
                let self_wot = Self::compute_wot_tx(self.self_id, &self_followees, &followees)?;
                Ok((
                    Self::read_head_tx(self.self_id, &tx.open_table(&events_heads::TABLE)?)?,
                    self_followees,
                    Self::read_followers_tx(self.self_id, &tx.open_table(&ids_followers::TABLE)?)?,
                    self_wot,
                ))
            })
            .await?;

        self.self_head_updated.send_replace(self_head);
        self.self_followees_updated
            .send_replace(Arc::new(self_followees));
        self.self_followers_updated
            .send_replace(Arc::new(self_followers));
        self.self_wot_updated.send_replace(Arc::new(self_wot));
        Ok(())
    }

    pub async fn compact(&mut self) -> Result<bool, redb::CompactionError> {
        tokio::task::block_in_place(|| self.inner.as_raw_mut().compact())
    }

    pub async fn dump_table(&self, name: &str) -> TableDumpResult<()> {
        self.read_with(|tx| {
            match name {
                "events" => Self::dump_table_dbtx(tx, &tables::events::TABLE)?,
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
                "social_vote_sums" => Self::dump_table_dbtx(tx, &tables::social_vote_sums::TABLE)?,
                "social_news_rank_by_post_id" => {
                    Self::dump_table_dbtx(tx, &tables::social_news_rank_by_post_id::TABLE)?
                }
                "social_news_rank_by_score" => {
                    Self::dump_table_dbtx(tx, &tables::social_news_rank_by_score::TABLE)?
                }
                "social_news_rank_by_time" => {
                    Self::dump_table_dbtx(tx, &tables::social_news_rank_by_time::TABLE)?
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

    /// Subscribe to owned snapshots of the retained current self-followee
    /// projection.
    pub fn self_followees_subscribe(
        &self,
    ) -> CurrentState<Arc<HashMap<RostraId, IdsFolloweesRecord>>> {
        CurrentState::new(self.self_followees_updated.subscribe())
    }

    /// Subscribe to owned snapshots of the retained current self-follower
    /// projection.
    pub fn self_followers_subscribe(
        &self,
    ) -> CurrentState<Arc<HashMap<RostraId, IdsFollowersRecord>>> {
        CurrentState::new(self.self_followers_updated.subscribe())
    }

    /// Subscribe to owned snapshots of the retained current Web-of-Trust
    /// projection.
    pub fn self_wot_subscribe(&self) -> CurrentState<Arc<WotData>> {
        CurrentState::new(self.self_wot_updated.subscribe())
    }

    /// Subscribe to owned snapshots of the retained deterministic self-head
    /// representative.
    ///
    /// The value is the minimum current `ShortEventId`. It is a stable default,
    /// not a claim that the current head set is a singleton. Every committed
    /// self-event insertion publishes, even when the representative is
    /// unchanged.
    pub fn self_head_subscribe(&self) -> CurrentState<Option<ShortEventId>> {
        CurrentState::new(self.self_head_updated.subscribe())
    }

    pub fn new_content_subscribe(&self) -> broadcast::Receiver<VerifiedEventContent> {
        self.new_content_tx.subscribe()
    }
    pub fn new_posts_subscribe(
        &self,
    ) -> broadcast::Receiver<(VerifiedEventContent, content_kind::SocialPost)> {
        self.new_posts_tx.subscribe()
    }

    pub fn new_shoutbox_subscribe(
        &self,
    ) -> broadcast::Receiver<(VerifiedEventContent, content_kind::Shoutbox)> {
        self.new_shoutbox_tx.subscribe()
    }

    /// Subscribe to incremental exact new-head signals.
    ///
    /// This lossy channel carries the event that became a head. Consumers that
    /// lag must recover from the durable complete head set.
    pub fn new_heads_subscribe(&self) -> broadcast::Receiver<(RostraId, ShortEventId)> {
        self.new_heads_tx.subscribe()
    }

    pub fn ids_with_missing_events_subscribe(
        &self,
        capacity: usize,
    ) -> dedup_chan::Receiver<RostraId> {
        self.ids_with_missing_events_tx.subscribe(capacity)
    }

    pub fn news_score_updates_subscribe(
        &self,
        capacity: usize,
    ) -> dedup_chan::Receiver<ExternalEventId> {
        self.news_score_updates_tx.subscribe(capacity)
    }

    /// Get a handle to the content-missing notification.
    ///
    /// The `MissingEventContentFetcher` calls `notified()` on this to wake up
    /// when new missing content is inserted into the database.
    pub fn content_missing_notify(&self) -> Arc<Notify> {
        self.content_missing_notify.clone()
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

    pub async fn get_heads_events_for_id(&self, id: RostraId) -> Vec<ShortEventId> {
        self.read_with(|tx| {
            let events_heads_tbl = tx.open_table(&events_heads::TABLE)?;
            Ok(Database::get_heads_events_tx(id, &events_heads_tbl)?
                .into_iter()
                .collect())
        })
        .await
        .expect("Database panic")
    }

    pub async fn count_missing_events_for_id(&self, id: RostraId) -> usize {
        self.read_with(|tx| {
            let events_missing_tbl = tx.open_table(&events_missing::TABLE)?;
            Database::count_missing_events_for_id_tx(id, &events_missing_tbl)
        })
        .await
        .expect("Database panic")
    }

    pub async fn count_heads_events_for_id(&self, id: RostraId) -> usize {
        self.read_with(|tx| {
            let events_heads_tbl = tx.open_table(&events_heads::TABLE)?;
            Database::count_heads_events_tx(id, &events_heads_tbl)
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_data_usage(&self, id: RostraId) -> IdsDataUsageRecord {
        self.read_with(|tx| {
            let ids_data_usage_tbl = tx.open_table(&ids_data_usage::TABLE)?;
            Database::get_data_usage_tx(id, &ids_data_usage_tbl)
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_self_followees(&self) -> Vec<(RostraId, PersonasTagsSelector)> {
        self.get_followees(self.self_id).await
    }

    pub async fn get_followees(&self, id: RostraId) -> Vec<(RostraId, PersonasTagsSelector)> {
        self.read_with(|tx| {
            let ids_followees_table = tx.open_table(&ids_followees::TABLE)?;
            Ok(Database::read_followees_tx(id, &ids_followees_table)?
                .into_iter()
                .map(|(id, record)| (id, record.effective_tags_selector()))
                .collect())
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_followees_extended(
        &self,
        id: RostraId,
    ) -> (HashMap<RostraId, PersonasTagsSelector>, HashSet<RostraId>) {
        self.read_with(|tx| {
            let ids_followees_table = tx.open_table(&ids_followees::TABLE)?;
            let followees: HashMap<RostraId, PersonasTagsSelector> =
                Database::read_followees_tx_iter(id, &ids_followees_table)?
                    .map_ok(|(id, record)| (id, record.effective_tags_selector()))
                    .collect::<Result<_, _>>()?;

            let mut extended = HashSet::new();

            for followee in followees.keys() {
                for extended_followee in
                    Database::read_followees_tx_iter(*followee, &ids_followees_table)?
                        .map_ok(|(id, _record)| id)
                {
                    let extended_followee = extended_followee?;
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

    /// Return the lifecycle state for event content that is not currently
    /// stored.
    ///
    /// For an existing event, `None` normally means the content was processed.
    /// A terminal state such as `Deleted`, `Pruned`, or `Invalid` means callers
    /// must not wait for payload availability.
    pub async fn get_event_content_state(
        &self,
        event_id: impl Into<ShortEventId>,
    ) -> Option<EventContentState> {
        let event_id = event_id.into();
        self.read_with(|tx| {
            Self::get_event_content_state_tx(
                event_id,
                &tx.open_table(&events_content_state::TABLE)?,
            )
        })
        .await
        .expect("Database panic")
    }

    /// Return the minimum current self-head as a deterministic representative.
    ///
    /// Use [`Self::get_heads_self`] when the complete set is required.
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

    /// Fallibly process a verified event envelope.
    ///
    /// # Errors
    ///
    /// Returns storage and transaction errors, including
    /// [`DbError::IdentityPrefixCollision`]. An error leaves the ingestion
    /// transaction uncommitted.
    pub async fn try_process_event(
        &self,
        event: &VerifiedEvent,
    ) -> DbResult<(InsertEventOutcome, ProcessEventState)> {
        let now = Timestamp::now();
        self.write_with(|tx| self.process_event_tx(event, now, tx))
            .await
    }

    /// Process a verified event envelope, panicking on storage failure.
    ///
    /// # Panics
    ///
    /// Panics after rolling back if storage fails or the event author's
    /// shortened identity prefix is already mapped to a different full
    /// identity.
    pub async fn process_event(
        &self,
        event: &VerifiedEvent,
    ) -> (InsertEventOutcome, ProcessEventState) {
        self.try_process_event(event).await.expect("Storage error")
    }

    /// Fallibly process a verified envelope and its verified content
    /// atomically.
    ///
    /// This operation is idempotent whether or not the envelope or content was
    /// processed previously.
    ///
    /// # Errors
    ///
    /// Returns storage and transaction errors, including
    /// [`DbError::IdentityPrefixCollision`]. An error rolls back the envelope,
    /// content, lifecycle, and projection changes together.
    pub async fn try_process_event_with_content(
        &self,
        content: &VerifiedEventContent,
    ) -> DbResult<(InsertEventOutcome, ProcessEventState)> {
        let now = Timestamp::now();
        self.write_with(|tx| {
            let res = self.process_event_tx(&content.event, now, tx)?;
            self.process_event_content_tx(content, now, tx)?;
            Ok(res)
        })
        .await
    }

    /// Process a verified envelope and its verified content, panicking on
    /// storage failure.
    ///
    /// This operation is idempotent whether or not the envelope or content was
    /// processed previously.
    ///
    /// # Panics
    ///
    /// Panics after rolling back if storage fails or a stored invariant is
    /// violated, including a vote winner with a missing or invalid inline
    /// projection or a shortened identity prefix mapped to a different full
    /// identity.
    pub async fn process_event_with_content(
        &self,
        content: &VerifiedEventContent,
    ) -> (InsertEventOutcome, ProcessEventState) {
        self.try_process_event_with_content(content)
            .await
            .expect("Storage error")
    }

    /// Fallibly process verified content and its carried envelope atomically.
    ///
    /// This is the safe public content-ingestion boundary. If the envelope is
    /// absent, it is inserted before content processing in the same
    /// transaction. If the envelope is already present, normal
    /// lifecycle-state checks make the operation idempotent and prevent
    /// repeated accounting or projections.
    ///
    /// # Errors
    ///
    /// Returns errors under the same conditions as
    /// [`Database::try_process_event_with_content`].
    pub async fn try_process_event_content(
        &self,
        event_content: &VerifiedEventContent,
    ) -> DbResult<()> {
        self.try_process_event_with_content(event_content)
            .await
            .map(|_| ())
    }

    /// Process verified content and its carried envelope, panicking on storage
    /// failure.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as
    /// [`Database::process_event_with_content`].
    pub async fn process_event_content(&self, event_content: &VerifiedEventContent) {
        self.try_process_event_content(event_content)
            .await
            .expect("Storage error");
    }

    /// Process event content.
    ///
    /// Ordinary processing requires Missing state, applies kind-specific side
    /// effects, stores the content, removes fetch scheduling, and transitions
    /// to Processed. RC was already incremented at event insertion.
    ///
    /// A verified, below-limit Deleted social-post edit is the sole
    /// terminal-state exception. It may add only immutable forward and reverse
    /// replacement rows; it does not store the supplied bytes or change
    /// lifecycle bookkeeping or other projections.
    ///
    /// The `now` parameter should be `Timestamp::now()` for normal operation,
    /// but can be set to a specific value for testing or migration.
    pub(crate) fn process_event_content_tx(
        &self,
        event_content: &VerifiedEventContent,
        now: Timestamp,
        tx: &WriteTransactionCtx,
    ) -> DbResult<()> {
        {
            let events_table = tx.open_table(&events::TABLE)?;
            let has_event = Database::has_event_tx(event_content.event.event_id, &events_table)?;
            if !has_event {
                // Event doesn't exist - this shouldn't happen in normal operation.
                // It means process_event_content_tx was called without first inserting the
                // event.
                debug_assert!(false, "Processing content for non-existent event");
                error!(
                    target: LOG_TARGET,
                    event_id = %event_content.event.event_id,
                    "Processing content for non-existent event - possible bug"
                );
                return Ok(());
            }
        }

        // Check if content should be processed (not deleted/pruned, is Missing)
        let (can_insert, is_deleted) = if u32::from(event_content.event.event.content_len)
            < Self::MAX_CONTENT_LEN
        {
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let state = events_content_state_table
                .get(&event_content.event_id().to_short())?
                .map(|state| state.value());
            (
                Database::can_insert_event_content_tx(event_content, &events_content_state_table)?,
                matches!(state, Some(EventContentState::Deleted { .. })),
            )
        } else {
            (false, false)
        };

        if can_insert {
            // Remove eligible content from the missing list.
            {
                let event_short_id = event_content.event_id().to_short();
                let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
                let mut events_content_missing_table =
                    tx.open_table(&tables::events_content_missing::TABLE)?;
                let state = events_content_state_table
                    .get(&event_short_id)?
                    .map(|g| g.value());
                if let Some(EventContentState::Missing {
                    next_fetch_attempt, ..
                }) = state
                {
                    events_content_missing_table.remove(&(next_fetch_attempt, event_short_id))?;
                }
            }

            if let Some(content) = event_content.content.as_ref() {
                let content_hash = event_content.content_hash();
                let event_short_id = event_content.event_id().to_short();

                // Process side effects
                let is_valid = match self.process_event_content_inserted_tx(event_content, now, tx)
                {
                    Ok(()) => {
                        if tx.commit_hooks_enabled() {
                            info!(target: LOG_TARGET,
                                kind = %event_content.kind(),
                                event_id = %event_short_id,
                                author = %event_content.author().to_short(),
                                len = %event_content.content_len(),
                                "New event content inserted"
                            );
                        }
                        true
                    }
                    Err(ProcessEventError::Invalid { source, location }) => {
                        if tx.commit_hooks_enabled() {
                            info!(
                                target: LOG_TARGET,
                                err = %source.as_ref().fmt_compact(),
                                %location,
                                "Invalid event content"
                            );
                        }
                        false
                    }
                    Err(ProcessEventError::Db { source }) => {
                        return Err(source);
                    }
                };

                if is_valid {
                    // Store content in content_store if not already there
                    {
                        let mut content_store_table = tx.open_table(&content_store::TABLE)?;
                        if content_store_table.get(&content_hash)?.is_none() {
                            content_store_table.insert(
                                &content_hash,
                                &ContentStoreRecord(Cow::Owned(content.clone())),
                            )?;
                        }
                    }

                    // Remove the Missing marker now that content is processed
                    {
                        let mut events_content_state_table =
                            tx.open_table(&events_content_state::TABLE)?;
                        events_content_state_table.remove(&event_short_id)?;
                    }

                    // Track payload as processed (missing → current)
                    {
                        let mut ids_data_usage_table =
                            tx.open_table(&tables::ids_data_usage::TABLE)?;
                        Database::track_payload_processed_tx(
                            event_content.author(),
                            event_content.content_len(),
                            &mut ids_data_usage_table,
                        )?;
                    }

                    // Notify about new content
                    if tx.commit_hooks_enabled() {
                        tx.on_commit({
                            let new_content_tx = self.new_content_tx.clone();
                            let event_content = event_content.clone();
                            move || {
                                let _ = new_content_tx.send(event_content);
                            }
                        });
                    }
                } else {
                    // Content failed validation — mark as Invalid, decrement RC,
                    // discard content bytes
                    {
                        let mut events_content_state_table =
                            tx.open_table(&events_content_state::TABLE)?;
                        events_content_state_table
                            .insert(&event_short_id, &EventContentState::Invalid)?;
                    }
                    {
                        let mut content_rc_table = tx.open_table(&tables::content_rc::TABLE)?;
                        Database::decrement_content_rc_tx(content_hash, &mut content_rc_table)?;
                    }

                    // Track payload as invalid (missing → invalid)
                    {
                        let mut ids_data_usage_table =
                            tx.open_table(&tables::ids_data_usage::TABLE)?;
                        Database::track_payload_invalid_tx(
                            event_content.author(),
                            event_content.content_len(),
                            &mut ids_data_usage_table,
                        )?;
                    }
                }
            }
        } else if is_deleted && event_content.content.is_some() {
            match Self::process_deleted_social_post_replacement_tx(event_content, tx) {
                Ok(_) => {}
                Err(ProcessEventError::Invalid { source, location }) => {
                    debug!(
                        target: LOG_TARGET,
                        err = %source.as_ref().fmt_compact(),
                        %location,
                        "Ignoring malformed Deleted social-post content"
                    );
                }
                Err(ProcessEventError::Db { source }) => return Err(source),
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

        let event_id = event_id.into();
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;
            let content_store_table = tx.open_table(&content_store::TABLE)?;

            // Get event to find content_hash
            let Some(event) = Database::get_event_tx(event_id, &events_table)? else {
                return Ok(false);
            };
            let content_hash = event.content_hash();

            // We want content if we DON'T have it yet
            Ok(!Database::has_event_content_tx(
                event_id,
                content_hash,
                &events_content_state_table,
                &content_store_table,
            )?)
        })
        .await
        .expect("Storage error")
    }

    pub fn db_init_time(&self) -> Timestamp {
        self.db_init_time
    }

    pub fn iroh_secret(&self) -> iroh::SecretKey {
        self.iroh_secret.clone()
    }
}

impl Database {
    fn write_with_inner_blocking<T>(
        inner: &redb_bincode::Database,
        f: impl FnOnce(&'_ WriteTransactionCtx) -> DbResult<T>,
    ) -> DbResult<T> {
        let mut dbtx = WriteTransactionCtx::from(inner.begin_write().context(TransactionSnafu)?);
        let res = f(&mut dbtx)?;

        dbtx.commit().context(CommitSnafu)?;

        Ok(res)
    }

    /// Runs a low-level write transaction without a `Database` publication
    /// boundary.
    ///
    /// The synchronous transaction closure must not start another write.
    pub(crate) async fn write_with_inner<T>(
        inner: &redb_bincode::Database,
        f: impl FnOnce(&'_ WriteTransactionCtx) -> DbResult<T>,
    ) -> DbResult<T> {
        tokio::task::block_in_place(|| Self::write_with_inner_blocking(inner, f))
    }

    /// Runs a serialized write transaction and its internal post-commit
    /// actions.
    ///
    /// The mutex spans transaction creation, durable commit, and all
    /// post-commit actions so their publication order matches writer order.
    /// The synchronous transaction closure and post-commit actions must not
    /// re-enter database writes.
    ///
    /// A post-commit action panic propagates to the caller after the
    /// transaction has committed. Every registered action is attempted, then
    /// the first panic resumes. A later write recovers the poisoned
    /// serialization mutex and proceeds.
    pub(crate) async fn write_with<T>(
        &self,
        f: impl FnOnce(&'_ WriteTransactionCtx) -> DbResult<T>,
    ) -> DbResult<T> {
        tokio::task::block_in_place(|| {
            let _write_and_publish_guard = self
                .write_and_publish_lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self::write_with_inner_blocking(&self.inner, f)
        })
    }

    /// Runs a serialized transaction over caller-owned extension tables.
    ///
    /// The closure cannot access built-in graph, lifecycle, or projection
    /// tables. It must not re-enter database writes. This trusted persistence
    /// boundary does not include extension data in core replay or convergence;
    /// callers own their schema, invariants, and compatibility. Total database
    /// migrations preserve extension tables byte-for-byte.
    ///
    /// Returning `Ok` commits all extension-table mutations atomically.
    /// Returning `Err`, panicking, or failing a reserved-name/table
    /// operation leaves the transaction uncommitted. Post-commit built-in
    /// projection hooks are not available at this boundary.
    ///
    /// # Errors
    ///
    /// Returns the closure's error and propagates transaction, table, and
    /// commit errors. Opening a reserved table returns
    /// [`DbError::ReservedExtensionTable`].
    pub async fn extension_write<T>(
        &self,
        f: impl FnOnce(&ExtensionWriteTransaction<'_>) -> DbResult<T>,
    ) -> DbResult<T> {
        self.write_with(|tx| f(&ExtensionWriteTransaction::new(tx)))
            .await
    }

    pub(crate) async fn read_with_inner<T>(
        inner: &redb_bincode::Database,
        f: impl FnOnce(&'_ ReadTransaction) -> DbResult<T>,
    ) -> DbResult<T> {
        tokio::task::block_in_place(|| {
            let mut dbtx = inner.begin_read().context(TransactionSnafu)?;

            f(&mut dbtx)
        })
    }

    pub(crate) async fn read_with<T>(
        &self,
        f: impl FnOnce(&'_ ReadTransaction) -> DbResult<T>,
    ) -> DbResult<T> {
        Self::read_with_inner(&self.inner, f).await
    }

    /// Runs a read transaction over trusted, caller-owned extension tables.
    ///
    /// Callers own extension schemas, invariants, and compatibility; core event
    /// replay does not validate or rebuild their contents.
    ///
    /// # Errors
    ///
    /// Returns the closure's error and propagates transaction and table errors.
    /// Opening a reserved table returns [`DbError::ReservedExtensionTable`].
    pub async fn extension_read<T>(
        &self,
        f: impl FnOnce(&ExtensionReadTransaction<'_>) -> DbResult<T>,
    ) -> DbResult<T> {
        self.read_with(|tx| f(&ExtensionReadTransaction::new(tx)))
            .await
    }

    pub(crate) fn verify_self_tx(
        self_id: RostraId,
        ids_self_t: &mut ids_self::Table,
    ) -> DbResult<()> {
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

    /// Return the minimum current head as a deterministic representative.
    ///
    /// Use [`Self::get_heads`] when the complete set is required.
    pub async fn get_head(&self, id: RostraId) -> Option<ShortEventId> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&events_heads::TABLE)?;

            Self::read_head_tx(id, &events_heads)
        })
        .await
        .expect("Database panic")
    }

    /// Return the complete current head set for an identity.
    pub async fn get_heads(&self, id: RostraId) -> HashSet<ShortEventId> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&events_heads::TABLE)?;

            Self::get_heads_tx(id, &events_heads)
        })
        .await
        .expect("Database panic")
    }

    /// Return the complete current head set for the local identity.
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

    /// Returns an identity's singleton winners for one event kind in descending
    /// `(timestamp, ShortEventId)` order.
    pub async fn get_latest_singleton_events(
        &self,
        rostra_id: RostraId,
        kind: EventKind,
    ) -> Vec<ShortEventId> {
        self.read_with(|tx| {
            let singletons = tx.open_table(&events_singletons_new::TABLE)?;
            let start = (rostra_id, kind, EventAuxKey::ZERO);
            let end = (rostra_id, kind, EventAuxKey::MAX);
            let mut events = singletons
                .range(start..=end)?
                .map(|entry| {
                    let (_, value) = entry?;
                    let value = value.value();
                    Ok((value.ts, value.inner.event_id))
                })
                .collect::<DbResult<Vec<_>>>()?;
            events.sort_unstable_by_key(|event| std::cmp::Reverse(*event));
            Ok(events.into_iter().map(|(_, event_id)| event_id).collect())
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

    /// Register an iroh node endpoint for an identity.
    ///
    /// Useful for test setups where peer node addresses need to be manually
    /// registered.
    pub async fn insert_id_node(&self, id: RostraId, node_id: IrohNodeId, ts: Timestamp) {
        self.write_with(|tx| {
            let mut table = tx.open_table(&ids_nodes::TABLE)?;
            table.insert(
                &(id, node_id),
                &IrohNodeRecord {
                    announcement_ts: ts,
                    stats: Default::default(),
                },
            )?;
            Ok(())
        })
        .await
        .expect("Database panic")
    }

    /// Get events for an identity, sorted by timestamp (most recent first).
    ///
    /// Returns a vector of (EventRecord, Timestamp, EventContentState
    /// option) limited to the specified count.
    pub async fn get_events_for_id(
        &self,
        id: RostraId,
        limit: usize,
    ) -> Vec<(event::EventRecord, Timestamp, Option<EventContentState>)> {
        self.read_with(|tx| {
            let events_table = tx.open_table(&events::TABLE)?;
            let events_by_time_table = tx.open_table(&events_by_time::TABLE)?;
            let events_content_state_table = tx.open_table(&events_content_state::TABLE)?;

            let mut results = Vec::new();

            // Iterate events_by_time in reverse (newest first)
            for entry in events_by_time_table.range(..)?.rev() {
                if limit <= results.len() {
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

    /// Get all identities that have authored retained events.
    pub async fn get_known_identities(&self) -> Vec<RostraId> {
        self.read_with(ids_full::read_all)
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
    /// The event already existed, so graph, lifecycle, and projection state did
    /// not change.
    ///
    /// Identity registration is still validated and may restore an absent
    /// shortened/full mapping.
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
        }
    }
}
#[cfg(test)]
mod content_ingestion_tests;
#[cfg(test)]
mod deleted_replacement_tests;
#[cfg(test)]
mod follow_epoch_tests;
#[cfg(test)]
mod identity_collision_tests;
#[cfg(test)]
mod reception_order_tests;
#[cfg(test)]
mod social_post_materialization_tests;
#[cfg(test)]
mod social_post_projection_tests;
#[cfg(test)]
mod social_post_receipt_tests;
#[cfg(test)]
mod tests;
