use std::future::Future;
use tokio::sync::watch;

const BACKGROUND: &str = "应用已转入后台，已停止读取";

pub(crate) fn application_active() -> bool {
    objc2::MainThreadMarker::new()
        .is_some_and(|main| objc2_app_kit::NSApplication::sharedApplication(main).isActive())
}

pub(crate) fn record_path(path: &str) -> bool {
    let route = path.split('?').next().unwrap_or("");
    matches!(route, "/usage-records" | "/usage-record-options")
        || route.starts_with("/usage-records/")
}

pub(crate) struct RecordActivity(watch::Sender<bool>);

impl Default for RecordActivity {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}

impl RecordActivity {
    pub(crate) fn active(&self) -> bool {
        *self.0.borrow()
    }

    pub(crate) fn set_active(&self, active: bool) -> bool {
        self.0.send_if_modified(|current| {
            if *current == active { return false; }
            *current = active;
            true
        })
    }

    pub(crate) async fn read<T>(
        &self,
        request: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let mut activity = self.0.subscribe();
        if !*activity.borrow_and_update() { return Err(BACKGROUND.into()); }
        tokio::select! {
            biased;
            _ = activity.changed() => Err(BACKGROUND.into()),
            result = request => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn inactive_application_never_starts_a_record_request() {
        let activity = RecordActivity::default();
        let requests = AtomicUsize::new(0);
        let request = || async { requests.fetch_add(1, Ordering::SeqCst); Ok(42) };
        assert!(activity.read(request()).await.is_err());
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        assert!(activity.set_active(true));
        assert_eq!(activity.read(request()).await.unwrap(), 42);
        assert!(activity.set_active(false));
        assert!(activity.read(request()).await.is_err());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        activity.set_active(true);
        assert_eq!(activity.read(request()).await.unwrap(), 42);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn deactivation_cancels_inflight_reads_and_rapid_refocus_cannot_revive_them() {
        let activity = RecordActivity::default();
        activity.set_active(true);
        let (started, wait_started) = tokio::sync::oneshot::channel();
        let request = async {
            started.send(()).unwrap();
            std::future::pending::<Result<(), String>>().await
        };
        let (result, ()) = tokio::join!(activity.read(request), async {
            wait_started.await.unwrap();
            assert!(!activity.set_active(true));
            activity.set_active(false);
            activity.set_active(true);
        });
        assert!(result.unwrap_err().contains("后台"));
        assert_eq!(activity.read(async { Ok(7) }).await.unwrap(), 7);
    }

    #[test]
    fn record_gate_does_not_include_background_snapshot_or_account_operations() {
        for path in ["/usage-records", "/usage-records?after_id=42", "/usage-records/42", "/usage-record-options?kind=api_keys"] {
            assert!(record_path(path));
        }
        for path in ["/snapshot", "/accounts/42/usage", "/quota-refresh", "/model-groups/1", "/usage-record-options-other"] {
            assert!(!record_path(path));
        }
    }
}
