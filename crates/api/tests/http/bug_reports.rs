use super::*;
use serde_json::{json, Value};
use shiki_bug_report::{BugReportService, BugReportStore};

async fn call(app: &axum::Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(COOKIE, "shiki_session=sid-valid.default; shiki_csrf=csrf")
                .header("x-csrf-token", "csrf")
                .header("content-type", "application/json")
                .header("idempotency-key", uuid::Uuid::new_v4().to_string())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
#[tokio::test]
async fn bug_reports_http_manual_approval_conflicts_and_settings() {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let sessions = Arc::new(MemorySessionStore::new());
    let mut record = session_record(now() + 3600, None, "csrf");
    record.principal.groups = vec![format!("/bug-http-{}", uuid::Uuid::new_v4())];
    sessions
        .put("default", "sid-valid", &record, Duration::from_secs(3600))
        .await
        .unwrap();
    let mut state = state_with(sessions, None);
    // HTTP ごとに独立 org。実DBと監査を通す。サービスの認可境界は foundation_it の実OpenFGAでも検証する。
    state.bug_report = Some(Arc::new(
        BugReportService::new(BugReportStore::new(pool), Arc::new(AllowAll))
            .with_github_destination(1),
    ));
    let app = build_router(state);
    let (status, mut policy) =
        call(&app, "GET", "/org-admin/bug-reports/settings", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    policy["allow_content_sharing"] = json!(true);
    let (_, policy) = call(&app, "PUT", "/org-admin/bug-reports/settings", policy).await;
    let (status, _) = call(&app, "GET", "/bug-reports/policy", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let content = json!({"category":"bug","title":"表示がおかしい","symptom":"表示されない","steps_tried":"再読込","expected_behavior":"表示する"});
    let (status, report) = call(&app, "POST", "/bug-reports", content.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let id = report["id"].as_str().unwrap();
    let path = format!("/bug-reports/{id}");
    assert_eq!(
        call(&app, "GET", &path, Value::Null).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", &format!("/org-admin{path}"), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, report) = call(
        &app,
        "PATCH",
        &path,
        json!({"expected_rev":1,"content":content}),
    )
    .await;
    assert_eq!(
        call(
            &app,
            "PATCH",
            &path,
            json!({"expected_rev":1,"content":content})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let submit = json!({"expected_rev":report["rev"],"policy_rev":policy["rev"],"destination_version":1,"consent":true,"notice_version":"bug-report-v1"});
    let (status, report) = call(&app, "POST", &format!("{path}/submit"), submit.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["status"], "pending_approval");
    assert_eq!(
        call(&app, "POST", &format!("{path}/submit"), submit)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, page) = call(&app, "GET", "/bug-reports", Value::Null).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert!(page["items"][0].get("content").is_none());
    assert_eq!(
        call(&app, "GET", "/org-admin/bug-reports", Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let (_, detail) = call(&app, "GET", &format!("/org-admin{path}"), Value::Null).await;
    assert_eq!(detail["submission"]["content_consent"], true);
    let (_, rejected) = call(
        &app,
        "POST",
        &format!("/org-admin{path}/reject"),
        json!({"expected_rev":report["rev"],"reason":"手順を教えてください"}),
    )
    .await;
    assert_eq!(rejected["status"], "rejected");
    let (_, detail) = call(&app, "GET", &path, Value::Null).await;
    assert_eq!(
        detail["submission"]["rejection_reason"],
        "手順を教えてください"
    );
    let (_, draft) = call(
        &app,
        "POST",
        &format!("{path}/reopen"),
        json!({"expected_rev":rejected["rev"]}),
    )
    .await;
    let (_,submitted) = call(&app,"POST",&format!("{path}/submit"),json!({"expected_rev":draft["rev"],"policy_rev":policy["rev"],"destination_version":1,"consent":true,"notice_version":"bug-report-v1"})).await;
    let (status, approved) = call(
        &app,
        "POST",
        &format!("/org-admin{path}/approve"),
        json!({"expected_rev":submitted["rev"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(approved["status"], "queued");
    assert_eq!(
        call(
            &app,
            "PATCH",
            &path,
            json!({"expected_rev":approved["rev"],"content":content})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/org-admin{path}/retry"),
            json!({"expected_rev":approved["rev"]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, _) = call(
        &app,
        "POST",
        &format!("{path}/cancel"),
        json!({"expected_rev":approved["rev"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, current) = call(&app, "GET", "/org-admin/bug-reports/settings", Value::Null).await;
    assert_eq!(
        call(
            &app,
            "PUT",
            "/org-admin/bug-reports/settings",
            current.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "PUT", "/org-admin/bug-reports/settings", current)
            .await
            .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn help_case_http_requires_real_case_and_valid_resolution_body() {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let sessions = Arc::new(MemorySessionStore::new());
    sessions
        .put(
            "default",
            "sid-valid",
            &session_record(now() + 3600, None, "csrf"),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let mut state = state_with(sessions, None);
    state.bug_report = Some(Arc::new(BugReportService::new(
        BugReportStore::new(pool),
        Arc::new(AllowAll),
    )));
    let app = build_router(state);
    let path = format!(
        "/threads/{}/help-cases/{}",
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    );
    assert_eq!(
        call(&app, "GET", &path, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{path}/resolution"),
            json!("unresolved")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "POST", &format!("{path}/report-intent"), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{path}/resolution"),
            json!({"unresolved":true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
