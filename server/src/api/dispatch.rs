use super::*;
use futures_util::future::BoxFuture;

pub(super) struct Call<'a> {
    pub runtime: &'a Arc<Runtime>,
    pub key: &'a str,
    pub path: &'a [&'a str],
    pub query: &'a QueryParams,
    pub method: &'a Method,
    pub payload: &'a Value,
    pub accepts_gzip: bool,
    pub if_none_match: Option<&'a str>,
}

type Action<'a> = BoxFuture<'a, Result<Response>>;

// Construct the future now. Domain reads and mutations run only after authentication.
macro_rules! reply {
    ($value:expr) => {
        reply!(StatusCode::OK, $value)
    };
    ($status:expr, $value:expr) => {
        Box::pin(async move {
            let data = $value;
            Ok(($status, Json(data)).into_response())
        })
    };
}

impl<'a> Call<'a> {
    pub fn prepare(&self, route: Route<&'a str, i64>) -> Result<Action<'a>> {
        use Route::*;
        let Self {
            runtime: r,
            key,
            query: q,
            path,
            method,
            payload: p,
            accepts_gzip,
            if_none_match,
        } = *self;
        let accepted = StatusCode::ACCEPTED;
        let action: Action<'a> = match route {
            ConnectionStatus => reply!(r.upstream.connection()),
            Capabilities => reply!(sub2ops_protocol::dto::CapabilitiesReply {
                api_version: 1,
                platforms: vec!["macos".into(), "android".into()],
                service: "Sub2API Ops Companion".into(),
                version: crate::VERSION.into(),
                implementation: "rust".into(),
                build_revision: crate::BUILD_REVISION.into(),
                upstream_connection: r.upstream.connection(),
            }),
            Snapshot => Box::pin(async move {
                match r.snapshot_http(if_none_match, accepts_gzip).await? {
                    crate::snapshot::SnapshotReply::NotModified { etag, observed_at } => {
                        let mut response = StatusCode::NOT_MODIFIED.into_response();
                        let headers = response.headers_mut();
                        if let Ok(val) = HeaderValue::from_str(&etag) {
                            headers.insert(header::ETAG, val);
                        }
                        if let Ok(val) = HeaderValue::from_str(&observed_at) {
                            headers.insert("x-observed-at", val);
                        }
                        headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
                        Ok(response)
                    }
                    crate::snapshot::SnapshotReply::Full {
                        etag,
                        observed_at,
                        body,
                        gzipped,
                    } => {
                        let mut response = body.into_response();
                        let headers = response.headers_mut();
                        headers.insert(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("application/json"),
                        );
                        if gzipped {
                            headers
                                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                        }
                        if let Ok(val) = HeaderValue::from_str(&etag) {
                            headers.insert(header::ETAG, val);
                        }
                        if let Ok(val) = HeaderValue::from_str(&observed_at) {
                            headers.insert("x-observed-at", val);
                        }
                        headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
                        Ok(response)
                    }
                }
            }),
            Errors => {
                let account = q.number("account_id", 1, i64::MAX)?;
                let before = q.number("before_id", 1, i64::MAX)?;
                let limit = q.limit()?;
                let category = q.category()?;
                reply!(
                    r.actions
                        .queries
                        .errors(account, before, limit, category)
                        .await?
                )
            }
            ErrorDetail(id) => {
                let category = q.category()?;
                reply!(r.error_detail(id, category).await?)
            }
            Recoveries => {
                let before = q.number("before_id", 1, i64::MAX)?;
                let limit = q.limit()?;
                reply!(r.actions.queries.recoveries(before, limit).await?)
            }
            DetectionLogs => {
                let limit = q.limit()?;
                reply!(r.detection.logs(q.get("before"), limit)?)
            }
            UsageRecords => {
                let filter = q.usage_filter()?;
                reply!(
                    crate::usage_records::UsageRecords(r.db.clone())
                        .list_at(&filter, r.monitor.now())
                        .await?
                )
            }
            UsageRecord(id) => reply!(
                crate::usage_records::UsageRecords(r.db.clone())
                    .detail(id)
                    .await?
            ),
            UsageOptions => {
                let kind = q.get("kind").ok_or_else(Error::invalid)?;
                let user = q.number("user_id", 1, i64::MAX)?;
                let limit = q.limit()?;
                reply!(
                    crate::usage_records::UsageRecords(r.db.clone())
                        .options(kind, q.get("q"), user, q.get("cursor"), limit)
                        .await?
                )
            }
            AccountUsage(id) => {
                q.empty()?;
                reply!(r.account_usage(id, key, false).await?)
            }
            AccountSettings(id) => reply!(r.actions.settings_view(id).await?),
            AccountModels(id) => {
                let test_models = match q.get("purpose") {
                    None => false,
                    Some("model_test") => true,
                    _ => return Err(Error::invalid()),
                };
                let refresh = q.flag("refresh")?;
                reply!(account_models(r, key, id, test_models, refresh).await?)
            }
            SubmitOperation(id) => {
                let submission = crate::request_schema::submission(p)?;
                reply!(
                    accepted,
                    r.operations.submit_validated(id, submission, key).await?
                )
            }
            ApplyCredentials(id) => {
                q.empty()?;
                reply!(r.apply_oauth_credentials(id, key, p).await?)
            }
            Detection(id) if method == Method::GET => reply!(r.detection.view(id).await?),
            Detection(id) => reply!(r.detection.save_validated(id, p).await?),
            Keepalive(id) if method == Method::GET => reply!(r.keepalive.view(id).await?),
            Keepalive(id) => reply!(r.keepalive.save_validated(id, p).await?),
            AccountQuota(id) => reply!(
                r.key_quota_settings(id, (method == Method::PUT).then_some(p))
                    .await?
            ),
            AccountToken(id) => {
                q.empty()?;
                reply!({
                    r.key_quota_settings(id, None).await?;
                    r.key_quota.account_access_token(id)?
                })
            }
            StartModelTest(id) => reply!(r.tests.start_validated(id, p, None).await?),
            LatestModelTest(id) => reply!(r.tests.latest(id)?),
            ModelTest(id) => reply!(r.tests.get(id)?),
            CancelModelTest(id) => reply!(r.tests.cancel(id).await?),
            RetryModelTest(id) => reply!(r.tests.retry_failed(id, string(&p["request_id"])).await?),
            Operations => reply!(r.operations.listing(q.get("batch_id"))?),
            SubmitBatch | ApplyTemplate => reply!(
                accepted,
                r.operations
                    .submit_batch_validated(p, key, route == ApplyTemplate)
                    .await?
            ),
            CancelBatch(id) => reply!(r.operations.cancel_batch(id).await?),
            Operation(id) => {
                let after = q.number("after_event", 0, i64::MAX)?.unwrap_or(0) as usize;
                reply!(r.operations.get(id, after)?)
            }
            CancelOperation(id) => reply!(r.operations.cancel(id).await?),
            Priorities if method == Method::GET => reply!(r.priorities.view().await?),
            Priorities => reply!(r.priorities.set_parallelism_validated(p).await?),
            PriorityOrders => reply!({
                let order = r.priorities.submit_validated(p).await?;
                r.priorities.tick().await?;
                order
            }),
            PriorityOrder(id) => reply!(r.priorities.get(id, q.get("details") != Some("0"))?),
            CancelOrder(id) => reply!(r.priorities.cancel(id)?),
            Templates if method == Method::GET => {
                let id = q.number("account_id", 1, i64::MAX)?;
                reply!({
                    let row = match id {
                        Some(id) => Some(r.actions.account(id, None).await?),
                        None => None,
                    };
                    r.actions.templates.view(row.as_ref())?
                })
            }
            Templates => reply!(r.actions.templates.edit(None, p, false)?),
            Template(id) => {
                reply!(r
                    .actions
                    .templates
                    .edit(Some(id), p, method == Method::DELETE)?)
            }
            Proxies => reply!(r.actions.proxies(key).await?),
            ProbeProxy(id, action) => reply!(r.proxies.run(key, id, action).await?),
            ManageProxies | ProxyData | ProxyBatch | ProxyCredentials(_) | ProxyAccounts(_)
            | ManageProxy(_) => reply!(
                crate::proxy_management::handle(&r.actions, key, path, method.clone(), q.map(), p)
                    .await?
            ),
            Config => reply!(r.config.snapshot(None)?),
            GlobalQuota if method == Method::GET => reply!(r.key_quota.settings()?),
            GlobalQuota => reply!(r.key_quota.save_settings_validated(p, None)?),
            EditConfig(section) => reply!(
                r.config
                    .save(
                        section,
                        &p["changes"],
                        "desktop:admin",
                        Some(string(&p["expected_revision"]))
                    )
                    .await?
            ),
            BarkTest => reply!(bark_test(r).await?),
            QuotaRefresh if method == Method::GET => reply!(r.quota_progress()),
            QuotaRefresh => reply!(r.start_quota_refresh(key).await?),
            Bank("lm-detector") => reply!(r.tests.lm_bank.public().await?),
            Bank(_) => reply!(r.tests.bank.public()),
            SyncBank("lm-detector") => reply!(r.tests.lm_bank.request_sync().await?),
            SyncBank(_) => reply!(r.tests.bank.request_sync()?),
            SystemVersion | CheckUpdates | SystemUpdate | SystemRestart => {
                reply!(system_request(&r.actions, key, path[1], q.map()).await?)
            }
            HostConfig | HostOperation | ValidateHost | ApplyHost => {
                q.empty()?;
                let action = path.get(1).copied().unwrap_or("read");
                reply!(
                    r.sub2api_config
                        .request(action, &r.upstream.active_url(), key, p)
                        .await?
                )
            }
            CreateAccount
            | CreationOptions
            | CreationSource(_)
            | CreationCatalog(_)
            | CreationAuth(_, _)
            | CreationImport(_)
            | CreationCheck
            | CreationModels
            | CreationGroupPreview => {
                let sync = route == CreateAccount
                    && q.map().len() == 1
                    && q.get("sync_models") == Some("true");
                if !sync {
                    q.empty()?;
                }
                reply!(crate::account_creation::handle(&r.actions, key, path, p, sync).await?)
            }
        };
        Ok(action)
    }
}

