//! One service-wide failure gate, independent of shutdown ownership.
use futures_util::FutureExt;
use std::{
    future::Future,
    panic::AssertUnwindSafe,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::{sync::watch, task::JoinHandle};

struct State {
    stopping: AtomicBool,
    failed: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct Supervisor(Arc<State>);

impl Default for Supervisor {
    fn default() -> Self {
        Self(Arc::new(State {
            stopping: AtomicBool::new(false),
            failed: watch::channel(false).0,
        }))
    }
}

impl Supervisor {
    pub fn is_stopping(&self) -> bool {
        self.0.stopping.load(Ordering::Acquire) || self.has_failed()
    }

    pub fn stop(&self) {
        self.0.stopping.store(true, Ordering::Release);
    }

    pub fn has_failed(&self) -> bool {
        *self.0.failed.borrow()
    }

    /// Static labels only. Never format the panic payload or a nested error.
    fn fail(&self, kind: &'static str) {
        if self.0.failed.send_if_modified(|failed| {
            if *failed {
                false
            } else {
                *failed = true;
                true
            }
        }) {
            eprintln!("background_failure task={kind} stage=run error_code=task_panicked");
        }
    }

    pub async fn failed(&self) {
        let mut receiver = self.0.failed.subscribe();
        loop {
            if *receiver.borrow_and_update() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    /// Cancellation drops this future normally; it is not a task failure.
    pub async fn guard<T>(&self, kind: &'static str, future: impl Future<Output = T>) -> Option<T> {
        match AssertUnwindSafe(future).catch_unwind().await {
            Ok(value) => Some(value),
            Err(_) => {
                self.fail(kind);
                None
            }
        }
    }

    pub fn spawn(
        &self,
        kind: &'static str,
        future: impl Future<Output = ()> + Send + 'static,
    ) -> JoinHandle<()> {
        let supervisor = self.clone();
        tokio::spawn(async move {
            supervisor.guard(kind, future).await;
        })
    }

    pub fn spawn_result<T: Send + 'static>(
        &self,
        kind: &'static str,
        future: impl Future<Output = crate::error::Result<T>> + Send + 'static,
    ) -> JoinHandle<crate::error::Result<T>> {
        let supervisor = self.clone();
        tokio::spawn(async move {
            supervisor
                .guard(kind, future)
                .await
                .unwrap_or_else(|| Err(crate::error::Error::internal()))
        })
    }
}

pub(crate) struct OnDrop<F: FnOnce()>(Option<F>);

pub(crate) fn on_drop<F: FnOnce()>(action: F) -> OnDrop<F> {
    OnDrop(Some(action))
}

/// A blocking calculation is part of its owning supervised task. Preserve
/// cancellation as an error, but propagate panic without formatting its payload.
pub(crate) async fn join<T>(handle: JoinHandle<T>) -> Result<T, tokio::task::JoinError> {
    match handle.await {
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        result => result,
    }
}

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(action) = self.0.take() {
            action();
        }
    }
}
