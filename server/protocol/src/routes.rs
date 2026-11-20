//! Public desktop API. Both the HTTP dispatcher and credential-bearing client use this table.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestBody {
    None,
    Operation,
    Credentials,
    Detection,
    Keepalive,
    AccountQuota,
    ModelTest,
    Retry,
    Batch,
    Parallelism,
    PriorityOrder,
    CancelOrder,
    Template,
    TemplateBatch,
    DeleteTemplate,
    Proxy,
    GlobalQuota,
    Config,
    HostConfig,
    Creation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Capabilities,
    Connection,
    Snapshot,
    Errors,
    ErrorDetail,
    Recoveries,
    DetectionLogs,
    UsageRecords,
    UsageRecord,
    UsageOptions,
    Usage,
    AccountSettings,
    Models,
    Operation,
    VerifiedAccount,
    Detection,
    Keepalive,
    QuotaSettings,
    AccountToken,
    ModelTest,
    LatestModelTest,
    Operations,
    Batch,
    Priorities,
    PriorityOrder,
    Templates,
    Proxies,
    ProxyDiagnostic,
    ProxyList,
    Proxy,
    ProxyData,
    DeletedProxy,
    ProxyImport,
    ProxyBatch,
    ProxyCredentials,
    ProxyAccounts,
    Message,
    Config,
    QuotaRefresh,
    Bank,
    SystemVersion,
    SystemUpdates,
    SystemUpdate,
    HostConfig,
    HostOperation,
    HostValidation,
    CreatedAccount,
    CreationOptions,
    CreationSource,
    CreationCatalog,
    CreationAuth,
    CreationImport,
    CreationCheck,
    CreationModels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Host,
    Client,
}

#[derive(Clone, Copy)]
pub struct Endpoint {
    pub method: &'static str,
    pub body: RequestBody,
    pub reply: Reply,
}

macro_rules! routes {
    ($($variant:ident $(($($arg:ident:$kind:ident),+))? => $pattern:pat $(if $guard:expr)?,
        $scope:ident, [$($method:ident $body:ident $reply:ident),+];)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Route<W, N = W> {
            $($variant $(($($kind),+))?,)*
        }
        impl<'a> Route<&'a str> {
            pub fn resolve(parts: &[&'a str]) -> Option<Self> {
                match parts {
                    $($pattern $(if $guard)? => Some(Self::$variant $(($($arg),+))?),)*
                    _ => None,
                }
            }
            pub fn parse_numbers<N, E>(self, parse: impl Fn(&str) -> Result<N, E>) -> Result<Route<&'a str, N>, E> {
                Ok(match self {
                    $(Self::$variant $(($($arg),+))? => Route::$variant $(($(routes!(@convert $kind $arg parse)),+))?,)*
                })
            }
        }
        impl<W,N> Route<W, N> {
            pub fn endpoints(self) -> &'static [Endpoint] {
                match self {
                    $(Self::$variant $(($(routes!(@wild $arg)),+))? =>
                        &[$(Endpoint { method: stringify!($method), body: RequestBody::$body, reply: Reply::$reply }),+],)*
                }
            }
            pub fn scope(self) -> Scope {
                match self {
                    $(Self::$variant $(($(routes!(@wild $arg)),+))? => Scope::$scope,)*
                }
            }
            pub fn endpoint(self, method: &str) -> Option<&'static Endpoint> {
                self.endpoints().iter().find(|entry| entry.method == method)
            }
        }
    };
    (@convert W $arg:ident $convert:ident) => { $arg };
    (@convert N $arg:ident $convert:ident) => { $convert($arg)? };
    (@wild $arg:ident) => { _ };
}