async fn account_models(
    runtime: &Runtime,
    key: &str,
    account_id: i64,
    test_models: bool,
    refresh: bool,
) -> Result<Value> {
    Ok(if test_models {
        let row = runtime.actions.queries.private_account(account_id).await?;
        if crate::model_target::supported(&row) {
            json!(
                runtime
                    .actions
                    .model_test_candidates(&row, key, refresh)
                    .await?
            )
        } else {
            let identity = runtime.actions.candidate_source(&row, key).await?;
            let actions = runtime.actions.clone();
            let key = key.to_owned();
            let fetch = async move {
                let result: Result<Vec<Value>> = async {
                    let models = match crate::model_directory::account_models_read(
                        &actions.queries.db,
                        account_id,
                    )
                    .await
                    {
                        Ok(models) => models,
                        Err(error) if error.status.as_u16() == 422 => {
                            let data = actions.admin_models(account_id, &key).await?;
                            let items = data
                                .as_array()
                                .ok_or_else(|| Error::new(502, "模型列表格式无效"))?;
                            if items.len() > 10_000 {
                                return Err(Error::new(502, "上游模型列表超过限制"));
                            }
                            crate::actions::filter_models(items, &[])
                        }
                        Err(error) => return Err(error),
                    };
                    Ok(models)
                }
                .await;
                result.map_err(|error| {
                    if error.status.is_server_error()
                        && error.detail["code"] != crate::model_directory::DIRECTORY_REVOKED
                    {
                        crate::model_candidates::Failure::temporary(error)
                    } else {
                        crate::model_candidates::Failure::permanent(error)
                    }
                })
            };
            json!(
                runtime
                    .actions
                    .candidates
                    .get(identity, refresh, fetch)
                    .await?
            )
        }
    } else {
        json!(runtime.actions.models(account_id, key, false).await?)
    })
}

