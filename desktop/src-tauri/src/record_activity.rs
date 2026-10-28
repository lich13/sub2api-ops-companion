use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::NSApplication;
use std::cell::OnceCell;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;

const BACKGROUND: &str = "应用已转入后台，已停止读取";

thread_local! {
    static APP: OnceCell<Option<Retained<NSApplication>>> = const { OnceCell::new() };
}

pub(crate) fn application_active() -> bool {
    APP.with(|cell| {
        cell.get_or_init(|| MainThreadMarker::new().map(NSApplication::sharedApplication))
            .as_ref()
            .is_some_and(|app| app.isActive())
    })
}

pub(crate) fn record_path(path: &str) -> bool {
    let route = path.split('?').next().unwrap_or("");
    matches!(route, "/usage-records" | "/usage-record-options")
        || route.starts_with("/usage-records/")
}

pub(crate) struct RecordActivity {
    active: AtomicBool,
    tx: watch::Sender<bool>,
}

impl Default for RecordActivity {
    fn default() -> Self {
        Self::new(false)
    }
}

impl RecordActivity {
    pub(crate) fn new(initial: bool) -> Self {
        Self {
            active: AtomicBool::new(initial),
            tx: watch::channel(initial).0,
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn set_active(&self, active: bool) -> bool {
        if self.active.load(Ordering::Relaxed) == active {
            return false;
        }
        self.tx.send_if_modified(|current| {
            if *current == active {
                self.active.store(active, Ordering::Release);
                return false;
            }
            *current = active;
            self.active.store(active, Ordering::Release);
            true
        })
    }

    pub(crate) async fn read<T>(
        &self,
        request: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        if !self.active.load(Ordering::Acquire) {
            return Err(BACKGROUND.into());
        }
        let mut activity = self.tx.subscribe();
        if !*activity.borrow_and_update() {
            return Err(BACKGROUND.into());
        }
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
        let request = || async {
            requests.fetch_add(1, Ordering::SeqCst);
            Ok(42)
        };
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
        for path in [
            "/usage-records",
            "/usage-records?after_id=42",
            "/usage-records/42",
            "/usage-record-options?kind=api_keys",
        ] {
            assert!(record_path(path));
        }
        for path in [
            "/snapshot",
            "/accounts/42/usage",
            "/quota-refresh",
            "/model-groups/1",
            "/usage-record-options-other",
        ] {
            assert!(!record_path(path));
        }
    }

    #[test]
    fn fast_path_mirror_tracks_initial_and_repeated_transitions() {
        let initially_active = RecordActivity::new(true);
        assert!(initially_active.active());
        assert!(!initially_active.set_active(true));
        assert!(initially_active.set_active(false));
        assert!(!initially_active.active());
        assert!(!initially_active.set_active(false));

        let initially_inactive = RecordActivity::new(false);
        assert!(!initially_inactive.active());
        assert!(!initially_inactive.set_active(false));
        assert!(initially_inactive.set_active(true));
        assert!(initially_inactive.active());
    }
}
