mod models;
pub mod social;
mod tables;
mod tx_ops;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::{io, ops, result};

pub use ids::{IdsFolloweesRecord, IdsFollowersRecord};
use redb_bincode::{ReadTransaction, ReadableTable, WriteTransaction};
use rostra_core::event::{
    content_kind, EventContent, EventKind, PersonaId, VerifiedEvent, VerifiedEventContent,
};
use rostra_core::id::{RostraId, ToShort as _};
use rostra_core::ShortEventId;
use rostra_util_error::{BoxedError, FmtCompact as _};
use snafu::{Location, OptionExt as _, ResultExt as _, Snafu};
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
    #[snafu(display("Provided Id does not match one used previously"))]
    IdMismatch {
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
}

impl Database {
    pub async fn mk_db_path(
        data_dir: &Path,
        self_id: RostraId,
    ) -> std::result::Result<PathBuf, io::Error> {
        tokio::fs::create_dir_all(&data_dir).await?;
        Ok(data_dir.join(format!("{}.redb", self_id)))
    }

    #[instrument(skip_all)]
    pub async fn open(path: impl Into<PathBuf>, self_id: RostraId) -> DbResult<Database> {
        let path = path.into();
        debug!(target: LOG_TARGET, path = %path.display(), "Opening database");
        let inner = tokio::task::spawn_blocking(move || redb_bincode::Database::create(path))
            .await
            .context(JoinSnafu)?
            .context(DatabaseSnafu)?;

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

        let s = Self {
            inner,
            self_id,
            iroh_secret,
            self_followees_updated,
            self_followers_updated,
            self_head_updated,
            new_content_tx,
        };

        Ok(s)
    }

    pub async fn compact(&mut self) -> Result<bool, redb::CompactionError> {
        tokio::task::block_in_place(|| self.inner.as_raw_mut().compact())
    }

