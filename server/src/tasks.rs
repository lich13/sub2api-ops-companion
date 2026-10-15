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
    supervisor: crate::supervision::Supervisor,
}

impl Tasks {
    pub fn supervised(supervisor: crate::supervision::Supervisor) -> Self {
        Self {
            state: Default::default(),
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
        let _ = tokio::time::timeout(Duration::from_secs(seconds), async {
            loop {
                if self
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .handles
                    .iter()
                    .all(JoinHandle::is_finished)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        let handles = std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .handles,
        );
        for handle in &handles {
            handle.abort();
        }
        for handle in handles {
            let _ = handle.await;
        }
    }
}
