//! Owned background requests survive client disconnects but never service shutdown.
use crate::error::{Error, Result};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;

#[derive(Default)]
struct State {
    closed: bool,
    handles: Vec<JoinHandle<()>>,
}

#[derive(Clone, Default)]
pub struct Tasks {
    state: Arc<Mutex<State>>,
    closing: Arc<tokio::sync::Mutex<()>>,
    supervisor: crate::supervision::Supervisor,
}

impl Tasks {
    pub fn supervised(supervisor: crate::supervision::Supervisor) -> Self {
        Self {
            state: Default::default(),
            closing: Default::default(),
            supervisor,
        }
    }

    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed || self.supervisor.is_stopping() {
            return Err(Error::new(503, "服务正在停止"));
        }
        state.handles.retain(|handle| !handle.is_finished());
        state
            .handles
            .push(self.supervisor.spawn("owned_request", future));
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.supervisor.is_stopping()
            || self
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .closed
    }

    pub async fn close(&self, seconds: u64) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed = true;
        let _guard = self.closing.lock().await;
        let handles = std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .handles,
        );
        if handles.is_empty() {
            return;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        let mut remaining = Vec::new();
        for mut handle in handles {
            if tokio::time::timeout_at(deadline, &mut handle)
                .await
                .is_err()
            {
                remaining.push(handle);
            }
        }
        for handle in &remaining {
            handle.abort();
        }
        for handle in remaining {
            let _ = handle.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Tasks;
    use std::time::{Duration, Instant};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn close_never_misses_final_task_completion_wakeup() {
        for _ in 0..64 {
            let tasks = Tasks::default();
            for _ in 0..16 {
                tasks
                    .spawn(async {
                        tokio::task::yield_now().await;
                    })
                    .unwrap();
            }
            let started = Instant::now();
            tasks.close(55).await;
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "Tasks::close must finish immediately as JoinHandles complete"
            );
        }
    }
}