async fn system_request(
    actions: &crate::actions::Actions,
    key: &str,
    action: &str,
    query: &BTreeMap<String, String>,
) -> Result<Value> {
    if !query.is_empty()
        && !(action == "check-updates"
            && query.len() == 1
            && query
                .get("force")
                .is_some_and(|value| matches!(value.as_str(), "true" | "false")))
    {
        return Err(Error::invalid());
    }
    let write = matches!(action, "update" | "restart");
    let path = format!(
        "system/{action}{}",
        if query.get("force").is_some_and(|value| value == "true") {
            "?force=true"
        } else {
            ""
        }
    );
    // Native updates can take 15 minutes. Only this operation gets a longer deadline.
    // Send each write once; native operations continue even after a disconnected client.
    let response = actions
        .request(
            if write {
                reqwest::Method::POST
            } else {
                reqwest::Method::GET
            },
            &path,
            key,
            None,
            std::time::Duration::from_secs(if action == "update" { 915 } else { 30 }),
            false,
        )
        .await?;
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&crate::upstream::read_limited(response, 8_000_000).await?)
            .map_err(|_| Error::new(502, "无法读取 Sub2API 响应"))?;
    if !status.is_success() || body["code"] != 0 {
        let message = crate::security::safe_error(&body["message"], 1500);
        return Err(Error::new(
            if status.is_success() {
                502
            } else {
                status.as_u16()
            },
            if message.is_empty() {
                "Sub2API 未确认操作成功".into()
            } else {
                message
            },
        ));
    }
    if !body["data"].is_object() {
        return Err(Error::new(502, "无法读取 Sub2API 响应"));
    }
    Ok(body["data"].clone())
}

async fn bark_test(r: &Runtime) -> Result<Value> {
    let push = r
        .config
        .bark
        .push(
            "Sub2API Ops Companion",
            "Bark 推送通道正常。",
            8,
            &json!({}),
        )
        .await;
    if !push.success {
        return Err(Error::new(
            502,
            format!("Bark 测试失败：{}", push.error_code),
        ));
    }
    Ok(json!({"message":"Bark 测试消息已发送"}))
}
