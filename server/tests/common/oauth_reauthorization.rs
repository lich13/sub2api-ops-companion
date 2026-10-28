use super::*;

#[tokio::test]
async fn reauthorization_preserves_configuration_rejects_overlap_and_never_replays_unknown_writes()
{
    let mut h = Harness::new().await;
    let group = h.insert_group("openai", false, &[]).await;
    let id = h
        .insert_account(
            "openai",
            "oauth",
            json!({"fixture-alias":"fixture-model"}),
            json!({"fixture_setting":true}),
            None,
            &[group],
        )
        .await;
    let path = format!("accounts/{id}/apply-oauth-credentials");
    let native = format!("/api/v1/admin/{path}");
    let body = json!({"credentials":{"access_token":"fixture-new-access","refresh_token":"fixture-new-refresh","chatgpt_account_id":"fixture-identity"},"extra":{"email":"fixture@example.invalid"}});
    let before = h.account(id).await;
    let (status, _, _) = h
        .request_with_key(
            Method::POST,
            &path,
            Some(body.clone()),
            "fixture-rejected-key",
        )
        .await;
    assert_eq!(status, 401);
    for invalid in [
        json!({"credentials":{"access_token":"fixture","model_mapping":{}}}),
        json!({"credentials":{"access_token":"fixture"},"proxy_id":3}),
        json!({"credentials":{"access_token":"fixture"},"extra":{"fixture_setting":false}}),
    ] {
        assert_eq!(h.request(Method::POST, &path, Some(invalid)).await.0, 422);
    }
    let lease = h.runtime.actions.locks.try_ids([id]).unwrap();
    assert_eq!(
        h.request(Method::POST, &path, Some(body.clone())).await.0,
        409
    );
    drop(lease);
    let gate = h.upstream.gate(&native);
    let client = h.client.clone();
    let endpoint = format!("{}/api/desktop/v1/{path}", h.base_url);
    let payload = body.clone();
    let first = tokio::spawn(async move {
        client
            .post(endpoint)
            .header("x-api-key", ADMIN_KEY)
            .json(&payload)
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        h.request(Method::POST, &path, Some(body.clone())).await.0,
        409
    );
    // An unrelated edit made in the native website while authorization is in flight survives.
    sqlx::query("UPDATE accounts SET priority=17,credentials=jsonb_set(credentials,'{model_mapping}','{\"fixture-alias\":\"fixture-new-model\"}') WHERE id=$1").bind(id).execute(&h.runtime.db.pool).await.unwrap();
    h.upstream.gates.lock().unwrap().remove(&native);
    gate.release.notify_one();
    let response = first.await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let result: Value = response.json().await.unwrap();
    assert_eq!(
        result,
        json!({"account_id":id,"verified":true,"status":"active"})
    );
    let after = h.account(id).await;
    assert_eq!(after["priority"], 17);
    assert_eq!(after["proxy_id"], before["proxy_id"]);
    assert_eq!(after["model_mapping"]["fixture-alias"], "fixture-new-model");
    assert_eq!(after["extra"]["fixture_setting"], true);
    assert_eq!(h.linked_groups(id).await, vec![group]);
    assert_eq!(
        h.request(Method::POST, &path, Some(body.clone())).await.0,
        200
    );
    assert_eq!(
        h.upstream.matching(Method::POST, &native).len(),
        1,
        "an identical credential submission is confirmed without replay"
    );
    h.upstream.update_status.store(502, Ordering::Release);
    let mut changed = body.clone();
    changed["credentials"]["access_token"] = "fixture-unknown-access".into();
    let (status, _, text) = h.request(Method::POST, &path, Some(changed)).await;
    assert_eq!(status, 502);
    assert!(!text.contains("fixture-unknown-access"));
    assert!(!text.contains("fixture-sensitive-upstream"));
    assert_eq!(h.upstream.matching(Method::POST, &native).len(), 2);
    h.upstream.update_status.store(200, Ordering::Release);
    h.upstream.apply_updates.store(false, Ordering::Release);
    assert_eq!(
        h.request(Method::POST, &path, Some(body.clone())).await.0,
        409,
        "readback must confirm actual credentials"
    );
    for (platform, kind) in [
        ("openai", "apikey"),
        ("anthropic", "oauth"),
        ("anthropic", "setup-token"),
        ("gemini", "service_account"),
    ] {
        let other = h
            .insert_account(platform, kind, json!({}), json!({}), None, &[])
            .await;
        assert_eq!(
            h.request(
                Method::POST,
                &format!("accounts/{other}/apply-oauth-credentials"),
                Some(body.clone())
            )
            .await
            .0,
            409
        );
    }
    for file in [
        &h.runtime.settings.usage_path,
        &h.runtime.settings.audit_path,
    ] {
        let data = fs::read_to_string(file).unwrap_or_default();
        assert!(!data.contains("fixture-new-access") && !data.contains("fixture-new-refresh"));
    }
    h.finish().await;
}