    const MAX_CONTENT_LEN: u32 = 1_000_000u32;

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
    ) -> Option<EventContent> {
        let event_id = event_id.into();
        self.read_with(|tx| {
            let events_content_table = tx.open_table(&crate::events_content::TABLE)?;
            Ok(
                Database::get_event_content_tx(event_id, &events_content_table)?.and_then(
                    |content_state| match content_state {
                        crate::event::EventContentState::Present(b) => Some(b.into_owned()),
                        crate::event::EventContentState::Deleted { .. }
                        | crate::event::EventContentState::Pruned => None,
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

    pub fn process_event_tx(
        &self,
        event: &VerifiedEvent,
        tx: &WriteTransactionCtx,
    ) -> DbResult<(InsertEventOutcome, ProcessEventState)> {
        let mut events_tbl = tx.open_table(&events::TABLE)?;
        let mut events_content_tbl = tx.open_table(&events_content::TABLE)?;
        let mut events_missing_tbl = tx.open_table(&events_missing::TABLE)?;
        let mut events_heads_tbl = tx.open_table(&events_heads::TABLE)?;
        let mut events_by_time_tbl = tx.open_table(&events_by_time::TABLE)?;
        let mut ids_full_tbl = tx.open_table(&ids_full::TABLE)?;

        let insert_event_outcome = Database::insert_event_tx(
            *event,
            &mut ids_full_tbl,
            &mut events_tbl,
            &mut events_by_time_tbl,
            &mut events_content_tbl,
            &mut events_missing_tbl,
            &mut events_heads_tbl,
        )?;

        if let InsertEventOutcome::Inserted {
            was_missing,
            is_deleted,
            deleted_parent,
            ref deleted_parent_content,
            ..
        } = insert_event_outcome
        {
            if is_deleted {
                info!(target: LOG_TARGET,
                    event_id = %event.event_id,
                    author = %event.event.author,
                    parent_prev = %event.event.parent_prev,
                    parent_aux = %event.event.parent_aux,
                    "Ignoring already deleted event"
                );
            } else {
                info!(target: LOG_TARGET,
                    event_id = %event.event_id,
                    author = %event.event.author,
                    parent_prev = %event.event.parent_prev,
                    parent_aux = %event.event.parent_aux,
                    "New event inserted"
                );
                if event.event.author == self.self_id {
                    let mut events_self_table = tx.open_table(&crate::events_self::TABLE)?;
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

            if let Some(content) = deleted_parent_content {
                let event_id = deleted_parent.expect("Must have the deleted event id");
                let event = events_tbl
                    .get(&event_id)?
                    .expect("Must have the event")
                    .value();
                let verified_event = VerifiedEvent::assume_verified_from_signed(event.signed);
                let verified_event_content =
                    VerifiedEventContent::assume_verified(verified_event, content.clone());
                self.revert_event_content_tx(&verified_event_content, tx)?;
            }
        }

        let process_event_content_state =
            if Self::MAX_CONTENT_LEN < u32::from(event.event.content_len) {
                Database::prune_event_content_tx(event.event_id, &mut events_content_tbl)?;

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
            event_content.event.event_id,
            &events_table
        )?);

        let content_added =
            if u32::from(event_content.event.event.content_len) < Self::MAX_CONTENT_LEN {
                Database::insert_event_content_tx(event_content, &mut events_content_table)?
            } else {
                false
            };

        if content_added {
            self.process_event_content_inserted_tx(event_content, tx)?;
            tx.on_commit({
                let new_content_tx = self.new_content_tx.clone();
                let event_content = event_content.clone();
                move || {
                    let _ = new_content_tx.send(event_content);
                }
            })
        }
        Ok(())
    }

    pub fn revert_event_content_tx(
        &self,
        event_content: &VerifiedEventContent,
        tx: &WriteTransactionCtx,
    ) -> DbResult<()> {
        #[allow(clippy::single_match)]
        match event_content.event.event.kind {
            EventKind::SOCIAL_POST => {
                    if let Ok(content) = event_content
                        .content
                        .deserialize_cbor::<content_kind::SocialPost>().inspect_err(|err| {
                            debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed SocialComment payload");
                        }) {

                        if let Some(reply_to) = content.reply_to {
                            let mut social_reply_tbl = tx.open_table(&social_post_reply::TABLE)?;
                            let mut social_post_tbl = tx.open_table(&social_post::TABLE)?;

                            social_reply_tbl.remove(
                                &(reply_to.event_id(), event_content.event.event.timestamp.into(),event_content.event.event_id.to_short())
                                )?;
                            let mut social_post_record = social_post_tbl.get(
                                &reply_to.event_id(),
                            )?.map(|g| g.value()).unwrap_or_default();

                            social_post_record.reply_count = social_post_record.reply_count.checked_sub(1).context(OverflowSnafu)?;

                            social_post_tbl.insert(&reply_to.event_id(), &social_post_record)?;
                        }
                    }
            }
            _ => {}
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
        let author = event_content.event.event.author;
        #[allow(clippy::single_match)]
        match event_content.event.event.kind {
            EventKind::FOLLOW | EventKind::UNFOLLOW => {
                let mut ids_followees_t = tx.open_table(&crate::ids_followees::TABLE)?;
                let mut ids_followers_t = tx.open_table(&crate::ids_followers::TABLE)?;
                let mut id_unfollowed_t = tx.open_table(&crate::ids_unfollowed::TABLE)?;

                let (followee, updated) = match event_content.event.event.kind {
                    EventKind::FOLLOW => {
                        match event_content.content.deserialize_cbor::<content_kind::Follow>() {
                            Ok(content) => (
                                Some(content.followee),
                                Database::insert_follow_tx(
                                    author,
                                    event_content.event.event.timestamp.into(),
                                    content,
                                    &mut ids_followees_t,
                                    &mut ids_followers_t,
                                    &mut id_unfollowed_t,
                                )?,
                            ),
                            Err(err) => {
                                debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed ContentFollow payload");
                                (None, false)
                            }
                        }
                    }
                    EventKind::UNFOLLOW => {
                        match event_content
                            .content
                            .deserialize_cbor::<content_kind::Unfollow>()
                        {
                            Ok(content) => (
                                Some(content.followee),
                                Database::insert_unfollow_tx(
                                    author,
                                    event_content.event.event.timestamp.into(),
                                    content,
                                    &mut ids_followees_t,
                                    &mut ids_followers_t,
                                    &mut id_unfollowed_t,
                                )?,
                            ),
                            Err(err) => {
                                debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed ContentUnfollow payload");
                                (None, false)
                            }
                        }
                    }
                    _ => unreachable!(),
                };

                if updated {
                    if author == self.self_id {
                        let followees_sender = self.self_followees_updated.clone();
                        let self_followees =
                            Database::read_followees_tx(self.self_id, &ids_followees_t)?;
                        tx.on_commit(move || {
                            let _ = followees_sender.send(self_followees);
                        });
                    }

                    if followee == Some(self.self_id) {
                        let followers_sender = self.self_followers_updated.clone();
                        let self_followers =
                            Database::read_followers_tx(self.self_id, &ids_followers_t)?;

                        tx.on_commit(move || {
                            let _ = followers_sender.send(self_followers);
                        });
                    }
                }
            }
            _ => match event_content.event.event.kind {
                EventKind::SOCIAL_PROFILE_UPDATE => {
                    if let Ok(content) = event_content
                        .content
                        .deserialize_cbor::<content_kind::SocialProfileUpdate>().inspect_err(|err| {
                            debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed SocialProfileUpdate payload");
                        }) {
                            Database::insert_latest_value_tx(
                                event_content.event.event.timestamp.into(),
                                &author,
                                IdSocialProfileRecord {
                                    event_id: event_content.event.event_id.to_short(),
                                    display_name: content.display_name,
                                    bio: content.bio,
                                    img_mime: content.img_mime,
                                    img: content.img,
                                },
                                &mut tx.open_table(&crate::social_profile::TABLE)?,
                            )?;
                    }
                }
                EventKind::SOCIAL_POST => {
                    if let Ok(content) = event_content
                        .content
                        .deserialize_cbor::<content_kind::SocialPost>().inspect_err(|err| {
                            debug!(target: LOG_TARGET, err = %err.fmt_compact(), "Ignoring malformed SocialComment payload");
                        }) {
                        if let Some(reply_to) = content.reply_to {
                            let mut social_comment_tbl = tx.open_table(&social_post_reply::TABLE)?;

                            let mut social_post_tbl= tx.open_table(&social_post::TABLE)?;

                            social_comment_tbl.insert(
                                &(reply_to.event_id(), event_content.event.event.timestamp.into(),event_content.event.event_id.to_short()
                                ),
                                &()
                            )?;
                            let mut social_post_record = social_post_tbl.get(
                                &reply_to.event_id(),
                            )?.map(|g| g.value()).unwrap_or_default();

                            social_post_record.reply_count = social_post_record.reply_count.checked_add(1).context(OverflowSnafu)?;

                            social_post_tbl.insert(&reply_to.event_id(), &social_post_record)?;
                        }

                    }
                },
                _ => {}
            },
        };

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

            Self::read_head_tx(id, &events_heads)
        })
        .await
    }

    pub async fn get_heads(&self, id: RostraId) -> DbResult<HashSet<ShortEventId>> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&events_heads::TABLE)?;

            Self::get_heads_tx(id, &events_heads)
        })
        .await
    }

    pub async fn get_heads_self(&self) -> DbResult<HashSet<ShortEventId>> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&events_heads::TABLE)?;

            Self::get_heads_tx(self.self_id, &events_heads)
        })
        .await
    }

    pub async fn get_social_profile(&self, id: RostraId) -> Option<IdSocialProfileRecord> {
        self.read_with(|tx| {
            let events_heads = tx.open_table(&social_profile::TABLE)?;

            Self::get_social_profile_tx(id, &events_heads)
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
        deleted_parent: Option<ShortEventId>,
        deleted_parent_content: Option<EventContent>,

        /// Ids of parents storage was not aware of yet, so they are now marked
        /// as "missing"
        missing_parents: Vec<ShortEventId>,
    },
}

impl InsertEventOutcome {
    fn validate(self) -> Self {
        if let InsertEventOutcome::Inserted {
            was_missing,
            is_deleted,
            deleted_parent,
            deleted_parent_content,
            ..
        } = &self
        {
            if *is_deleted {
                // Can't be missing if it was already deleted
                assert!(!was_missing);
            }

            if deleted_parent_content.is_some() {
                assert!(deleted_parent.is_some());
            }
        }
        self
    }
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
