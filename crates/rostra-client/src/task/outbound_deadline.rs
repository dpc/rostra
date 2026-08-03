//! Bounded network operations for background maintenance workers.

use std::future::Future;
use std::time::Duration;

/// Maximum time a maintenance worker gives one peer operation.
pub(crate) const PEER_OPERATION_DEADLINE: Duration = Duration::from_secs(30);

/// Maximum time one Web-of-Trust repair sweep may occupy its worker.
pub(crate) const WOT_SYNC_CYCLE_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// Run an operation until its worker-specific deadline expires.
pub(crate) async fn within<T>(
    deadline: Duration,
    operation: impl Future<Output = T>,
) -> Result<T, tokio::time::error::Elapsed> {
    tokio::time::timeout(deadline, operation).await
}
