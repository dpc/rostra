use std::sync::Mutex;

use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use n0_future::task::AbortOnDropHandle;
use tokio::sync::Notify;

use super::child_task::ChildTask;

/// Shared task registry with cancellation-safe asynchronous completion.
pub(crate) struct ClientTasks {
    /// Running handles or a retained, shared join future after abort.
    state: Mutex<TaskState>,
    /// Signals that dropping the unique task owner has requested termination.
    stopping: Notify,
}

enum TaskState {
    Running(Vec<AbortOnDropHandle<()>>),
    Stopping(Shared<BoxFuture<'static, ()>>),
}

impl Default for ClientTasks {
    fn default() -> Self {
        Self {
            state: Mutex::new(TaskState::Running(vec![])),
            stopping: Notify::new(),
        }
    }
}

impl ClientTasks {
    /// Admit and retain a task atomically with respect to shutdown.
    pub(crate) fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        self.spawn_registered(future);
    }

    /// Give a parent cancellation ownership while retaining the child's join.
    pub(crate) fn spawn_child<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Option<ChildTask<T>> {
        let (send, result) = tokio::sync::oneshot::channel();
        let abort = self.spawn_registered(async move {
            let _ = send.send(future.await);
        })?;
        Some(ChildTask { abort, result })
    }

    fn spawn_registered(
        &self,
        future: impl Future<Output = ()> + Send + 'static,
    ) -> Option<tokio::task::AbortHandle> {
        let mut state = self.state.lock().unwrap();
        let TaskState::Running(tasks) = &mut *state else {
            return None;
        };
        tasks.retain(|task| !task.is_finished());
        let task = tokio::spawn(future);
        let abort = task.abort_handle();
        tasks.push(AbortOnDropHandle::new(task));
        Some(abort)
    }

    /// Retain a task until the unique task owner requests its cancellation.
    #[cfg(test)]
    pub(crate) fn push(&self, task: AbortOnDropHandle<()>) {
        if let TaskState::Running(tasks) = &mut *self.state.lock().unwrap() {
            tasks.push(task);
        } else {
            panic!("test task submitted after shutdown");
        }
    }

    /// Request cancellation synchronously without requiring a running executor.
    pub(crate) fn abort(&self) {
        let mut state = self.state.lock().unwrap();
        let TaskState::Running(tasks) = &mut *state else {
            return;
        };
        let tasks = std::mem::take(tasks);
        for task in &tasks {
            task.abort();
        }
        *state = TaskState::Stopping(
            async move {
                for task in tasks {
                    let _ = task.await;
                }
            }
            .boxed()
            .shared(),
        );
        drop(state);
        self.stopping.notify_waiters();
    }

    /// Wait for owner-drop initiation and actual termination of every old task.
    ///
    /// Owner drop covers partial construction as well as the completed client.
    /// The stored shared join future survives cancellation of any one waiter.
    pub(crate) async fn terminated(&self) {
        loop {
            let notified = self.stopping.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let completion = match &*self.state.lock().unwrap() {
                TaskState::Running(_) => None,
                TaskState::Stopping(completion) => Some(completion.clone()),
            };
            if let Some(completion) = completion {
                completion.await;
                return;
            }
            notified.await;
        }
    }

    /// Return the running task count for activation regression tests.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        match &*self.state.lock().unwrap() {
            TaskState::Running(tasks) => tasks.len(),
            TaskState::Stopping(_) => 0,
        }
    }
}
