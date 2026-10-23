use super::*;
use axum::{
    body::Body,
    extract::Request,
    http::{Response, StatusCode},
    routing::any,
    Router,
};
use std::{
    future::Future,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::{
    net::TcpListener,
    sync::{Barrier, Notify},
    task::JoinHandle,
};

struct Site {
    base: String,
    task: JoinHandle<()>,
}
impl Drop for Site {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn site<F, Fut>(handler: F) -> Site
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response<Body>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/site/v1", listener.local_addr().unwrap());
    let handler = Arc::new(handler);
    let router = Router::new().fallback(any(move |request: Request| {
        let handler = handler.clone();
        async move { handler(request).await }
    }));
    Site {
        base,
        task: tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }),
    }
}
fn reply(status: u16, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::from(value.to_string()))
        .unwrap()
}
fn fixture() -> (tempfile::TempDir, Service) {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::new(dir.path().join("quota.json"), false).unwrap();
    (dir, service)
}
fn input(service: &Service, site: &Site, id: i64) -> Query {
    service
        .input(
            id,
            &json!({"base_url":site.base,"api_key":"fixture-key"}),
            &json!({"account":id,"parent":null}),
        )
        .unwrap()
}
fn set_seconds(s: &Service, seconds: u64) {
    s.save_settings(
        &json!({"expected_revision":s.settings().unwrap()["revision"],"seconds":seconds}),
        None,
    )
    .unwrap();
}
fn set_token(s: &Service, id: i64, token: &str) {
    s.save_settings(&json!({"expected_revision":s.account_settings(id).unwrap()["revision"],"access_token":token}), Some(id)).unwrap();
}

