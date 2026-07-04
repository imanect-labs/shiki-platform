//! Keycloak admin REST クライアントの結合テスト（in-process モックサーバ・外部依存なし）。
//!
//! auth_flow.rs と同じ手法で axum のモック Keycloak を立て、`KeycloakAdmin` の
//! 全経路（group/user の冪等作成・検索・削除・409/404 の冪等化）を検証する。

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use api::{
    config::{AuthConfig, Tenancy},
    keycloak_admin::{KeycloakAdmin, KeycloakAdminError},
};
use axum::{
    extract::{Path, Query},
    http::StatusCode,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde_json::{json, Value};

/// モック Keycloak の共有状態（作成済み group/user を記憶して冪等系を再現する）。
/// user は POST された payload（attributes 込み）を保持し、検索で返す。
#[derive(Default)]
struct KcState {
    group_exists: AtomicBool,
    created_user: Mutex<Option<Value>>,
    /// 初回ログインで UPDATE_PASSWORD を消化済みか（true なら username 検索で
    /// requiredActions を空にして返す＝ログイン済みユーザーを模す）。
    logged_in: AtomicBool,
}

/// token（client_credentials）＋ admin REST（groups/users）のモック Keycloak を立てる。
async fn spawn_mock_kc(state: Arc<KcState>) -> String {
    let token_route =
        post(|| async { Json(json!({ "access_token": "mock-admin-token", "expires_in": 60 })) });

    let groups_post = {
        let st = state.clone();
        post(move || {
            let st = st.clone();
            async move {
                // 2 回目以降は 409（既存）→ クライアントは検索へフォールバックする。
                if st.group_exists.swap(true, Ordering::SeqCst) {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::CREATED
                }
            }
        })
    };
    let groups_get = get(|Query(q): Query<Value>| async move {
        let name = q.get("search").and_then(Value::as_str).unwrap_or("");
        Json(json!([{ "id": "group-1", "name": name }]))
    });

    let users_post = {
        let st = state.clone();
        post(move |Json(body): Json<Value>| {
            let st = st.clone();
            async move {
                let mut created = st.created_user.lock().unwrap();
                if created.is_some() {
                    StatusCode::CONFLICT
                } else {
                    *created = Some(body);
                    StatusCode::CREATED
                }
            }
        })
    };
    let users_get = {
        let st = state.clone();
        get(move |Query(q): Query<Value>| {
            let st = st.clone();
            async move {
                // username 検索: 作成済みならその payload（attributes 込み）を返す。
                // tenant 属性検索（q=tenant:X）: 1 ページ目に 1 件・2 ページ目は空（終端）。
                if let Some(username) = q.get("username").and_then(Value::as_str) {
                    let created = st.created_user.lock().unwrap().clone();
                    if let Some(user) = created {
                        // 初回ログイン前は POST payload の requiredActions（UPDATE_PASSWORD）を
                        // そのまま返す。logged_in が立てば消化済みとして空で返す。
                        let required_actions = if st.logged_in.load(Ordering::SeqCst) {
                            json!([])
                        } else {
                            user.get("requiredActions").cloned().unwrap_or(json!([]))
                        };
                        return Json(json!([{
                            "id": "user-1",
                            "username": username,
                            "attributes": user.get("attributes").cloned().unwrap_or(json!({})),
                            "requiredActions": required_actions,
                        }]));
                    }
                    return Json(json!([]));
                }
                let first = q
                    .get("first")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                if first == 0 {
                    Json(json!([{ "id": "user-1", "username": "tenant-admin" }]))
                } else {
                    Json(json!([]))
                }
            }
        })
    };
    let user_delete = delete(|Path(id): Path<String>| async move {
        // user-1 は削除成功、それ以外は 404（冪等に成功へ倒されることを検証）。
        if id == "user-1" {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::NOT_FOUND
        }
    });
    let group_delete = delete(|Path(_id): Path<String>| async move { StatusCode::NO_CONTENT });
    // 一時パスワードの再設定（#91 M-6）。204 を返すだけの冪等スタブ。
    let reset_password = put(|| async { StatusCode::NO_CONTENT });

    let app = Router::new()
        .route("/realms/shiki/protocol/openid-connect/token", token_route)
        .route("/admin/realms/shiki/groups", groups_post.merge(groups_get))
        .route("/admin/realms/shiki/groups/{id}", group_delete)
        .route("/admin/realms/shiki/users", users_post.merge(users_get))
        .route("/admin/realms/shiki/users/{id}", user_delete)
        .route(
            "/admin/realms/shiki/users/{id}/reset-password",
            reset_password,
        )
        .with_state(());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn auth_config(base: &str) -> AuthConfig {
    AuthConfig {
        issuer: format!("{base}/realms/shiki"),
        internal_base_url: Some(format!("{base}/realms/shiki")),
        jwks_uri: None,
        audience: "shiki-api".into(),
        jwks_ttl_secs: 300,
        client_id: "shiki-web".into(),
        client_secret: None,
        redirect_uri: "http://localhost:3000/auth/callback".into(),
        post_logout_redirect_uri: "http://localhost:3000/".into(),
        scopes: "openid profile".into(),
        tenancy: Tenancy::Single,
        tenant_id: Some("default".into()),
        provisioner_client_id: Some("shiki-provisioner".into()),
        provisioner_client_secret: Some("dev-secret".into()),
        admin_base_url: None,
    }
}

#[tokio::test]
async fn from_config_requires_provisioner_credentials() {
    let base = spawn_mock_kc(Arc::default()).await;
    let http = reqwest::Client::new();
    let mut auth = auth_config(&base);
    auth.provisioner_client_id = None;
    let err = KeycloakAdmin::from_config(&http, &auth).err().unwrap();
    assert!(matches!(err, KeycloakAdminError::NotConfigured(_)));
}

#[tokio::test]
async fn ensure_group_is_idempotent() {
    let base = spawn_mock_kc(Arc::default()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();
    // 1 回目: 201 → 検索で id 解決。2 回目: 409 → 冪等に同じ id。
    assert_eq!(kc.ensure_group("acme").await.unwrap(), "group-1");
    assert_eq!(kc.ensure_group("acme").await.unwrap(), "group-1");
}

#[tokio::test]
async fn ensure_tenant_admin_reissues_before_first_login() {
    // #91 M-6: 初回ログイン前（UPDATE_PASSWORD 未消化）の既存 admin は再実行で一時
    // パスワードを再発行する（プロビジョニング後段の失敗で初回応答が破棄されても回収可）。
    // ログイン済みになったらパスワードには触れない。
    let state = Arc::new(KcState::default());
    let base = spawn_mock_kc(state.clone()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();

    // 新規作成: user id と一時パスワードが返る。
    let (id, password) = kc
        .ensure_tenant_admin(
            "acme",
            "acme",
            "admin@acme.example",
            "admin@acme.example",
            "tmp-pass",
        )
        .await
        .unwrap();
    assert_eq!(id, "user-1");
    assert_eq!(password.as_deref(), Some("tmp-pass"));

    // 既存かつログイン前: 一時パスワードを再発行して返す（M-6）。
    let (id2, password2) = kc
        .ensure_tenant_admin(
            "acme",
            "acme",
            "admin@acme.example",
            "admin@acme.example",
            "reissued",
        )
        .await
        .unwrap();
    assert_eq!(id2, "user-1");
    assert_eq!(password2.as_deref(), Some("reissued"));

    // ログイン済み（UPDATE_PASSWORD 消化）になったらパスワードには触れない。
    state.logged_in.store(true, Ordering::SeqCst);
    let (id3, password3) = kc
        .ensure_tenant_admin(
            "acme",
            "acme",
            "admin@acme.example",
            "admin@acme.example",
            "ignored",
        )
        .await
        .unwrap();
    assert_eq!(id3, "user-1");
    assert_eq!(password3, None);
}

#[tokio::test]
async fn ensure_tenant_admin_rejects_cross_tenant_username() {
    // username が**別テナント**の既存ユーザーと衝突したら 409（乗っ取り防止）。
    let base = spawn_mock_kc(Arc::default()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();
    kc.ensure_tenant_admin("acme", "acme", "admin@x.example", "admin@x.example", "p1")
        .await
        .unwrap();
    let err = kc
        .ensure_tenant_admin(
            "other-corp",
            "other-corp",
            "admin@x.example",
            "admin@x.example",
            "p2",
        )
        .await
        .err()
        .unwrap();
    assert!(
        matches!(err, KeycloakAdminError::Status { status: 409, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn find_users_by_tenant_paginates() {
    let base = spawn_mock_kc(Arc::default()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();
    let users = kc.find_users_by_tenant("acme").await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].id, "user-1");
}

#[tokio::test]
async fn deletes_are_idempotent() {
    let base = spawn_mock_kc(Arc::default()).await;
    let http = reqwest::Client::new();
    let auth = auth_config(&base);
    let kc = KeycloakAdmin::from_config(&http, &auth).unwrap();
    // 存在するユーザー削除は成功、不在（404）も成功に倒れる。
    kc.delete_user("user-1").await.unwrap();
    kc.delete_user("missing-user").await.unwrap();
    // group 削除（検索ヒット→削除）と、不在 group の削除（検索ヒットでも mock は常に返すため
    // ここでは成功経路のみ）。
    kc.delete_group_by_name("acme").await.unwrap();
}

#[tokio::test]
async fn admin_base_derivation_failure_is_not_configured() {
    let http = reqwest::Client::new();
    let mut auth = auth_config("http://kc.example");
    // realm セグメントの無い internal_base → admin_base 導出不能 → NotConfigured。
    auth.internal_base_url = Some("http://kc.example/oauth".into());
    auth.admin_base_url = None;
    let err = KeycloakAdmin::from_config(&http, &auth).err().unwrap();
    assert!(matches!(err, KeycloakAdminError::NotConfigured(_)));
}
