use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::sync::oneshot;
use tokio::task::AbortHandle;

/// A parent's cancellation owner; the client registry retains the actual join.
pub(crate) struct ChildTask<T> {
    /// Cancels the child when its immediate parent goes away.
    pub(crate) abort: AbortHandle,
    /// Delivers the result without taking join ownership from the registry.
    pub(crate) result: oneshot::Receiver<T>,
}

impl<T> Future for ChildTask<T> {
    type Output = Result<T, oneshot::error::RecvError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.result).poll(cx)
    }
}

impl<T> Drop for ChildTask<T> {
    fn drop(&mut self) {
        self.abort.abort();
    }
}
