mod models;
pub mod social;
mod tables;
mod tx_ops;

use std::path::PathBuf;
use std::{ops, result};

use ids::IdsFollowersRecord;
use redb_bincode::{ReadTransaction, ReadableTable, WriteTransaction};
use rostra_core::event::{
    content, EventContent, EventKind, PersonaId, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::RostraId;
use rostra_core::ShortEventId;
use rostra_util_error::{BoxedError, FmtCompact as _};
use snafu::{Location, ResultExt as _, Snafu};
use tables::ids::IdsFolloweesRecord;
use tokio::sync::watch;
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
        source: redb::TransactionError,
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
    IdMismatch {
        #[snafu(implicit)]
        location: Location,
    },
}
pub type DbResult<T> = std::result::Result<T, DbError>;

#[derive(Debug)]
pub struct Database {
    inner: redb_bincode::Database,
    self_id: RostraId,
    iroh_secret: iroh::SecretKey,

    self_followee_list_updated: watch::Sender<()>,
    self_head_updated: watch::Sender<Option<ShortEventId>>,
}

impl Database {
    const DB_VER: u64 = 0;

    #[instrument(skip_all)]
    pub async fn open(path: impl Into<PathBuf>, self_id: RostraId) -> DbResult<Database> {
        let path = path.into();
        debug!(target: LOG_TARGET, path = %path.display(), "Opening database");
        let inner = tokio::task::spawn_blocking(move || redb_bincode::Database::create(path))
            .await
            .context(JoinSnafu)?
            .context(DatabaseSnafu)?;

        Self::write_with_inner(&inner, |tx| {
            Self::verify_self_tx(self_id, &mut tx.open_table(&ids_self::TABLE)?)?;
            Self::handle_db_ver_migrations(tx)?;
            Ok(())
        })
        .await?;

        let (self_head, iroh_secret) = Self::read_with_inner(&inner, |tx| {
            Ok((
                Self::get_head_tx(self_id, &tx.open_table(&events_heads::TABLE)?)?,
                Self::get_iroh_secret_tx(&tx.open_table(&ids_self::TABLE)?)?,
            ))
        })
        .await?;

        let (self_followee_list_updated, _) = watch::channel(());
        let (self_head_updated, _) = watch::channel(self_head);

        let s = Self {
            inner,
            self_id,
            iroh_secret,
            self_followee_list_updated,
            self_head_updated,
        };

        Ok(s)
    }

    // async fn init(self) -> DbResult<Self> {
    //     self.write_with(|dbtx| {
    //         // dbtx.open_table(&db_ver::TABLE)?;
    //         // dbtx.open_table(&id_self::TABLE)?;
    //         // dbtx.open_table(&id::TABLE)?;
    //         // dbtx.open_table(&events_id_followees::TABLE)?;
    //         // dbtx.open_table(&TABLE_ID_UNFOLLOWED)?;
    //         // dbtx.open_table(&events_id_personas::TABLE)?;
    //         // dbtx.open_table(&events::TABLE)?;
    //         // dbtx.open_table(&events_by_time::TABLE)?;
    //         // dbtx.open_table(&events_content::TABLE)?;
    //         // dbtx.open_table(&events_self::TABLE)?;
    //         // dbtx.open_table(&events_missing::TABLE)?;
    //         // dbtx.open_table(&events_heads::TABLE)?;

    //         Self::handle_db_ver_migrations(dbtx)?;

    //         Ok(())
    //     })
    //     .await?;

    //     Ok(self)
    // }

    fn handle_db_ver_migrations(dbtx: &WriteTransaction) -> DbResult<()> {
        let mut table_db_ver = dbtx.open_table(&db_migration_ver::TABLE)?;

        let Some(cur_db_ver) = table_db_ver.first()?.map(|g| g.1.value()) else {
            info!(target: LOG_TARGET, "Initializing empty database");
            table_db_ver.insert(&(), &Self::DB_VER)?;

            return Ok(());
        };

        debug!(target: LOG_TARGET, db_ver = cur_db_ver, "Checking db version");
        if Self::DB_VER < cur_db_ver {
            return DbVersionTooHighSnafu {
                db_ver: cur_db_ver,
                code_ver: Self::DB_VER,
            }
            .fail();
        }

        // migration code will go here

        Ok(())
    }

    const MAX_CONTENT_LEN: u32 = 1_000_000u32;

    pub fn self_followees_list_subscribe(&self) -> watch::Receiver<()> {
        self.self_followee_list_updated.subscribe()
    }