routes! {
    Capabilities => ["capabilities"], Host, [GET None Capabilities];
    ConnectionStatus => ["connection-status"], Client, [GET None Connection];
    Snapshot => ["snapshot"], Host, [GET None Snapshot];
    Errors => ["errors"], Client, [GET None Errors];
    ErrorDetail(id:N) => ["errors", id], Client, [GET None ErrorDetail];
    Recoveries => ["recoveries"], Client, [GET None Recoveries];
    DetectionLogs => ["detection-logs"], Client, [GET None DetectionLogs];
    UsageRecords => ["usage-records"], Client, [GET None UsageRecords];
    UsageRecord(id:N) => ["usage-records", id], Client, [GET None UsageRecord];
    UsageOptions => ["usage-record-options"], Client, [GET None UsageOptions];
    AccountUsage(id:N) => ["accounts", id, "usage"], Client, [GET None Usage];
    AccountSettings(id:N) => ["accounts", id, "settings"], Client, [GET None AccountSettings];
    AccountModels(id:N) => ["accounts", id, "models"], Client, [GET None Models];
    SubmitOperation(id:N) => ["accounts", id, "operations"], Client, [POST Operation Operation];
    ApplyCredentials(id:N) => ["accounts", id, "apply-oauth-credentials"], Client, [POST Credentials VerifiedAccount];
    Detection(id:N) => ["accounts", id, "model-detection"], Client, [GET None Detection, PUT Detection Detection];
    Keepalive(id:N) => ["accounts", id, "keepalive"], Client, [GET None Keepalive, PUT Keepalive Keepalive];
    AccountQuota(id:N) => ["accounts", id, "key-quota-settings"], Client, [GET None QuotaSettings, PUT AccountQuota QuotaSettings];
    AccountToken(id:N) => ["accounts", id, "key-quota-settings", "access-token"], Client, [GET None AccountToken];
    StartModelTest(id:N) => ["accounts", id, "model-tests"], Client, [POST ModelTest ModelTest];
    LatestModelTest(id:N) => ["accounts", id, "model-tests", "latest"], Client, [GET None LatestModelTest];
    ModelTest(id:W) => ["model-tests", id], Client, [GET None ModelTest];
    CancelModelTest(id:W) => ["model-tests", id, "cancel"], Client, [POST None ModelTest];
    RetryModelTest(id:W) => ["model-tests", id, "retry"], Client, [POST Retry ModelTest];
    Operations => ["account-operations"], Client, [GET None Operations];
    SubmitBatch => ["account-operations", "batch"], Client, [POST Batch Batch];
    CancelBatch(id:W) => ["account-operations", "batches", id, "cancel"], Client, [POST None Batch];
    Operation(id:W) => ["account-operations", id], Client, [GET None Operation];
    CancelOperation(id:W) => ["account-operations", id, "cancel"], Client, [POST None Operation];
    Priorities => ["account-priorities"], Client, [GET None Priorities, PUT Parallelism Priorities];
    PriorityOrders => ["account-priority-orders"], Client, [POST PriorityOrder PriorityOrder];
    PriorityOrder(id:W) => ["account-priority-orders", id], Client, [GET None PriorityOrder];
    CancelOrder(id:W) => ["account-priority-orders", id, "cancel"], Client, [POST CancelOrder PriorityOrder];
    Templates => ["account-templates"], Client, [GET None Templates, POST Template Templates];
    ApplyTemplate => ["account-templates", "apply"], Client, [POST TemplateBatch Batch];
    Template(id:W) => ["account-templates", id], Client, [PUT Template Templates, DELETE DeleteTemplate Templates];
    Proxies => ["proxies"], Client, [GET None Proxies];
    ProbeProxy(id:N, action:W) => ["proxies", id, action @ ("test" | "quality-check")], Client, [POST None ProxyDiagnostic];
    ManageProxies => ["proxy-management"], Client, [GET None ProxyList, POST Proxy Proxy];
    ProxyData => ["proxy-management", "data"], Client, [GET None ProxyData, POST Proxy ProxyImport];
    ProxyBatch => ["proxy-management", "batch" | "batch-delete"], Client, [POST Proxy ProxyBatch];
    ProxyCredentials(id:W) => ["proxy-management", id, "credentials"], Client, [GET None ProxyCredentials];
    ProxyAccounts(id:W) => ["proxy-management", id, "accounts"], Client, [GET None ProxyAccounts];
    ManageProxy(id:W) => ["proxy-management", id], Client, [GET None Proxy, PUT Proxy Proxy, DELETE None DeletedProxy];
    Config => ["config"], Client, [GET None Config];
    GlobalQuota => ["config", "key-quota"], Client, [GET None QuotaSettings, PUT GlobalQuota QuotaSettings];
    EditConfig(section:W) => ["config", section @ ("oauth" | "bark")], Client, [PUT Config Config];
    BarkTest => ["actions", "bark-test"], Client, [POST None Message];
    QuotaRefresh => ["quota-refresh"], Client, [GET None QuotaRefresh, POST None QuotaRefresh];
    Bank(engine:W) => [engine @ ("modeltrace" | "lm-detector"), "fingerprint-bank"], Client, [GET None Bank];
    SyncBank(engine:W) => [engine @ ("modeltrace" | "lm-detector"), "fingerprint-bank", "sync"], Client, [POST None Bank];
    SystemVersion => ["sub2api-system", "version"], Client, [GET None SystemVersion];
    CheckUpdates => ["sub2api-system", "check-updates"], Client, [GET None SystemUpdates];
    SystemUpdate => ["sub2api-system", "update"], Client, [POST None SystemUpdate];
    SystemRestart => ["sub2api-system", "restart"], Client, [POST None Message];
    HostConfig => ["sub2api-config"], Client, [GET None HostConfig];
    HostOperation => ["sub2api-config", "operation"], Client, [GET None HostOperation];
    ValidateHost => ["sub2api-config", "validate"], Client, [POST HostConfig HostValidation];
    ApplyHost => ["sub2api-config", "apply"], Client, [POST HostConfig HostOperation];
    CreateAccount => ["account-creation"], Client, [POST Creation CreatedAccount];
    CreationOptions => ["account-creation", "options"], Client, [GET None CreationOptions];
    CreationSource(id:W) => ["account-creation", "source", id] if id.parse::<i64>().is_ok_and(|n| n > 0), Client, [GET None CreationSource];
    CreationCatalog(kind:W) => ["account-creation", "catalog", kind] if catalog_route(kind).is_some(), Client, [GET None CreationCatalog];
    CreationAuth(provider:W, action:W) => ["account-creation", "auth", provider, action] if auth_route(provider, action).is_some(), Client, [POST Creation CreationAuth];
    CreationImport(kind:W) => ["account-creation", "import", kind] if import_route(kind).is_some(), Client, [POST Creation CreationImport];
    CreationCheck => ["account-creation", "check-mixed-channel"], Client, [POST Creation CreationCheck];
    CreationModels => ["account-creation", "models"], Client, [POST Creation CreationModels];
}

