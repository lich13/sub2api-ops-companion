use super::*;

pub(super) fn mock(state: &MockUpstream, path: &str) -> Option<Response> {
    let action = path.strip_prefix("/api/v1/admin/system/")?;
    if matches!(
        *state.manual_mode.lock().unwrap(),
        ManualMode::DisconnectedApplied
    ) {
        return Some(
            Response::builder()
                .status(200)
                .body(Body::from_stream(futures_util::stream::once(async {
                    Err::<Vec<u8>, std::io::Error>(std::io::Error::other("fixture disconnect"))
                })))
                .unwrap(),
        );
    }
    let status = state.update_status.load(Ordering::Acquire) as u16;
    let data = state.system_results.lock().unwrap().get(action).cloned().unwrap_or_else(|| match action {
        "version" => json!({"version":"0.2.20"}),
        "check-updates" => json!({"current_version":"0.2.20","latest_version":"0.2.21","has_update":true,"build_type":"release","cached":false}),
        "update" => json!({"message":"Update completed. Please restart the service.","need_restart":true,"operation_id":"fixture-update"}),
        "restart" => json!({"message":"Service restart initiated","operation_id":"fixture-restart"}),
        _ => Value::Null,
    });
    Some(response(
        StatusCode::from_u16(status).unwrap(),
        json!({"code":if status==200 {0} else {1},"data":data,"message":"fixture update failed"}),
    ))
}

#[tokio::test]
async fn native_system_routes_authenticate_forward_cached_reads_and_never_repeat_writes() {
    let mut h = Harness::new().await;
    assert_eq!(
        h.request_with_key(
            Method::POST,
            "sub2api-system/update",
            None,
            "fixture-rejected-key"
        )
        .await
        .0,
        401
    );
    assert!(h
        .upstream
        .matching(Method::POST, "/api/v1/admin/system/update")
        .is_empty());
    assert_eq!(
        h.request(Method::GET, "sub2api-system/version", None)
            .await
            .1["version"],
        "0.2.20"
    );
    assert_eq!(
        h.request(Method::GET, "sub2api-system/check-updates?force=true", None)
            .await
            .1["has_update"],
        true
    );
    assert_eq!(
        h.upstream
            .matching(Method::GET, "/api/v1/admin/system/check-updates")[0]
            .uri,
        "/api/v1/admin/system/check-updates?force=true"
    );
    assert_eq!(
        h.request(
            Method::GET,
            "sub2api-system/check-updates?force=invalid",
            None
        )
        .await
        .0,
        422
    );
    assert_eq!(
        h.request(Method::GET, "sub2api-system/update", None)
            .await
            .0,
        405
    );
    assert_eq!(
        h.request(Method::POST, "sub2api-system/rollback", None)
            .await
            .0,
        404
    );
    for (action, flag) in [("update", "need_restart"), ("restart", "message")] {
        let (status, result, _) = h
            .request(Method::POST, &format!("sub2api-system/{action}"), None)
            .await;
        assert_eq!(status, 200);
        assert!(!result[flag].is_null());
        assert_eq!(
            h.upstream
                .matching(Method::POST, &format!("/api/v1/admin/system/{action}"))
                .len(),
            1
        );
    }
    h.upstream.system_results.lock().unwrap().insert("update".into(),json!({"message":"Already up to date","already_up_to_date":true,"current_version":"0.2.20","latest_version":"0.2.20"}));
    let latest = h.request(Method::POST, "sub2api-system/update", None).await;
    assert_eq!(latest.0, 200);
    assert_eq!(latest.1["already_up_to_date"], true);
    assert!(latest.1.get("need_restart").is_none());
    h.upstream.update_status.store(500, Ordering::Release);
    let failed = h.request(Method::POST, "sub2api-system/update", None).await;
    assert_eq!(failed.0, 500);
    assert!(failed.2.contains("fixture update failed"));
    h.upstream.update_status.store(200, Ordering::Release);
    *h.upstream.manual_mode.lock().unwrap() = ManualMode::DisconnectedApplied;
    for action in ["update", "restart"] {
        let path = format!("/api/v1/admin/system/{action}");
        let before = h.upstream.matching(Method::POST, &path).len();
        assert_eq!(
            h.request(Method::POST, &format!("sub2api-system/{action}"), None)
                .await
                .0,
            502
        );
        assert_eq!(h.upstream.matching(Method::POST, &path).len(), before + 1);
    }
    let mut settings = (*h.runtime.settings).clone();
    settings.read_only = true;
    let readonly = Runtime::new(settings).await.unwrap();
    assert!(readonly.writable().is_err());
    assert!(sub2ops_server::sub2api_system::request(
        &readonly.actions,
        ADMIN_KEY,
        "update",
        &BTreeMap::new()
    )
    .await
    .is_err());
    readonly.close().await;
    h.finish().await;
}
