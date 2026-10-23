use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use futures::future::BoxFuture;
use rostra_client_db::Database;
use snafu::Snafu;
use tokio::task::JoinHandle;

use super::Client;
use crate::error::PostResult;

#[cfg(test)]
mod tests;

/// An incomplete close or a failure to prove resource release.
#[derive(Debug, Snafu)]
pub enum ClientShutdownError {
    /// Another token already owns this client's shutdown.
    #[snafu(display("Client shutdown has already started"))]
    AlreadyClosing,
    /// Drop external client references and finish their operations, then retry.
    #[snafu(display("External client owners remain; release them and retry join"))]
    OutstandingClientOwners,
    /// Drop external database references and finish their operations, then
    /// retry.
    #[snafu(display("External database owners remain; release them and retry join"))]
    OutstandingDatabaseOwners,
    /// Database destruction failed; this token cannot certify a safe reopen.
    #[snafu(display("Database close task failed: {message}"))]
    DatabaseClose {
        /// The blocking task's failure, retained for subsequent join attempts.
        message: String,
    },
}

/// Retained completion of an irreversible, single-client shutdown.
///
/// Keep this token and its Tokio runtime alive until [`Self::join`] succeeds.
/// Cancelling a join future is safe: retry on the same token. Dropping the
/// token is only best-effort cleanup, not proof that storage can be reopened.
#[must_use = "join shutdown before reopening the database"]
pub struct ClientShutdown {
    /// Retained until all tasks terminate and external client owners disappear.
    client: Option<Arc<Client>>,
    /// Retained until atomic sole-owner acquisition succeeds.
    database: Option<Arc<Database>>,
    /// Retained transport close future, including across cancelled joins.
    endpoint_close: Option<BoxFuture<'static, ()>>,
    /// Blocking database destruction must survive cancelled join waiters.
    database_close: Option<JoinHandle<()>>,
    /// A failed destructor prevents claiming completion on subsequent retries.
    failure: Option<String>,
}

impl Client {
    /// Irreversibly start closing a caller-quiesced, single-client session.
    ///
    /// Stop admitting application operations and finish or cancel-and-join
    /// their jobs first. This consumes one strong reference, denies new weak
    /// handle upgrades and signing/activation entry, and aborts owned work.
    /// Existing operations may already have signed or committed; shutdown does
    /// not roll them back or flush publications to peers.
    ///
    /// Retain the returned token and await [`ClientShutdown::join`]. Do not use
    /// this on a [`crate::MultiClient`]-owned client: its manager deliberately
    /// retains resources. A second initiation returns `AlreadyClosing`; only
    /// the first token can certify completion.
    pub fn begin_shutdown(self: Arc<Self>) -> Result<ClientShutdown, ClientShutdownError> {
        self.closing
            .compare_exchange(false, true, SeqCst, SeqCst)
            .map_err(|_| ClientShutdownError::AlreadyClosing)?;
        self.active.store(false, SeqCst);
        self.task_handles.abort();
        Ok(ClientShutdown {
            client: Some(self),
            database: None,
            endpoint_close: None,
            database_close: None,
            failure: None,
        })
    }

    pub(super) fn require_open_for_signing(&self) -> PostResult<()> {
        snafu::ensure!(!self.closing.load(SeqCst), crate::error::ClientClosingSnafu);
        Ok(())
    }
}

impl ClientShutdown {
    /// Join all owned Rostra work and close the database before allowing
    /// reopen.
    ///
    /// Success means the old client and its database have been destroyed,
    /// descendant tasks (including blocking transaction sections) have
    /// finished, and its internally created iroh endpoint has completed
    /// close. No old client can sign or mutate storage after success. This
    /// is not a delivery acknowledgment, secret zeroization, or an in-place
    /// relock.
    ///
    /// External `Arc<Client>`/`ClientRef` or `Arc<Database>` owners return a
    /// retryable error. Release those owners, then retry on this token. No
    /// timeout is imposed: blocking transactions and transport close can delay
    /// completion. Cancelling this future retains all completion state.
    /// Repeated calls after success succeed.
    ///
    /// Injected iroh/Pkarr services remain caller-owned: their Rostra handles
    /// are released, but shared services are not shut down. Upstream internal
    /// threads, external connection handles, OS buffers, and previously issued
    /// signatures are not globally revoked by this API.
    pub async fn join(&mut self) -> Result<(), ClientShutdownError> {
        if let Some(message) = &self.failure {
            return Err(ClientShutdownError::DatabaseClose {
                message: message.clone(),
            });
        }
        if let Some(client) = &self.client {
            client.task_handles.terminated().await;
        }
        if let Some(client) = self.client.take() {
            let client = match Arc::try_unwrap(client) {
                Ok(client) => client,
                Err(client) => {
                    self.client = Some(client);
                    return Err(ClientShutdownError::OutstandingClientOwners);
                }
            };
            if client.owns_endpoint {
                let endpoint = client.networking.endpoint.clone();
                self.endpoint_close = Some(Box::pin(async move {
                    endpoint.close().await;
                }));
            }
            self.database = Some(client.db);
            drop(client.networking);
        }
        if let Some(close) = &mut self.endpoint_close {
            close.await;
            self.endpoint_close = None;
        }
        if let Some(database) = self.database.take() {
            match Arc::try_unwrap(database) {
                Ok(database) => {
                    self.database_close = Some(tokio::task::spawn_blocking(move || drop(database)));
                }
                Err(database) => {
                    self.database = Some(database);
                    return Err(ClientShutdownError::OutstandingDatabaseOwners);
                }
            }
        }
        if let Some(close) = &mut self.database_close {
            let result = close.await;
            self.database_close = None;
            if let Err(error) = result {
                let message = error.to_string();
                self.failure = Some(message.clone());
                return Err(ClientShutdownError::DatabaseClose { message });
            }
        }
        Ok(())
    }
}
