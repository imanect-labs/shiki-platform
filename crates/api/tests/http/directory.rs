use super::*;
use serde_json::{json, Value};
use storage::directory::admin::{DirectoryChangeSet, DirectoryOperation};
#[path = "directory_ad.rs"]
mod ad;
#[path = "directory_keycloak.rs"]
mod keycloak;
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
async fn fixture() -> Option<(
    AppState,
    authz::AuthContext,
    Arc<std::sync::Mutex<keycloak::KeycloakFixture>>,
)> {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let sessions = Arc::new(MemorySessionStore::new());
    let mut record = session_record(now() + 3600, None, "csrf");
    let org = format!("directory-{}", uuid::Uuid::new_v4().simple());
    record.principal.groups = vec![format!("/tenants/default/{org}")];
    let ctx = authz::AuthContext::new(
        record.principal.clone(),
        org,
        authz::TenantId::parse("default").unwrap(),
    );
    sessions
        .put("default", "sid-valid", &record, Duration::from_secs(3600))
        .await
        .unwrap();
    let (base, fixture) = keycloak::spawn().await;
    let mut state = state_with(sessions, None);
    state.directory = Arc::new(storage::DirectoryStore::new(pool.clone()));
    state.audit = Arc::new(storage::audit::AuditRecorder::new(pool));
    let config = Arc::make_mut(&mut state.config);
    config.auth.admin_base_url = Some(format!("{base}/admin/realms/shiki"));
    config.auth.internal_base_url = Some(format!("{base}/realms/shiki"));
    config.auth.provisioner_client_id = Some("service".into());
    config.auth.provisioner_client_secret = Some("service-secret".into());
    config.auth.directory_ldap_allowed_hosts = vec!["ad.example.test".into()];
    Some((state, ctx, fixture))
}
#[tokio::test]
async fn directory_http_connection_sync_plan_apply_replay_and_preview() {
    let Some((state, ctx, fixture)) = fixture().await else {
        return;
    };
    let app = build_router(state.clone());
    let base = "/org-admin/directory";
    let (status, info) = call(&app, "GET", base, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["keycloak_available"], true);
    let input = json!({"connection_url":"ldaps://ad.example.test:636","users_dn":"OU=Users,DC=example","groups_dn":"OU=Groups,DC=example","bind_dn":"CN=reader,DC=example","bind_password":"sensitive-bind-secret"});
    let (status, _) = call(
        &app,
        "POST",
        &format!("{base}/connection/test"),
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, connection) =
        call(&app, "PUT", &format!("{base}/connection"), input.clone()).await;
    if status != StatusCode::OK {
        let sdk = api::keycloak_admin::KeycloakAdmin::from_config(&state.http, &state.config.auth)
            .unwrap();
        let input = serde_json::from_value(input.clone()).unwrap();
        if let Err(error) = sdk.save_ldap(&ctx, &input).await {
            panic!("LDAP fixture: {error}");
        }
    }
    assert_eq!(status, StatusCode::OK, "{connection}");
    assert_eq!(connection["credential_saved"], true);
    let sdk =
        api::keycloak_admin::KeycloakAdmin::from_config(&state.http, &state.config.auth).unwrap();
    let mut moved: api::keycloak_admin::ldap::LdapConnectionInput =
        serde_json::from_value(input.clone()).unwrap();
    moved.connection_url = "ldaps://other-allowed.example.test:636".into();
    moved.bind_password = None;
    let writes = fixture.lock().unwrap().writes.len();
    assert!(sdk.test_ldap(&ctx, &moved).await.is_err());
    moved.bind_password = Some("**********".into());
    assert!(sdk.test_ldap(&ctx, &moved).await.is_err());
    assert!(sdk.save_ldap(&ctx, &moved).await.is_err());
    assert_eq!(fixture.lock().unwrap().writes.len(), writes);

    assert!(!connection.to_string().contains("sensitive-bind-secret"));
    assert_eq!(
        call(&app, "GET", &format!("{base}/connection"), Value::Null)
            .await
            .0,
        StatusCode::OK
    );
    let mut update = input.clone();
    update["bind_password"] = Value::Null;
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), update)
            .await
            .0,
        StatusCode::OK
    );
    let source_id = {
        let mut kc = fixture.lock().unwrap();
        let root = kc
            .groups
            .iter()
            .find(|g| g["path"] == format!("/tenants/default/{}/ad-guid-v1", ctx.org()))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        kc.add_group(
            Some(&root),
            &uuid::Uuid::new_v4().to_string(),
            json!({"distinguishedName":["CN=営業グループ,OU=営業,OU=本社,DC=example"]}),
        )
    };
    let (status, op) = call(
        &app,
        "POST",
        &format!("{base}/sync"),
        json!({"idempotency_key":uuid::Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{op}");
    api::directory_admin::process(&state, &ctx, op["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap();
    let (_, folders) = call(&app, "GET", &format!("{base}/folders"), Value::Null).await;
    assert_eq!(folders["items"][0]["name"], "本社");
    let (_, groups) = call(
        &app,
        "GET",
        &format!("{base}/groups?q=%E5%96%B6%E6%A5%AD"),
        Value::Null,
    )
    .await;
    assert_eq!(groups["items"][0]["id"], source_id);
    let (status, plan) = call(
        &app,
        "POST",
        &format!("{base}/auto-plan"),
        json!({"group_ids":[source_id],"hierarchy":"ou"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["changes"]["roles"].as_array().unwrap().len(), 3);
    let changes = plan["changes"].clone();
    let (status, op) = call(&app, "POST", &format!("{base}/apply"), changes.clone()).await;
    assert_eq!(status, StatusCode::OK, "{op}");
    let op: DirectoryOperation = serde_json::from_value(op).unwrap();
    api::directory_admin::process(&state, &ctx, op.id)
        .await
        .unwrap();
    let (status, replay) = call(&app, "POST", &format!("{base}/apply"), changes.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["state"], "succeeded");
    let (_, progress) = call(
        &app,
        "GET",
        &format!("{base}/operations/{}", op.id),
        Value::Null,
    )
    .await;
    assert_eq!(progress["processed"], 4);
    let (_, plan) = call(
        &app,
        "POST",
        &format!("{base}/auto-plan"),
        json!({"group_ids":[source_id],"hierarchy":"ou"}),
    )
    .await;
    assert!(plan["changes"]["roles"].as_array().unwrap().is_empty());
    assert!(plan["changes"]["mappings"].as_array().unwrap().is_empty());
    let (_, roles) = call(
        &app,
        "GET",
        &format!("{base}/roles?q=%E5%96%B6%E6%A5%AD%E3%82%B0%E3%83%AB%E3%83%BC%E3%83%97"),
        Value::Null,
    )
    .await;
    let mut role = roles["items"][0].clone();
    let role_id = role["id"].as_str().unwrap().to_string();
    role["label"] = json!("営業チーム");
    role["expected_version"] = role["version"].clone();
    let rename = json!({"idempotency_key":uuid::Uuid::new_v4(),"roles":[role],"mappings":[]});
    let (status, preview) = call(&app, "POST", &format!("{base}/preview"), rename.clone()).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["before_roles"][0]["label"], "営業グループ");
    let (_, op) = call(&app, "POST", &format!("{base}/apply"), rename).await;
    api::directory_admin::process(&state, &ctx, op["id"].as_str().unwrap().parse().unwrap())
        .await
        .unwrap();
    let (_, roles) = call(
        &app,
        "GET",
        &format!("{base}/roles?ids={role_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(roles["items"][0]["label"], "営業チーム");
    let (status, plan) = call(
        &app,
        "POST",
        &format!("{base}/import-plan"),
        json!({"csv":format!("group_id,role_id\n{source_id},\n")}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert!(plan["changes"]["mappings"][0]["role_ids"]
        .as_array()
        .unwrap()
        .is_empty());
    let (_, history) = call(&app, "GET", &format!("{base}/operations"), Value::Null).await;
    assert_eq!(history.as_array().unwrap().len(), 3);
    let mut first_member = String::new();
    for index in 0..41 {
        let id = format!("member-{}", uuid::Uuid::new_v4());
        if index == 0 {
            first_member = id.clone();
        }
        state
            .directory
            .upsert_user(
                &id,
                ctx.tenant_id(),
                ctx.org(),
                &format!("member{index}@example.invalid"),
                &format!("Member {index}"),
            )
            .await
            .unwrap();
    }
    let mut direct = vec![(role_id.clone(), true)];
    direct.extend((0..20).map(|i| (format!("zz-member-role-{i}"), false)));
    state
        .directory
        .replace_user_roles(ctx.tenant_id(), ctx.org(), &first_member, &direct)
        .await
        .unwrap();
    let (status, members) = call(
        &app,
        "GET",
        &format!("{base}/members?q=Member"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{members}");
    assert_eq!(members["items"].as_array().unwrap().len(), 40);
    let next = members["next_cursor"].as_str().unwrap();
    let (_, last) = call(
        &app,
        "GET",
        &format!("{base}/members?q=Member&cursor={next}"),
        Value::Null,
    )
    .await;
    assert_eq!(last["items"].as_array().unwrap().len(), 1);
    let (_, before_failed_save) = call(&app, "GET", &format!("{base}/groups"), Value::Null).await;
    let mut invalid_connection = input.clone();
    invalid_connection["preserve_group_inheritance"] = json!(true);
    invalid_connection["bind_password"] = json!("**********");
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("{base}/connection"),
            invalid_connection
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let (_, after_failed_save) = call(&app, "GET", &format!("{base}/groups"), Value::Null).await;
    assert_eq!(before_failed_save, after_failed_save);
    let mut changed_connection = input.clone();
    changed_connection["preserve_group_inheritance"] = json!(true);
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("{base}/connection"),
            changed_connection
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, stale_plan) = call(
        &app,
        "POST",
        &format!("{base}/auto-plan"),
        json!({"hierarchy":"group"}),
    )
    .await;
    assert_eq!(stale_plan["group_count"], 0);
    let (_, inactive) = call(&app, "GET", &format!("{base}/groups"), Value::Null).await;
    assert!(inactive["items"].as_array().unwrap().is_empty());
    let locked = fixture.lock().unwrap();
    assert_eq!(
        locked
            .components
            .iter()
            .filter(|c| c["providerId"] == "ldap")
            .count(),
        1
    );
    let provider = locked
        .components
        .iter()
        .find(|c| c["providerId"] == "ldap")
        .unwrap();
    assert_eq!(
        provider["config"]["bindCredential"][0],
        "sensitive-bind-secret"
    );
    assert_eq!(provider["config"]["editMode"][0], "READ_ONLY");
    let writes_to_roles = locked
        .writes
        .iter()
        .filter(|w| w["method"] == "PUT" && w["path"].as_str().unwrap().contains("/groups/"))
        .count();
    assert_eq!(writes_to_roles, 4);
    let _typed: DirectoryChangeSet = serde_json::from_value(changes).unwrap();
}
#[tokio::test]
async fn directory_http_fails_closed_and_retry_is_persistent() {
    let Some((state, ctx, fixture)) = fixture().await else {
        return;
    };
    let app = build_router(state.clone());
    let base = "/org-admin/directory";
    let prefix = storage::directory::admin::role_id_prefix(&ctx);
    let connection = json!({"connection_url":"ldaps://ad.example.test:636","users_dn":"OU=Users,DC=example","groups_dn":"OU=Groups,DC=example","bind_dn":"CN=reader,DC=example","bind_password":"test-bind-secret"});
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("{base}/connection"),
            connection.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    let source: storage::directory::admin::AdGroup = serde_json::from_value(json!({"id":"retry-source","name":"Retry source","claim_path":format!("/tenants/{}/{}/ad-guid-v1/Retry",ctx.tenant_id(),ctx.org()),"distinguished_name":"CN=Retry,OU=Groups,DC=example","ou_path":[],"parent_ids":[],"role_ids":[],"mapping_version":0})).unwrap();
    state
        .directory
        .admin()
        .cache_group(&ctx, &source)
        .await
        .unwrap();
    let request = json!({"idempotency_key":uuid::Uuid::new_v4(),"roles":[{"id":format!("{prefix}test"),"label":"Test","parent_id":null,"expected_version":0}],"mappings":[{"group_id":"retry-source","role_ids":[format!("{prefix}test")],"expected_version":0}]});
    let (status, op) = call(&app, "POST", &format!("{base}/apply"), request).await;
    assert_eq!(status, StatusCode::OK);
    let id: uuid::Uuid = op["id"].as_str().unwrap().parse().unwrap();
    fixture.lock().unwrap().fail = true;
    assert!(api::directory_admin::process(&state, &ctx, id)
        .await
        .is_err());
    state
        .directory
        .admin()
        .fail_operation(&ctx, id, "synchronization_failed")
        .await
        .unwrap();
    fixture.lock().unwrap().fail = false;
    let (_, before_config_change) = call(&app, "GET", &format!("{base}/groups"), Value::Null).await;
    let mut changed_scope = connection.clone();
    changed_scope["groups_dn"] = json!("OU=OtherGroups,DC=example");
    let (status, blocked) = call(&app, "PUT", &format!("{base}/connection"), changed_scope).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(blocked["message"]
        .as_str()
        .unwrap()
        .contains("停止中の反映"));
    let (_, after_config_change) = call(&app, "GET", &format!("{base}/groups"), Value::Null).await;
    assert_eq!(before_config_change, after_config_change);
    let mut repaired = connection;
    repaired["bind_dn"] = json!("CN=repaired-reader,DC=example");
    repaired["bind_password"] = json!("repaired-test-secret");
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), repaired)
            .await
            .0,
        StatusCode::OK
    );
    let (_, op) = call(
        &app,
        "POST",
        &format!("{base}/operations/{id}/retry"),
        json!({"recovery_confirmed":true}),
    )
    .await;
    assert_eq!(op["state"], "pending");
    fixture.lock().unwrap().fail = false;
    api::directory_admin::process(&state, &ctx, id)
        .await
        .unwrap();
    assert_eq!(
        state
            .directory
            .admin()
            .operation(&ctx, id)
            .await
            .unwrap()
            .state,
        "succeeded"
    );
    assert_eq!(
        state
            .directory
            .admin()
            .groups(&ctx, "", None, 40, &[])
            .await
            .unwrap()
            .items[0]
            .role_ids,
        vec![format!("{prefix}test")]
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{base}/roles?ids={}", vec!["x"; 101].join(",")),
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut denied = state.clone();
    denied.authz = Arc::new(AdminGate {
        admin: false,
        auditor: false,
    });
    let denied = build_router(denied);
    for path in [
        "",
        "/roles",
        "/groups",
        "/folders",
        "/operations",
        "/connection",
    ] {
        assert_eq!(
            call(&denied, "GET", &format!("{base}{path}"), Value::Null)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let csrf = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{base}/apply"))
                .header(COOKIE, "shiki_session=sid-valid.default")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(csrf.status(), StatusCode::FORBIDDEN);
    let disallowed =
        json!({"connection_url":"ldaps://127.0.0.1","users_dn":"x","groups_dn":"x","bind_dn":"x"});
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), disallowed)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/auto-plan"),
            json!({"hierarchy":"group","group_ids":null})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn failed_initial_sync_can_be_repaired_by_configuring_ad() {
    let Some((state, ctx, _fixture)) = fixture().await else {
        return;
    };
    let app = build_router(state.clone());
    let base = "/org-admin/directory";
    let (status, op) = call(
        &app,
        "POST",
        &format!("{base}/sync"),
        json!({"idempotency_key":uuid::Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = op["id"].as_str().unwrap().parse().unwrap();
    assert!(api::directory_admin::process(&state, &ctx, id)
        .await
        .is_err());
    state
        .directory
        .admin()
        .fail_operation(&ctx, id, "synchronization_failed")
        .await
        .unwrap();
    let input = json!({"connection_url":"ldaps://ad.example.test:636","users_dn":"OU=Users,DC=example","groups_dn":"OU=Groups,DC=example","bind_dn":"CN=reader,DC=example","bind_password":"repair-test-secret"});
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), input)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/operations/{id}/retry"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    api::directory_admin::process(&state, &ctx, id)
        .await
        .unwrap();
    assert_eq!(
        state
            .directory
            .admin()
            .operation(&ctx, id)
            .await
            .unwrap()
            .state,
        "succeeded"
    );
}

#[tokio::test]
async fn interrupted_running_operation_requires_confirmed_manual_recovery() {
    let Some((state, ctx, fixture)) = fixture().await else {
        return;
    };
    let app = build_router(state.clone());
    let base = "/org-admin/directory";
    let prefix = storage::directory::admin::role_id_prefix(&ctx);
    let request = json!({"idempotency_key":uuid::Uuid::new_v4(),"roles":[{"id":format!("{prefix}interrupted"),"label":"Interrupted","parent_id":null,"expected_version":0}],"mappings":[]});
    let (_, op) = call(&app, "POST", &format!("{base}/apply"), request).await;
    let id = op["id"].as_str().unwrap().parse().unwrap();
    state
        .directory
        .admin()
        .start_operation(&ctx, id)
        .await
        .unwrap()
        .unwrap();
    api::directory_admin::process(&state, &ctx, id)
        .await
        .unwrap();
    let stopped = state.directory.admin().operation(&ctx, id).await.unwrap();
    assert_eq!(stopped.state, "failed");
    assert_eq!(stopped.error_code.as_deref(), Some("execution_interrupted"));
    assert!(fixture.lock().unwrap().writes.is_empty());
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/operations/{id}/retry"),
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/operations/{id}/retry"),
            json!({"recovery_confirmed":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    api::directory_admin::process(&state, &ctx, id)
        .await
        .unwrap();
    let finished = state.directory.admin().operation(&ctx, id).await.unwrap();
    assert_eq!(finished.state, "succeeded");
    assert_eq!(finished.attempts, 2);
}

#[tokio::test]
async fn preview_batches_more_than_twenty_thousand_candidate_ids() {
    let Some((state, ctx, _fixture)) = fixture().await else {
        return;
    };
    let owner = PgPoolOptions::new()
        .max_connections(2)
        .connect(&shiki_testenv::var("STORAGE_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let prefix = storage::directory::admin::role_id_prefix(&ctx);
    let mut tx = owner.begin().await.unwrap();
    sqlx::query("INSERT INTO directory_role (tenant_id,org,role_id,display_name) SELECT $1,$2,$3 || 'p-' || lpad(n::text,5,'0'),'Parent ' || n FROM generate_series(0,10000) n").bind(ctx.tenant_id()).bind(ctx.org()).bind(&prefix).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO directory_managed_role (tenant_id,org,role_id,label,keycloak_group_id) SELECT tenant_id,org,role_id,display_name,'fixture-group' FROM directory_role WHERE tenant_id=$1 AND org=$2").bind(ctx.tenant_id()).bind(ctx.org()).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let changes = DirectoryChangeSet {
        idempotency_key: uuid::Uuid::new_v4(),
        roles: (0..10001)
            .map(|n| storage::directory::admin::RoleEdit {
                id: format!("{prefix}c-{n:05}"),
                label: "Child".into(),
                parent_id: Some(format!("{prefix}p-{n:05}")),
                expected_version: 0,
            })
            .collect(),
        mappings: vec![],
    };
    let plan = api::directory_admin::preview::create(&state, &ctx, &changes)
        .await
        .unwrap();
    assert_eq!(plan.nodes.len(), 20002);
    assert_eq!(plan.changes.roles.len(), 10001);
    assert!(plan.before_roles.is_empty());
}

#[tokio::test]
async fn unknown_role_write_result_requires_confirmation_even_with_live_lease() {
    let Some((state, ctx, fixture)) = fixture().await else {
        return;
    };
    let app = build_router(state.clone());
    let base = "/org-admin/directory";
    let prefix = storage::directory::admin::role_id_prefix(&ctx);
    let request = json!({"idempotency_key":uuid::Uuid::new_v4(),"roles":[{"id":format!("{prefix}uncertain"),"label":"Uncertain","parent_id":null,"expected_version":0}],"mappings":[]});
    let (status, op) = call(&app, "POST", &format!("{base}/apply"), request).await;
    assert_eq!(status, StatusCode::OK);
    let id = op["id"].as_str().unwrap().parse().unwrap();
    fixture.lock().unwrap().fail_role_write = true;
    assert!(api::directory_admin::process(&state, &ctx, id)
        .await
        .is_err());
    let stopped = state.directory.admin().operation(&ctx, id).await.unwrap();
    assert_eq!(stopped.error_code.as_deref(), Some("execution_interrupted"));
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/operations/{id}/retry"),
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    fixture.lock().unwrap().fail_role_write = false;
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/operations/{id}/retry"),
            json!({"recovery_confirmed":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    api::directory_admin::process(&state, &ctx, id)
        .await
        .unwrap();
    assert_eq!(
        state
            .directory
            .admin()
            .operation(&ctx, id)
            .await
            .unwrap()
            .state,
        "succeeded"
    );
}

#[tokio::test]
async fn uncertain_connection_blocks_following_writes_until_confirmed_repair() {
    let Some((state, ctx, fixture)) = fixture().await else {
        return;
    };
    let app = build_router(state.clone());
    let base = "/org-admin/directory";
    let mut input = json!({"connection_url":"ldaps://ad.example.test:636","users_dn":"OU=Users,DC=example","groups_dn":"OU=Groups,DC=example","bind_dn":"CN=reader,DC=example","bind_password":"repair-test-secret"});
    fixture.lock().unwrap().fail_component_write = true;
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), input.clone())
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(state
        .directory
        .admin()
        .connection_recovery_required(&ctx)
        .await
        .unwrap());
    let (status, connection) = call(&app, "GET", &format!("{base}/connection"), Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(connection["recovery_required"], true);
    let writes = fixture.lock().unwrap().writes.len();
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), input.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(fixture.lock().unwrap().writes.len(), writes);
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/sync"),
            json!({"idempotency_key":uuid::Uuid::new_v4()})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    fixture.lock().unwrap().fail_component_write = false;
    input["recovery_confirmed"] = json!(true);
    assert_eq!(
        call(&app, "PUT", &format!("{base}/connection"), input)
            .await
            .0,
        StatusCode::OK
    );
    assert!(!state
        .directory
        .admin()
        .connection_recovery_required(&ctx)
        .await
        .unwrap());
    let (status, op) = call(
        &app,
        "POST",
        &format!("{base}/sync"),
        json!({"idempotency_key":uuid::Uuid::new_v4()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = op["id"].as_str().unwrap().parse().unwrap();
    api::directory_admin::process(&state, &ctx, id)
        .await
        .unwrap();
}

#[derive(Default)]
struct ClaimsAuthz(
    std::sync::Mutex<std::collections::BTreeMap<String, std::collections::BTreeSet<String>>>,
);
#[async_trait]
impl AuthzClient for ClaimsAuthz {
    async fn check(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object: &FgaObject,
        _consistency: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }

    async fn write_tuple(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        if relation == Relation::Member
            && subject.to_string().starts_with("user:")
            && object.to_string().starts_with("role:")
        {
            self.0
                .lock()
                .unwrap()
                .entry(subject.to_string())
                .or_default()
                .insert(object.to_string());
        }
        Ok(true)
    }

    async fn delete_tuple(
        &self,
        subject: &Subject,
        relation: Relation,
        object: &FgaObject,
    ) -> Result<bool, AuthzError> {
        if relation == Relation::Member {
            if let Some(roles) = self.0.lock().unwrap().get_mut(&subject.to_string()) {
                roles.remove(&object.to_string());
            }
        }
        Ok(true)
    }

    async fn read_tuples(
        &self,
        _object: &FgaObject,
        _relation: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }

    async fn list_objects(
        &self,
        _subject: &Subject,
        _relation: Relation,
        _object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }

    async fn delete_object_tuples(&self, _object: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }

    async fn read_subject_objects(
        &self,
        subject: &Subject,
        _object_type: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(&subject.to_string())
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default())
    }
}

#[tokio::test]
async fn directory_mapping_follows_authenticated_claims_and_revokes_on_refresh() {
    let Some((mut state, ctx, _fixture)) = fixture().await else {
        return;
    };
    let probe = Arc::new(ClaimsAuthz::default());
    state.authz = probe.clone();
    let source = storage::directory::admin::AdGroup {
        id: "claims-group".into(),
        name: "Claims group".into(),
        claim_path: format!("/tenants/default/{}/ad-guid-v1/claims-group", ctx.org()),
        distinguished_name: Some("CN=Claims,OU=Groups,DC=example".into()),
        ou_path: vec!["Groups".into()],
        parent_ids: vec![],
        role_ids: vec![],
        mapping_version: 0,
    };
    state
        .directory
        .admin()
        .cache_group(&ctx, &source)
        .await
        .unwrap();
    let role_id = format!(
        "{}claims-role",
        storage::directory::admin::role_id_prefix(&ctx)
    );
    let change = serde_json::from_value(json!({"idempotency_key":uuid::Uuid::new_v4(),"roles":[{"id":role_id,"label":"Mapped role","parent_id":null,"expected_version":0}],"mappings":[{"group_id":source.id,"role_ids":[role_id],"expected_version":0}]})).unwrap();
    let operation = api::directory_admin::submit(&state, &ctx, &change, None)
        .await
        .unwrap();
    api::directory_admin::process(&state, &ctx, operation.id)
        .await
        .unwrap();
    for (include_source, keep_mapping) in [(true, true), (false, true), (true, false)] {
        if !keep_mapping {
            let clear = serde_json::from_value(json!({"idempotency_key":uuid::Uuid::new_v4(),"roles":[],"mappings":[{"group_id":source.id,"role_ids":[],"expected_version":1}]})).unwrap();
            let op = api::directory_admin::submit(&state, &ctx, &clear, None)
                .await
                .unwrap();
            api::directory_admin::process(&state, &ctx, op.id)
                .await
                .unwrap();
        }
        let mut groups = vec![format!("/tenants/default/{}", ctx.org())];
        if include_source {
            groups.push(source.claim_path.clone());
        }
        let access =
            fake_jwt(json!({"sub":ctx.principal().id,"groups":groups,"roles":["managed-support"]}));
        let token_base = spawn_token_server(
            StatusCode::OK,
            json!({"access_token":access,"expires_in":3600}),
        )
        .await;
        Arc::make_mut(&mut state.config).auth.internal_base_url = Some(token_base);
        let mut record = session_record(now() - 10, Some("refresh-token"), "csrf");
        record.principal = ctx.principal().clone();
        state
            .sessions
            .put("default", "sid-valid", &record, Duration::from_secs(3600))
            .await
            .unwrap();
        let (status, body) = call(&build_router(state.clone()), "GET", "/me", Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let expected = ctx.ns().role(&role_id).to_string();
        let legacy = ctx.ns().role("managed-support").to_string();
        let mut observed = false;
        for _ in 0..100 {
            let roles = probe
                .read_subject_objects(&ctx.subject(), ObjectType::Role)
                .await
                .unwrap();
            if roles.contains(&expected) == (include_source && keep_mapping)
                && roles.contains(&legacy)
            {
                observed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            observed,
            "AD mapping membership did not converge: include={include_source}"
        );
    }
}
