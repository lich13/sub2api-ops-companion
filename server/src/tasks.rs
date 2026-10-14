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
pub struct Tasks(Arc<Mutex<State>>);

impl Tasks {
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return Err(Error::new(503, "服务正在停止"));
        }
        state.handles.retain(|handle| !handle.is_finished());
        state.handles.push(tokio::spawn(future));
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed
    }

    pub async fn close(&self, seconds: u64) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed = true;
        let _ = tokio::time::timeout(Duration::from_secs(seconds), async {
            loop {
                if self
                    .0
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
                .0
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
