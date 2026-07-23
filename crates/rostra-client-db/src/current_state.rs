use std::error::Error;
use std::fmt;

use tokio::sync::watch;

/// Error returned after a current-state publisher closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentStateClosed;

impl fmt::Display for CurrentStateClosed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("current-state publisher is closed")
    }
}

impl Error for CurrentStateClosed {}

/// A retained current-state subscription that returns owned snapshots.
///
/// The subscription initially contains the latest state published before it
/// was created. [`Self::snapshot`] and [`Self::changed`] clone that state while
/// holding the internal Tokio watch borrow only for the duration of the call,
/// so the returned value can safely live across awaits and database writes.
///
/// Cloning a subscription creates an independent change cursor over the same
/// retained state.
///
/// ```
/// # use rostra_client_db::Database;
/// # async fn inspect_state(db: &Database) {
/// let mut state = db.self_wot_subscribe();
/// let snapshot = state.snapshot();
///
/// // The snapshot is owned and may remain live across database operations.
/// let _heads = db.get_heads_self().await;
/// assert_eq!(
///     snapshot.len(),
///     snapshot.followees.len() + snapshot.extended.len()
/// );
///
/// let _newer_snapshot = state.changed().await;
/// # }
/// ```
///
/// The raw Tokio receiver and its borrow guard are intentionally not exposed:
///
/// ```compile_fail
/// # use rostra_client_db::Database;
/// # fn borrow_state(db: &Database) {
/// let state = db.self_wot_subscribe();
/// let _borrow = state.borrow();
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct CurrentState<T> {
    /// Internal retained-state receiver.
    receiver: watch::Receiver<T>,
}

impl<T> CurrentState<T> {
    pub(crate) fn new(receiver: watch::Receiver<T>) -> Self {
        Self { receiver }
    }
}

impl<T> CurrentState<T>
where
    T: Clone,
{
    /// Return an owned snapshot of the latest retained state.
    ///
    /// This does not advance the subscription's change cursor. A subsequent
    /// [`Self::changed`] call immediately returns if it has not yet observed
    /// the publication that supplied this snapshot.
    pub fn snapshot(&self) -> T {
        self.receiver.borrow().clone()
    }

    /// Wait for a publication newer than the cursor and return the latest
    /// owned snapshot.
    ///
    /// If multiple publications arrive before this call observes them, it
    /// coalesces them and returns the latest value. The returned value may
    /// equal a previous snapshot because snapshots do not advance the
    /// cursor and publishers may publish equal values.
    ///
    /// After the publisher closes, this method delivers any last unseen
    /// publication before returning [`CurrentStateClosed`]. [`Self::snapshot`]
    /// remains available after closure.
    ///
    /// This method is cancellation safe: using it in [`tokio::select!`] does
    /// not consume an update when another branch wins.
    pub async fn changed(&mut self) -> Result<T, CurrentStateClosed> {
        self.receiver
            .changed()
            .await
            .map_err(|_| CurrentStateClosed)?;
        Ok(self.receiver.borrow_and_update().clone())
    }
}
