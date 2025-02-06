use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::{cmp, fmt, hash};

use snafu::Snafu;
use tokio::sync::watch;

#[derive(Snafu, Debug)]
pub enum RecvError {
    Closed,
    Lagging,
}

pub struct SendError<T>(pub T);

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

#[derive(Clone)]
struct Inner<T> {
    set: HashSet<T>,
    queue: VecDeque<T>,
    dropped_messages: bool,
}

/// See [`channel`]
#[derive(Clone)]
pub struct Sender<T> {
    inner: Arc<std::sync::Mutex<Inner<T>>>,
    tx: watch::Sender<usize>,
    capacity: usize,
}

impl<T> Sender<T>
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
            return Err(SendError(v));
        }

        if self.tx.send(len + 1).is_err() {
            return Err(SendError(v));
        };

        lock.set.insert(v.clone());
        lock.queue.push_back(v);
        Ok(())
    }

    pub fn subscribe(&self) -> Receiver<T> {
        Receiver {
            inner: self.inner.clone(),
            rx: self.tx.subscribe(),
        }
    }
}

/// See [`channel`]
pub struct Receiver<T> {
    inner: Arc<Mutex<Inner<T>>>,
    rx: watch::Receiver<usize>,
}

impl<T> Receiver<T>
where
    T: cmp::Eq + hash::Hash,
{
    pub async fn recv(&mut self) -> std::result::Result<T, RecvError> {
        loop {
            if self.rx.wait_for(|len| 0 < *len).await.is_err() {
                return Err(RecvError::Closed);
            }

            let mut lock = self.inner.lock().expect("Locking error");
            let len = lock.queue.len();

            if len == 0 {
                continue;
            }

            if lock.dropped_messages {
                lock.dropped_messages = false;
                return Err(RecvError::Lagging);
            }

            let v = lock
                .queue
                .pop_front()
                .expect("Must have a queue element when len > 0");

            if !lock.set.remove(&v) {
                panic!("Must have a set element when len > 0");
            }
        }
    }
}

/// Deduplicating work channel
///
/// This channel is designed for passing work items to multiple worker
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let (sending_tx, sending_rx) = watch::channel(0);
    let inner = Inner {
        set: HashSet::new(),
        queue: VecDeque::new(),
        dropped_messages: false,
    };

    let inner = Arc::new(Mutex::new(inner));

    (
        Sender {
            tx: sending_tx,
            inner: inner.clone(),
            capacity,
        },
        Receiver {
            inner,
            rx: sending_rx,
        },
    )
}