#[tokio::test]
async fn refresh_defaults_cache_disable_and_manual_read_are_preserved() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let site = site(move |req| {
        let counted = counted.clone();
        async move {
            assert_eq!(req.method(), "GET");
            assert_eq!(req.uri().path(), "/site/v1/usage");
            assert_eq!(req.headers()["authorization"], "Bearer fixture-key");
            counted.fetch_add(1, Ordering::SeqCst);
            reply(200, json!({"balance":0}))
        }
    })
    .await;
    let (_dir, s) = fixture();
    assert_eq!(s.settings().unwrap()["seconds"], 60);
    let a = s.query(input(&s, &site, 1), false).await.unwrap();
    assert_eq!(a.plans[0].remaining, Some(0.));
    s.query(input(&s, &site, 1), false).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    s.query(input(&s, &site, 1), true).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    set_seconds(&s, 0);
    assert_eq!(
        s.query(input(&s, &site, 2), false).await.unwrap().state,
        "idle"
    );
    s.query(input(&s, &site, 2), true).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn failure_keeps_success_and_manual_queries_honor_retry_after() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let site = site(move |_| {
        let counted = counted.clone();
        async move {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                reply(200, json!({"balance":7}))
            } else {
                Response::builder()
                    .status(429)
                    .header("retry-after", "120")
                    .body(Body::empty())
                    .unwrap()
            }
        }
    })
    .await;
    let (_dir, s) = fixture();
    let old = s.query(input(&s, &site, 1), true).await.unwrap();
    let failed = s.query(input(&s, &site, 1), true).await.unwrap();
    assert_eq!(failed.state, "error");
    assert_eq!(failed.plans, old.plans);
    assert_eq!(failed.success_at, old.success_at);
    assert!(failed.retry_at.unwrap() >= now() + 119);
    let repeated = s.query(input(&s, &site, 1), true).await.unwrap();
    assert_eq!(repeated.retry_at, failed.retry_at);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn duplicate_queries_share_a_flight_and_all_accounts_share_three_slots() {
    let count = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Barrier::new(2));
    let (c, g) = (count.clone(), gate.clone());
    let site = site(move |_| {
        let (c, g) = (c.clone(), g.clone());
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            g.wait().await;
            reply(200, json!({"balance":5}))
        }
    })
    .await;
    let (_dir, s) = fixture();
    let (a, b, _) = tokio::join!(
        s.query(input(&s, &site, 1), true),
        s.query(input(&s, &site, 1), true),
        gate.wait()
    );
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(site);
    let active = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let count = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Barrier::new(4));
    let (a, m, c, g) = (active.clone(), max.clone(), count.clone(), gate.clone());
    let site = self::site(move |_| {
        let (a, m, c, g) = (a.clone(), m.clone(), c.clone(), g.clone());
        async move {
            let current = a.fetch_add(1, Ordering::SeqCst) + 1;
            m.fetch_max(current, Ordering::SeqCst);
            if c.fetch_add(1, Ordering::SeqCst) < 3 {
                g.wait().await;
            }
            a.fetch_sub(1, Ordering::SeqCst);
            reply(200, json!({"balance":6}))
        }
    })
    .await;
    let queries = (1..=5).map(|id| s.query(input(&s, &site, id), true));
    let (results, _) = tokio::join!(futures_util::future::join_all(queries), gate.wait());
    assert!(results.into_iter().all(|r| r.is_ok()));
    assert_eq!(count.load(Ordering::SeqCst), 5);
    assert_eq!(max.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn account_token_and_key_stay_separate_and_partial_success_retains_currency() {
    let site=site(|req| async move {
        match req.uri().path() {
            "/site/api/user/self" => { assert_eq!(req.headers()["authorization"],"Bearer fixture-account"); reply(200,json!({"success":true,"data":{"quota":750000,"used_quota":250000,"request_count":12}})) }
            "/site/api/status" => { assert!(!req.headers().contains_key("authorization")); reply(200,json!({"data":{"quota_per_unit":500000,"quota_display_type":"USD"}})) }
            "/site/api/usage/token/" => { assert_eq!(req.headers()["authorization"],"Bearer fixture-key"); Response::builder().status(429).header("retry-after","60").body(Body::empty()).unwrap() }
            _ => panic!("unexpected protocol"),
        }
    }).await;
    let (_dir, s) = fixture();
    set_token(&s, 1, "fixture-account");
    let v = s.query(input(&s, &site, 1), true).await.unwrap();
    assert_eq!(v.state, "ok");
    assert_eq!(v.plans[0].name, "账户余额");
    assert_eq!(v.plans[0].unit, "USD");
    assert_eq!(v.plans[0].remaining, Some(1.5));
    assert!(v.error.is_some() && v.retry_at.is_some());
    assert!(!serde_json::to_string(&v).unwrap().contains("fixture-"));
}

#[tokio::test]
async fn automatic_detection_falls_back_and_redacts_echoed_secrets() {
    let site=site(|req| async move {
        match req.uri().path() {
            "/site/v1/usage" => reply(404,json!({})),
            "/site/api/usage/token/" => reply(200,json!({"success":true,"data":{"object":"token_usage","total_available":1000,"total_used":500,"total_granted":1500,"subscriptions":[{"name":"fixture-key","amount_total":2000,"amount_used":1000}]}})),
            "/site/api/status" => reply(200,json!({"data":{"quota_per_unit":100,"quota_display_type":"CNY","usd_exchange_rate":7}})),
            _ => panic!("unexpected protocol"),
        }
    }).await;
    let (_dir, s) = fixture();
    let v = s.query(input(&s, &site, 1), true).await.unwrap();
    assert_eq!(v.source.as_deref(), Some("newapi"));
    assert_eq!(v.plans[0].remaining, Some(70.));
    assert_eq!(v.subscriptions[0].plans[0].remaining, Some(70.));
    assert!(v.subscriptions[0].name.is_none());
}

#[tokio::test]
async fn replacing_credentials_does_not_allow_old_flight_to_overwrite_new_cache() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (e, r) = (entered.clone(), release.clone());
    let site = site(move |req| {
        let (e, r) = (e.clone(), r.clone());
        async move {
            if req.headers()["authorization"] == "Bearer fixture-key" {
                e.notify_one();
                r.notified().await;
                reply(200, json!({"balance":1}))
            } else {
                reply(200, json!({"balance":9}))
            }
        }
    })
    .await;
    let (_dir, s) = fixture();
    let old = input(&s, &site, 1);
    let task = {
        let s = s.clone();
        tokio::spawn(async move { s.query(old, true).await })
    };
    entered.notified().await;
    let replacement = || {
        s.input(
            1,
            &json!({"base_url":site.base,"api_key":"fixture-new-key"}),
            &json!({"parent":2}),
        )
        .unwrap()
    };
    let new = s.query(replacement(), true).await.unwrap();
    release.notify_one();
    task.await.unwrap().unwrap();
    let cached = s.query(replacement(), false).await.unwrap();
    assert_eq!(cached.plans, new.plans);
    assert_eq!(cached.plans[0].remaining, Some(9.));
}

