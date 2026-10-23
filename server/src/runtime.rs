use crate::{
    account_locks::AccountLocks,
    account_templates::Templates,
    actions::Actions,
    bark::Bark,
    capacity_alerts::CapacityAlerts,
    config_service::ConfigService,
    credit::CreditController,
    db::Database,
    error::{Error, Result},
    execution::{DiskLease, TestSlots},
    fingerprint_bank::FingerprintBank,
    keepalive::Keepalive,
    model_catalog::Catalog,
    model_detection::Detection,
    model_tests::ModelTests,
    oauth_monitor::Monitor,
    oauth_queries::Coordinator,
    oauth_state::OAuthState,
    operations::Operations,
    proxy_diagnostics::ProxyDiagnostics,
    queries::Queries,
    runtime_hooks::Hooks,
    settings::Settings,
    upstream::Upstream,
};
use serde_json::{json, Value};
use std::{
    future::{Future, IntoFuture},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
use tokio::{
    sync::{watch, Semaphore},
    task::JoinHandle,
};

fn startup<T>(stage: &'static str, result: Result<T>) -> Result<T> {
    if let Err(error) = &result {
        eprintln!(
            "Sub2Ops startup stage {stage} failed ({})",
            error.status.as_u16()
        );
    }
    result
}

pub struct Runtime {
    pub settings: Arc<Settings>,
    pub db: Database,
    pub upstream: Upstream,
    pub actions: Actions,
    pub config: ConfigService,
    pub monitor: Monitor,
    pub tests: ModelTests,
    pub detection: Detection,
    pub keepalive: Keepalive,
    pub operations: Arc<Operations>,
    pub priorities: Arc<crate::priority_orders::PriorityOrders>,
    pub catalog: Catalog,
    pub proxies: ProxyDiagnostics,
    pub key_quota: crate::key_quota::Service,
    pub slots: TestSlots,
    pub budget: Arc<Semaphore>,
    pub closing: AtomicBool,
    pub(crate) snapshot: tokio::sync::Mutex<crate::snapshot::Cache>,
    pub(crate) cache_epoch: AtomicU64,
    pub(crate) quota_batch: Mutex<Value>,
    pub(crate) quota_task: Mutex<Option<JoinHandle<()>>>,
    pub(crate) usage_flights:
        Mutex<std::collections::BTreeMap<i64, crate::manual_actions::UsageFlight>>,
    pub(crate) requests: crate::tasks::Tasks,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    stop: watch::Sender<bool>,
    _instance: Option<DiskLease>,
}

impl Runtime {
    pub async fn new(settings: Settings) -> Result<Arc<Self>> {
        Self::new_with_clock(settings, Arc::new(chrono::Utc::now)).await
    }

    /// Share the scheduler clock before constructing hooks and task owners.
    pub async fn new_with_clock(
        settings: Settings,
        clock: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
    ) -> Result<Arc<Self>> {
        Self::new_with_clocks(settings, clock, Arc::new(std::time::Instant::now)).await
    }

    /// Keep cache expiry monotonic while allowing deterministic lifecycle tests.
    pub async fn new_with_clocks(
        settings: Settings,
        clock: Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
        monotonic: Arc<dyn Fn() -> std::time::Instant + Send + Sync>,
    ) -> Result<Arc<Self>> {
        let settings = Arc::new(settings);
        for (resource, path) in [
            ("oauth_state", settings.usage_path.clone()),
            ("oauth_config", settings.oauth_path.clone()),
            ("bark_config", settings.bark_path.clone()),
            ("connection_config", settings.connection_path.clone()),
            ("model_config", settings.model_path.clone()),
            ("keepalive", settings.keepalive_path.clone()),
            ("audit", settings.audit_path.clone()),
            (
                "priority_orders",
                settings
                    .usage_path
                    .with_file_name("account-priority-orders.json"),
            ),
            (
                "priority_orders_v2",
                settings
                    .usage_path
                    .with_file_name("account-priority-orders-v2.json"),
            ),
            (
                "operations",
                settings
                    .usage_path
                    .with_file_name("account-operations.json"),
            ),
            (
                "capacity",
                settings
                    .usage_path
                    .with_file_name("capacity-alert-state.json"),
            ),
            (
                "model_tests",
                settings.data_dir().join("model-test-results.json"),
            ),
            (
                "detection",
                settings.data_dir().join("model-detection-state.json"),
            ),
            (
                "fingerprints",
                settings.data_dir().join("modeltrace-bank-state.json"),
            ),
            (
                "templates",
                settings.data_dir().join("account-templates.json"),
            ),
            (
                "legacy_profiles",
                settings.data_dir().join("account-model-profiles.json"),
            ),
        ] {
            crate::store::diagnose_permissions(&path, resource);
        }
        let instance = if settings.read_only {
            None
        } else {
            Some(DiskLease::try_acquire(
                &settings.usage_path,
                "companion-instance",
                false,
            )?)
        };
        let db = Database::open(&settings.database_url, settings.read_only).await?;
        let upstream = startup("upstream", Upstream::new(settings.clone()))?;
        let oauth = OAuthState::new(settings.usage_path.clone(), settings.read_only);
        let queries = Queries {
            db: db.clone(),
            oauth,
        };
        let templates = Templates::new(
            settings.data_dir().join("account-templates.json"),
            settings.read_only,
        );
        let mut actions = Actions::new(
            queries.clone(),
            upstream.clone(),
            AccountLocks::default(),
            templates,
            settings.clone(),
        )?;
        actions.candidates =
            crate::model_candidates::Candidates::with_clock(actions.supervisor.clone(), monotonic);
        let mut bark = Bark::new(settings.clone())?;
        bark.supervise(actions.supervisor.clone());
        let config = ConfigService::new(settings.clone(), bark.clone(), queries);
        let hooks = Arc::new(Hooks(OnceLock::new()));
        let config_for_slots = config.clone();
        let slots = TestSlots::new(Arc::new(move || {
            config_for_slots
                .current()
                .ok()
                .and_then(|v| v["oauth_recovery_test_concurrency"].as_u64())
                .unwrap_or(2) as usize
        }));
        let config_for_queries = config.clone();
        let mut coordinator = Coordinator::new(
            actions.clone(),
            Arc::new(move || {
                config_for_queries
                    .current()
                    .ok()
                    .and_then(|v| v["oauth_usage_refresh_concurrency"].as_u64())
                    .unwrap_or(4) as usize
            }),
        );
        coordinator.clock = clock.clone();
        let budget = Arc::new(Semaphore::new(24));
        let mut credits = CreditController::new(
            actions.clone(),
            coordinator.clone(),
            hooks.clone(),
            slots.clone(),
            budget.clone(),
        );
        credits.clock = clock.clone();
        let mut monitor = Monitor::new(
            actions.clone(),
            config.clone(),
            coordinator,
            hooks.clone(),
            credits,
            slots.clone(),
            budget.clone(),
        );
        monitor.clock = clock.clone();
        let mut bank = startup("fingerprints", FingerprintBank::new(settings.clone()))?;
        bank.supervise(actions.supervisor.clone());
        let mut tests = ModelTests::new(actions.clone(), bank, hooks.clone(), budget.clone());
        tests.clock = clock.clone();
        let capacity = CapacityAlerts::new(db.clone(), &settings);
        let mut detection = Detection::new(
            actions.clone(),
            tests.clone(),
            monitor.clone(),
            capacity,
            bark,
        );
        detection.clock = clock;
        let operations = startup(
            "operations",
            Operations::new_deferred(actions.clone(), hooks.clone(), budget.clone()),
        )?;
        let keepalive = Keepalive::new(detection.clone(), operations.clone(), budget.clone());
        let priorities =
            crate::priority_orders::PriorityOrders::new(actions.clone(), operations.clone());
        let catalog = Catalog::new(&settings, db.clone(), upstream.clone())?;
        let proxies = ProxyDiagnostics::new(actions.clone());
        let key_quota = startup(
            "key_quota",
            crate::key_quota::Service::new(
                settings.data_dir().join("api-key-quota-config.json"),
                settings.read_only,
            ),
        )?;
        let requests = crate::tasks::Tasks::supervised(actions.supervisor.clone());
        let (stop, _) = watch::channel(false);
        let runtime = Arc::new(Self {
            settings,
            db,
            upstream,
            actions,
            config,
            monitor,
            tests,
            detection,
            keepalive,
            operations,
            priorities,
            catalog,
            proxies,
            key_quota,
            slots,
            budget,
            closing: AtomicBool::new(false),
            snapshot: tokio::sync::Mutex::new(crate::snapshot::Cache::default()),
            cache_epoch: AtomicU64::new(0),
            quota_batch: Mutex::new(json!({"status":"idle","items":[],"total":0,"completed":0})),
            quota_task: Mutex::new(None),
            usage_flights: Mutex::new(Default::default()),
            requests,
            tasks: Mutex::new(vec![]),
            stop,
            _instance: instance,
        });
        hooks
            .0
            .set(Arc::downgrade(&runtime))
            .map_err(|_| Error::internal())?;
        startup("preflight", runtime.preflight().await)?;
        Ok(runtime)
    }

    /// Validation is also used by --check-state. It performs no business writes,
    /// migrations, task dispatch or state-file creation.
    pub async fn preflight(&self) -> Result<Value> {
        startup("oauth_state", self.actions.queries.oauth.snapshot())?;
        startup("oauth_config", self.config.snapshot(None))?;
        startup("model_tests", self.tests.data())?;
        startup("detection", self.detection.store.read())?;
        startup("keepalive", self.keepalive.store.read())?;
        startup("capacity", self.detection.capacity.store.snapshot())?;
        startup("operations", self.operations.store.read())?;
        startup("priority_orders", self.priorities.legacy.read())?;
        startup("priority_orders_v2", self.priorities.store.read())?;
        startup(
            "templates",
            self.actions
                .templates
                .store
                .read()
                .and_then(crate::account_templates::normalize),
        )?;
        if let Some(profiles) = self.legacy_profiles() {
            startup("legacy_profiles", profiles.read())?;
        }
        startup(
            "model_config",
            crate::model_config::ModelConfig::new(
                self.settings.model_path.clone(),
                self.settings.read_only,
            )
            .read(),
        )?;
        startup("connection_config", self.settings.connection_config())?;
        let accounts = self.actions.queries.accounts().await?;
        self.actions.queries.groups(&accounts).await?;
        self.actions.queries.errors(None, None, 1, None).await?;
        Ok(
            json!({"ok":true,"api_version":1,"implementation":"rust","version":crate::VERSION,
            "build_revision":crate::BUILD_REVISION,"read_only":self.settings.read_only,"account_count":accounts.len()}),
        )
    }

    pub async fn start(self: &Arc<Self>) -> Result<()> {
        if self.settings.read_only {
            return Ok(());
        }
        startup("oauth_config", self.config.migrate_legacy().await)?;
        startup("operations", self.operations.store.recover_startup())?;
        startup("priority_orders", self.priorities.recover_startup().await)?;
        startup("legacy_recovery", self.migrate_legacy_recovery().await)?;
        startup("model_tests", self.tests.startup())?;
        startup(
            "capacity",
            self.detection.capacity.retire_legacy(chrono::Utc::now()),
        )?;
        startup("detection", self.detection.migrate_dispatches())?;
        startup("keepalive", self.keepalive.startup())?;
        startup("legacy_templates", self.retire_legacy_templates().await)?;
        startup("monitor", self.monitor.startup().await)?;
        self.detection.capacity.store.transaction(|state| {
            if state.get("detection_since").is_none() {
                state["detection_since"] = crate::compat::now().into();
            }
            Ok(())
        })?;
        self.refresh_route().await;
        self.tests.resume()?;
        for (kind, milliseconds) in [
            ("operations", 250),
            ("priority_orders", 1000),
            ("oauth", 2000),
            ("daily", 1000),
            ("notifications", 2000),
            ("credit_notifications", 2000),
            ("capacity", 2000),
            ("detection", 2000),
            ("keepalive", 2000),
            ("keepalive_notifications", 2000),
            ("disposition", 2000),
            ("route", 30000),
            ("bank", 300000),
        ] {
            let runtime = self.clone();
            let mut stop = self.stop.subscribe();
            let handle = self.actions.supervisor.spawn(kind, async move {
                let mut failures = 0_u32;
                loop {
                    if *stop.borrow() || runtime.actions.supervisor.is_stopping() {
                        break;
                    }
                    let started = std::time::Instant::now();
                    let result: Result<()> = match kind {
                        "operations" => runtime.operations.tick().await,
                        "priority_orders" => runtime.priorities.tick().await,
                        "oauth" => runtime.monitor.tick().await,
                        "daily" => runtime.config.current().and_then(|config| {
                            runtime
                                .monitor
                                .schedule
                                .tick(&config, runtime.monitor.now())
                                .map(|_| ())
                        }),
                        "notifications" => runtime.monitor.deliver().await,
                        "credit_notifications" => {
                            runtime
                                .monitor
                                .credits
                                .deliver_results(&runtime.config.bark)
                                .await
                        }
                        "capacity" => runtime.detection.capacity.poll().await,
                        "detection" => runtime.detection.tick().await,
                        "keepalive" => runtime.keepalive.tick().await,
                        "keepalive_notifications" => runtime.keepalive.deliver().await,
                        "disposition" => runtime.detection.deliver_notifications().await,
                        "route" => {
                            runtime.refresh_route().await;
                            Ok(())
                        }
                        "bank" => {
                            runtime.tests.bank.sync(false).await;
                            Ok(())
                        }
                        _ => unreachable!(),
                    };
                    if result.is_err() {
                        failures = (failures + 1).min(4);
                        crate::audit::write(
                            &runtime.settings.audit_path,
                            "runtime_task_error",
                            json!({"task":kind,"error_code":"cycle_failed"}),
                            false,
                        );
                    } else {
                        failures = 0;
                    }
                    let interval = Duration::from_millis(milliseconds);
                    let delay = match kind {
                        "oauth" => interval
                            .saturating_sub(started.elapsed())
                            .max(Duration::from_millis(50)),
                        "capacity" => Duration::from_secs((1_u64 << (failures + 1)).min(30)),
                        _ => interval,
                    };
                    tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = stop.changed() => {} }
                }
            });
            self.tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(handle);
        }
        Ok(())
    }

    /// The entrypoint owns shutdown; a failed worker only closes admission and
    /// signals this owner, so no worker can end up waiting for its own handle.
    pub async fn serve(
        self: &Arc<Self>,
        listener: tokio::net::TcpListener,
        shutdown: impl Future<Output = ()>,
    ) -> Result<()> {
        if let Err(error) = self.start().await {
            self.close().await;
            return Err(error);
        }
        let (stop, receive) = tokio::sync::oneshot::channel();
        let server = axum::serve(listener, crate::api::router(self.clone()))
            .with_graceful_shutdown(async {
                let _ = receive.await;
            })
            .into_future();
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => { self.close().await; result?; },
            _ = shutdown => {
                let _ = stop.send(());
                let _ = tokio::time::timeout(Duration::from_secs(60), async {
                    tokio::join!(self.close(), &mut server)
                }).await;
            },
            _ = self.actions.supervisor.failed() => {
                let _ = stop.send(());
                let _ = tokio::time::timeout(Duration::from_secs(60), async {
                    tokio::join!(self.close(), &mut server)
                }).await;
            },
        }
        if self.actions.supervisor.has_failed() {
            Err(Error::internal())
        } else {
            Ok(())
        }
    }

    async fn refresh_route(&self) {
        if let Ok(key) = self.actions.queries.oauth.admin_token() {
            if !key.is_empty() {
                let _ = self.upstream.authenticate(&key, false).await;
            }
        }
    }

    pub fn writable(&self) -> Result<()> {
        if self.settings.read_only {
            Err(Error::read_only())
        } else if self.closing.load(Ordering::Acquire) || self.actions.supervisor.is_stopping() {
            Err(Error::new(503, "服务正在停止"))
        } else {
            Ok(())
        }
    }

    pub async fn authenticate(&self, key: &str, fresh: bool) -> Result<()> {
        if self.actions.supervisor.is_stopping() {
            return Err(Error::new(503, "服务正在停止"));
        }
        self.upstream.authenticate(key, fresh).await?;
        if !self.settings.read_only {
            self.actions.queries.oauth.save_admin_token(key)?;
        }
        Ok(())
    }

    pub async fn close(&self) {
        if self.closing.swap(true, Ordering::AcqRel) {
            return;
        }
        self.actions.supervisor.stop();
        let _ = self.stop.send(true);
        let mut tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
        let quota = self
            .quota_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(quota) = quota {
            tasks.push(quota);
        }
        let aborts = tasks
            .iter()
            .map(JoinHandle::abort_handle)
            .collect::<Vec<_>>();
        let background = async {
            let _ = tokio::time::timeout(Duration::from_secs(55), async {
                for task in tasks {
                    let _ = task.await;
                }
            })
            .await;
            for abort in &aborts {
                abort.abort();
            }
        };
        tokio::join!(
            self.operations.close(),
            self.monitor.drain(55),
            self.tests.close(),
            self.keepalive.close(),
            self.requests.close(55),
            self.proxies.close(55),
            self.actions.candidates.close(),
            self.tests.bank.close(55),
            background
        );
        for abort in aborts {
            abort.abort();
        }
        self.usage_flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.db.close()).await;
    }
}