    pub fn self_head_subscribe(&self) -> watch::Receiver<Option<ShortEventId>> {
        self.self_head_updated.subscribe()
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

    pub async fn get_self_followees(&self) -> Vec<(RostraId, PersonaId)> {
        self.read_with(|tx| {
            let ids_followees_table = tx.open_table(&ids_followees::TABLE)?;
            Ok(
                Database::read_followees_tx(self.self_id, &ids_followees_table)?
                    .into_iter()
                    .map(|(id, record)| (id, record.persona))
                    .collect(),
            )
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_event(
        &self,
        event_id: impl Into<ShortEventId>,
    ) -> Option<crate::db::event::EventRecord> {
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
    ) -> Option<EventContent> {
        let event_id = event_id.into();
        self.read_with(|tx| {
            let events_content_table = tx.open_table(&crate::db::events_content::TABLE)?;
            Ok(
                Database::get_event_content_tx(event_id, &events_content_table)?.and_then(
                    |content_state| match content_state {
                        crate::db::event::ContentStateRef::Present(b) => Some(b.into_owned()),
                        crate::db::event::ContentStateRef::Deleted { .. }
                        | crate::db::event::ContentStateRef::Pruned => None,
                    },
                ),
            )
        })
        .await
        .expect("Database panic")
    }

    pub async fn get_self_current_head(&self) -> Option<ShortEventId> {
        self.read_with(|tx| {
            let events_heads_table = tx.open_table(&events_heads::TABLE)?;

            Database::get_head_tx(self.self_id, &events_heads_table)
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
        event: &VerifiedEvent,
        content: &VerifiedEventContent,
    ) -> (InsertEventOutcome, ProcessEventState) {
        self.write_with(|tx| {
            let res = self.process_event_tx(event, tx)?;
            self.process_event_content_tx(content, tx)?;
            Ok(res)
        })
        .await
        .expect("Storage error")
    }

    pub fn process_event_tx(
        &self,
        event: &VerifiedEvent,
        tx: &WriteTransactionCtx,
    ) -> DbResult<(InsertEventOutcome, ProcessEventState)> {
        let mut events_table = tx.open_table(&events::TABLE)?;
        let mut events_content_table = tx.open_table(&events_content::TABLE)?;
        let mut events_missing_table = tx.open_table(&events_missing::TABLE)?;
        let mut events_heads_table = tx.open_table(&events_heads::TABLE)?;
        let mut events_by_time_table = tx.open_table(&events_by_time::TABLE)?;

        let insert_event_outcome = Database::insert_event_tx(
            event,
            &mut events_table,
            &mut events_by_time_table,
            &mut events_content_table,
            &mut events_missing_table,
            &mut events_heads_table,
        )?;

        if let InsertEventOutcome::Inserted { was_missing, .. } = insert_event_outcome {
            info!(target: LOG_TARGET,
                event_id = %event.event_id,
                author = %event.event.author,
                parent_prev = %event.event.parent_prev,
                parent_aux = %event.event.parent_aux,
                "New event inserted"
            );
            if event.event.author == self.self_id {
                let mut events_self_table = tx.open_table(&crate::db::events_self::TABLE)?;
                Database::insert_self_event_id(event.event_id, &mut events_self_table)?;

                if !was_missing {
                    info!(target: LOG_TARGET, event_id = %event.event_id, "New self head");

                    let sender = self.self_head_updated.clone();
                    let event_id = event.event_id.into();
                    tx.on_commit(move || {
                        let _ = sender.send(Some(event_id));
                    });
                }
            }
        }

        let process_event_content_state =
            if Self::MAX_CONTENT_LEN < u32::from(event.event.content_len) {
                Database::prune_event_content_tx(event.event_id, &mut events_content_table)?;

                ProcessEventState::Pruned
            } else {
                match insert_event_outcome {
                    InsertEventOutcome::AlreadyPresent => ProcessEventState::Existing,
                    InsertEventOutcome::Inserted { is_deleted, .. } => {
                        if is_deleted {
                            ProcessEventState::Deleted
                        } else {
                            // If the event was not there, and it wasn't deleted
                            // it definitely does not have content yet.
                            ProcessEventState::New
                        }
                    }
                }
            };
        Ok((insert_event_outcome, process_event_content_state))
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
        let mut events_content_table = tx.open_table(&events_content::TABLE)?;

        debug_assert!(Database::has_event_tx(
            event_content.event_id,
            &events_table
        )?);

        let content_added = if u32::from(event_content.event.content_len) < Self::MAX_CONTENT_LEN {
            Database::insert_event_content_tx(event_content, &mut events_content_table)?
        } else {
            false
        };

        if content_added {
            self.process_event_content_inserted_tx(event_content, tx)?;
        }
        Ok(())
    }

    /// After an event content was inserted process special kinds of event
    /// content, like follows/unfollows
    pub fn process_event_content_inserted_tx(
        &self,
        event_content: &VerifiedEventContent,
        tx: &WriteTransactionCtx,
    ) -> DbResult<()> {
        let author = event_content.event.author;
        let updated = match event_content.event.kind {
            EventKind::FOLLOW | EventKind::UNFOLLOW => {
                let mut ids_followees_t = tx.open_table(&crate::db::ids_followees::TABLE)?;
                let mut ids_followers_t = tx.open_table(&crate::db::ids_followers::TABLE)?;
                let mut id_unfollowed_t = tx.open_table(&crate::db::ids_unfollowed::TABLE)?;

                match event_content.event.kind {
                    EventKind::FOLLOW => match event_content.content.decode::<content::Follow>() {
                        Ok(follow_content) => Database::insert_follow_tx(
                            author,
                            event_content.event.timestamp.into(),
                            follow_content,
                            &mut ids_followees_t,
                            &mut ids_followers_t,
                            &mut id_unfollowed_t,
                        )?,
                        Err(err) => {
                            debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed ContentFollow payload");
                            false
                        }
                    },
                    EventKind::UNFOLLOW => {
                        match event_content.content.decode::<content::Unfollow>() {
                            Ok(unfollow_content) => Database::insert_unfollow_tx(
                                author,
                                event_content.event.timestamp.into(),
                                unfollow_content,
                                &mut ids_followees_t,
                                &mut ids_followers_t,
                                &mut id_unfollowed_t,
                            )?,
                            Err(err) => {
                                debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed ContentUnfollow payload");
                                false
                            }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            _ => false,
        };

        if updated && author == self.self_id {
            let sender = self.self_followee_list_updated.clone();
            tx.on_commit(move || {
                let _ = sender.send(());
            });
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
            let events_content_table = tx.open_table(&events_content::TABLE)?;

            Database::has_event_content_tx(event_id.into(), &events_content_table)
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
        if let Some(existing_self_id_record) = Self::read_self_id_tx(ids_self_t)? {
            if existing_self_id_record.rostra_id != self_id {
                return IdMismatchSnafu.fail();
            }
        } else {
            Self::write_self_id_tx(self_id, ids_self_t)?;
        };
        Ok(())
    }

    pub async fn get_head(&self, id: RostraId) -> DbResult<Option<ShortEventId>> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&events_heads::TABLE)?;

            Self::get_head_tx(id, &events_heads)
        })
        .await
    }

    pub async fn read_followees(
        &self,
        id: RostraId,
    ) -> DbResult<Vec<(RostraId, IdsFolloweesRecord)>> {
        self.read_with(|tx| {
            let ids_followees_table = tx.open_table(&ids_followees::TABLE)?;

            Self::read_followees_tx(id, &ids_followees_table)
        })
        .await
    }

    pub async fn read_followers(
        &self,
        id: RostraId,
    ) -> DbResult<Vec<(RostraId, IdsFollowersRecord)>> {
        self.read_with(|tx| {
            let ids_followers_table = tx.open_table(&ids_followers::TABLE)?;

            Self::read_followers_tx(id, &ids_followers_table)
        })
        .await
    }
}

fn get_first_in_range<K, V>(
    events_table: &impl ReadableTable<K, V>,
    range: impl ops::RangeBounds<K>,
) -> Result<Option<K>, DbError>
where
    K: bincode::Decode + bincode::Encode,
    V: bincode::Decode + bincode::Encode,
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
    K: bincode::Decode + bincode::Encode,
    V: bincode::Decode + bincode::Encode,
{
    Ok(events_table
        .range(range)?
        .next_back()
        .transpose()?
        .map(|(k, _)| k.value()))
}

pub enum InsertEventOutcome {
    /// An event already existed, so it changed nothing
    AlreadyPresent,
    Inserted {
        /// An event already had a parent reporting its existence.
        ///
        /// This also implies that the event is not considered a "head event"
        /// anymore.
        was_missing: bool,
        /// This event was already marked as deleted by some processed children
        /// event
        is_deleted: bool,
        /// An existing parent event had its content marked as deleted.
        ///
        /// Note, if the parent event was marked for deletion, but it was not
        /// processed yet, this will not be set, and instead `is_deleted` will
        /// be set to true, when the deleted parent is processed.
        deleted_parent_content: Option<ShortEventId>,

        /// Ids of parents storage was not aware of yet, so they are now marked
        /// as "missing"
        missing_parents: Vec<ShortEventId>,
    },
}

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
mod tests;