#[tokio::test]
async fn unsupported_auth_network_and_redirect_are_distinct() {
    for (status, state, text) in [
        (404, "unsupported", "不可查询"),
        (401, "error", "认证失败"),
        (503, "error", "暂时不可用"),
        (302, "error", "跳转"),
    ] {
        let site = site(move |_| async move { reply(status, json!({})) }).await;
        let (_dir, s) = fixture();
        let v = s.query(input(&s, &site, 1), true).await.unwrap();
        assert_eq!(v.state, state);
        assert!(v.error.unwrap().contains(text));
    }
    let site = site(|_| async { reply(StatusCode::OK.as_u16(), json!({"balance":4})) }).await;
    let (_dir, s) = fixture();
    s.query(input(&s, &site, 1), true).await.unwrap();
    site.task.abort();
    let v = s.query(input(&s, &site, 1), true).await.unwrap();
    assert_eq!(v.state, "error");
    assert_eq!(v.plans[0].remaining, Some(4.));
}

#[test]
fn settings_are_private_persistent_conflict_checked_and_allow_explicit_clear() {
    let (dir, s) = fixture();
    let initial = s.settings().unwrap();
    for n in [1, 9, 86401] {
        assert!(s
            .save_settings(
                &json!({"seconds":n,"expected_revision":initial["revision"]}),
                None
            )
            .is_err());
    }
    set_seconds(&s, 120);
    assert!(s
        .save_settings(
            &json!({"seconds":60,"expected_revision":initial["revision"]}),
            None
        )
        .is_err());
    set_token(&s, 1, "fixture-account");
    let public = s.account_settings(1).unwrap();
    assert_eq!(public["has_access_token"], true);
    assert!(!public.to_string().contains("fixture-account"));
    s.save_settings(&json!({"expected_revision":public["revision"]}), Some(1))
        .unwrap();
    assert_eq!(s.account_settings(1).unwrap(), public);
    let restored = Service::new(dir.path().join("quota.json"), false).unwrap();
    assert_eq!(restored.settings().unwrap()["seconds"], 120);
    assert_eq!(restored.account_settings(1).unwrap(), public);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join("quota.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    set_token(&s, 1, "");
    assert_eq!(s.account_settings(1).unwrap()["has_access_token"], false);
}

#[tokio::test]
async fn unit_query_backoff_keeps_raw_values_and_respects_http_date_and_fractional_delay() {
    for (code, retry) in [
        (429, "1.5".to_string()),
        (
            503,
            httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(180)),
        ),
    ] {
        let requests = Arc::new(AtomicUsize::new(0));
        let calls = requests.clone();
        let site = site(move |req| {
            let (calls,retry)=(calls.clone(),retry.clone());
            async move {
                calls.fetch_add(1,Ordering::SeqCst);
                match req.uri().path() {
                    "/site/v1/usage" => reply(404,json!({})),
                    "/site/api/usage/token/" => reply(200,json!({"success":true,"data":{"object":"token_usage","total_available":1000}})),
                    "/site/api/status" => Response::builder().status(code).header("retry-after",retry).body(Body::empty()).unwrap(),
                    _ => panic!("unexpected route"),
                }
            }
        }).await;
        let (_dir, s) = fixture();
        let value = s.query(input(&s, &site, 1), true).await.unwrap();
        assert_eq!(value.state, "ok");
        assert_eq!(value.plans[0].unit, "额度单位");
        assert_eq!(value.plans[0].remaining, Some(1000.));
        assert!(value.error.is_some());
        assert!(value.retry_at.unwrap() >= now() + if code == 429 { 1 } else { 177 });
        s.query(input(&s, &site, 1), true).await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 3);
    }
}