pub fn auth_route(platform: &str, action: &str) -> Option<&'static str> {
    match (platform, action) {
        ("anthropic", "authorize") => Some("accounts/generate-auth-url"),
        ("anthropic", "exchange") => Some("accounts/exchange-code"),
        ("anthropic", "cookie") => Some("accounts/cookie-auth"),
        ("setup-token", "authorize") => Some("accounts/generate-setup-token-url"),
        ("setup-token", "exchange") => Some("accounts/exchange-setup-token-code"),
        ("setup-token", "cookie") => Some("accounts/setup-token-cookie-auth"),
        ("openai", "authorize") => Some("openai/generate-auth-url"),
        ("openai", "exchange") => Some("openai/exchange-code"),
        ("openai", "refresh") => Some("openai/refresh-token"),
        ("gemini", "authorize") => Some("gemini/oauth/auth-url"),
        ("gemini", "exchange") => Some("gemini/oauth/exchange-code"),
        ("antigravity", "authorize") => Some("antigravity/oauth/auth-url"),
        ("antigravity", "exchange") => Some("antigravity/oauth/exchange-code"),
        ("antigravity", "refresh") => Some("antigravity/oauth/refresh-token"),
        ("grok", "authorize") => Some("grok/oauth/auth-url"),
        ("grok", "exchange") => Some("grok/oauth/exchange-code"),
        ("grok", "refresh") => Some("grok/oauth/refresh-token"),
        _ => None,
    }
}
pub fn import_route(kind: &str) -> Option<&'static str> {
    match kind {
        "codex-session" => Some("accounts/import/codex-session"),
        "codex-pat" => Some("openai/create-from-codex-pat"),
        "grok-sso" => Some("grok/sso-to-oauth"),
        _ => None,
    }
}
pub fn catalog_route(kind: &str) -> Option<&'static str> {
    match kind {
        "gemini" => Some("gemini/oauth/capabilities"),
        "grok" => Some("grok/oauth/capabilities"),
        "tls" => Some("tls-fingerprint-profiles"),
        "settings" => Some("settings"),
        "web-search" => Some("settings/web-search-emulation"),
        "antigravity-models" => Some("accounts/antigravity/default-model-mapping"),
        _ => None,
    }
}

pub fn client_endpoint(method: &str, path: &str) -> Option<&'static Endpoint> {
    if path.contains(['#', '\\']) || path.bytes().any(|b| b.is_ascii_control()) {
        return None;
    }
    let plain = path.split_once('?').map_or(path, |(p, _)| p);
    let segments = plain.strip_prefix('/')?.split('/').collect::<Vec<_>>();
    if segments.iter().any(|s| {
        s.is_empty()
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    }) {
        return None;
    }
    let route = Route::resolve(&segments)?;
    if route.scope() != Scope::Client {
        return None;
    }
    let route = route
        .parse_numbers(|s| s.parse::<i64>().ok().filter(|n| *n > 0).ok_or(()))
        .ok()?;
    let job_id = |id: &str| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit());
    match route {
        Route::PriorityOrder(id) | Route::CancelOrder(id) | Route::CancelBatch(id)
            if !job_id(id) || id.bytes().any(|b| b.is_ascii_uppercase()) =>
        {
            None
        }
        Route::ModelTest(id)
        | Route::CancelModelTest(id)
        | Route::RetryModelTest(id)
        | Route::Operation(id)
        | Route::CancelOperation(id)
            if !job_id(id) =>
        {
            None
        }
        Route::Template(id)
            if !matches!(id, "full" | "degraded" | "takeover")
                && !id.strip_prefix("custom-").is_some_and(|s| {
                    s.len() == 24
                        && s.bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                }) =>
        {
            None
        }
        Route::ManageProxy(id) | Route::ProxyAccounts(id) | Route::ProxyCredentials(id)
            if !id.parse::<i64>().is_ok_and(|n| n > 0) =>
        {
            None
        }
        _ => route.endpoint(method),
    }
}
