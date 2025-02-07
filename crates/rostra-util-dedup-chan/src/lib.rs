//! Deduplicating multi-channel
//!
//! This channel is designed for broadcasting work to potentially multiple
//! worker threads, where multiple updates should be deduplicated while waiting
//! for processing.
//!
//! To use, first a [`Sender`] is created. Then [`Receiver`]s can be created
//! by calling [`Sender::subscribe`]. Each subscription creates a separate
//! channel.
//!
//! On [`Receiver::send`] a copy of an item will be addedto each subscribed
//! channel. If the item is already in the channel, yet unprocessed, it will not
//! be added again.
//!
//! Channels will be destroyed when the last [`Receiver`] is gone and the
//! channel is "disconnected".
//!
//! [`Receivers`] will be notified when the amount of pending work exceeds
//! `capacity`.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::{cmp, fmt, hash};

use snafu::Snafu;
use tokio::sync::watch;

#[derive(Snafu, Debug, PartialEq, Eq)]
pub enum RecvError {
    Closed,
    Lagging,
}

pub enum SendError<T> {
    Closed(T),
    Lagging(T),
}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SendError")
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        <Self as fmt::Debug>::fmt(self, f)
    }
}

impl<T> std::error::Error for SendError<T> {}

/// An inner part of a [`Channel`], shared with the [`Receiver`]s
#[derive(Clone)]
struct ChannelInner<T> {
    set: HashSet<T>,
    queue: VecDeque<T>,
    dropped_messages: bool,
}

/// A queue in which items are being collected for the
#[derive(Clone)]
struct Channel<T> {
    inner: Arc<std::sync::Mutex<ChannelInner<T>>>,
    tx: watch::Sender<usize>,
    capacity: usize,
}

impl<T> Channel<T>
where
    T: cmp::Eq + hash::Hash + Clone,
{
    /// Add a value to the channel
    ///
    /// Returns [`SendError`] if the channel is already at full capacity.
    /// Some receiver will be notified about the dropped message.
    pub async fn send(&mut self, v: T) -> std::result::Result<(), SendError<T>> {
        let mut lock = self.inner.lock().expect("locking failed");

        if lock.set.contains(&v) {
            return Ok(());
        }

        let len = lock.queue.len();
        if self.capacity <= len {
            lock.dropped_messages = true;
            return Err(SendError::Lagging(v));
        }

        if self.tx.send(len + 1).is_err() {
            return Err(SendError::Closed(v));
        };

        lock.set.insert(v.clone());
        lock.queue.push_back(v);
        Ok(())
    }
}

#[derive(Clone)]
pub struct Sender<T> {
    channels: BTreeMap<usize, Channel<T>>,
}

impl<T> Sender<T>
where
    T: cmp::Eq + hash::Hash + Clone,
{
    pub fn new() -> Self {
        Self {
            channels: Default::default(),
        }
    }
    /// Add a value to the channel
    ///
    /// Returns [`SendError`] if the channel is already at full capacity.
    /// Some receiver will be notified about the dropped message.
    ///
    /// Returns number of channel that the message was delivered to.
    pub async fn send(&mut self, v: T) -> usize {
        let mut to_delete = vec![];
        let mut sent_count = 0;

        for (k, inner) in &mut self.channels {
            match inner.send(v.clone()).await {
                Ok(_) => {
                    sent_count += 1;
                }
                Err(SendError::Closed(_)) => {
                    to_delete.push(*k);
                }
                Err(SendError::Lagging(_)) => {}
            }
        }

        for to_delete in to_delete {
            self.channels.remove(&to_delete).expect("Must be some");
        }

        sent_count
    }

    /// Subscribe to the `Sender`
    ///
    /// From now on, a copy of every sent item will be queued to be delivered to
    /// the returned `Receiver`.
    pub fn subscribe(&mut self, capacity: usize) -> Receiver<T> {
        let (sending_tx, sending_rx) = watch::channel(0);
        let inner = ChannelInner {
            set: HashSet::new(),
            queue: VecDeque::new(),
            dropped_messages: false,
        };

        let inner = Arc::new(Mutex::new(inner));

        let sender_inner = Channel {
            tx: sending_tx,
            inner: inner.clone(),
            capacity,
        };

        assert!(self
            .channels
            .insert(
                self.channels
                    .last_key_value()
                    .map(|(k, _)| *k + 1)
                    .unwrap_or_default(),
                sender_inner,
            )
            .is_none());
        Receiver {
            inner,
            rx: sending_rx,
        }
    }
}

impl<T> Default for Sender<T>
where
    T: cmp::Eq + hash::Hash + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

/// A receiving end of a deduplicated channel
///
/// Notably, when `Cloned` the new `Receiver` pulls work from the same
/// queue, leading to potential load-balancing.
///
/// To create a new, independent queue, call [`Sender::subscribe`] instead.
#[derive(Clone)]
pub struct Receiver<T> {
    inner: Arc<Mutex<ChannelInner<T>>>,
    rx: watch::Receiver<usize>,
}

impl<T> Receiver<T>
where
    T: cmp::Eq + hash::Hash,
{
    #[allow(clippy::await_holding_lock)] // clippy is wrong
    pub async fn recv(&mut self) -> std::result::Result<T, RecvError> {
        loop {
            let mut lock = self.inner.lock().expect("Locking error");
            let len = lock.queue.len();

            if len == 0 {
                if lock.dropped_messages {
                    lock.dropped_messages = false;
                    return Err(RecvError::Lagging);
                }
                drop(lock);

                if self.rx.changed().await.is_err() {
                    return Err(RecvError::Closed);
                }
                continue;
            }

            let v = lock
                .queue
                .pop_front()
                .expect("Must have a queue element when len > 0");

            if !lock.set.remove(&v) {
                panic!("Must have a set element when len > 0");
            }
            return Ok(v);
        }
    }
}

#[cfg(test)]
mod tests;
