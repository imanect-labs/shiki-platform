//! ルータ構築。**全エンドポイントは [`route_table`] の宣言的マップからのみ登録される**
//! （「エンドポイント→必要スコープ」の一律強制・architecture-invariants / #91 M-1）。
//! 表に載せずにルートを増やすことはできず、非 Public エントリは OpenAPI 仕様との
//! 整合テスト（`route_table_matches_openapi`）で宣言漏れを検出する。

use std::time::Duration;

use axum::{
    extract::Request,
    http::{header, HeaderName, HeaderValue, Method, StatusCode},
    middleware,
    response::IntoResponse,
    routing::{delete, get, patch, post, put, MethodRouter},
    Router,
};
use tower_http::{cors::CorsLayer, timeout::TimeoutLayer, trace::TraceLayer};

use crate::{health, middleware::require_session, openapi, routes, state::AppState, telemetry};

/// エンドポイントのアクセスポリシー（必要スコープの宣言）。
///
/// ハンドラ個別の認証チェックを禁じ、ポリシー種別ごとに単一の middleware を
/// 一律適用するための閉じた語彙。データアクセスの認可（OpenFGA check）は
/// この下の `AuthContext` ＋ `StorageService` チョークポイントが担う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessPolicy {
    /// 認証不要（ヘルス・BFF 認証エンドポイント・OpenAPI 配布）。標準 30s タイムアウト。
    Public,
    /// BFF セッション必須（標準 30s タイムアウト）。
    Session,
    /// BFF セッション必須・長時間許容（300s。finalize のサーバ側ハッシュ/コピー等）。
    SessionLongRunning,
    /// provisioner service account の Bearer JWT 必須（admin プレーン・300s）。
    /// config（provisioner 資格情報＋admin base）が無ければルートごと不在（fail-closed）。
    Provisioner,
}

/// 1 エンドポイントの宣言（パス・メソッド・ポリシー・ハンドラ登録）。
pub struct RouteDecl {
    pub path: &'static str,
    /// 宣言メソッド（OpenAPI 整合テストで実体と突合する）。
    pub methods: &'static [&'static str],
    pub policy: AccessPolicy,
    handler: fn() -> MethodRouter<AppState>,
}

/// 全エンドポイントの単一定義（宣言的スコープマップ）。
///
/// ルータは本表からのみ構築されるため、「表に無いエンドポイント」は存在できない。
/// 追加時はポリシーの宣言が必須になり、認可レイヤの適用漏れが構造的に起きない。
pub fn route_table() -> Vec<RouteDecl> {
    use AccessPolicy::{Provisioner, Public, Session, SessionLongRunning};
    fn r(
        path: &'static str,
        methods: &'static [&'static str],
        policy: AccessPolicy,
        handler: fn() -> MethodRouter<AppState>,
    ) -> RouteDecl {
        RouteDecl {
            path,
            methods,
            policy,
            handler,
        }
    }
    vec![
        // --- Public（認証不要。/auth/* はセッション確立前に叩く。logout は内部で CSRF 自己検証） ---
        r("/healthz", &["GET"], Public, || get(health::healthz)),
        r("/readyz", &["GET"], Public, || get(health::readyz)),
        r("/auth/login", &["GET"], Public, || get(routes::auth::login)),
        r("/auth/callback", &["GET"], Public, || {
            get(routes::auth::callback)
        }),
        r("/auth/logout", &["POST"], Public, || {
            post(routes::auth::logout)
        }),
        r("/auth/session", &["GET"], Public, || {
            get(routes::auth::session)
        }),
        // OIDC Back-Channel Logout の受け口（Keycloak → RP・#91）。ブラウザ由来ではないため
        // Public だが、ハンドラ内で logout_token（署名/iss/aud/events/nonce）を検証する。
        r("/auth/backchannel-logout", &["POST"], Public, || {
            post(routes::auth::backchannel_logout)
        }),
        r("/api-docs/openapi.json", &["GET"], Public, || {
            get(openapi_handler)
        }),
        // --- Session（メタ操作・標準 30s） ---
        r("/me", &["GET"], Session, || get(routes::get_me)),
        r("/folders", &["POST"], Session, || {
            post(routes::folders::create_folder)
        }),
        r("/folders/{id}", &["PATCH", "DELETE"], Session, || {
            patch(routes::folders::update_folder).delete(routes::folders::delete_folder)
        }),
        r("/folders/{id}/restore", &["POST"], Session, || {
            post(routes::folders::restore_folder)
        }),
        r("/nodes", &["GET"], Session, || {
            get(routes::folders::list_children)
        }),
        r("/nodes/{id}/breadcrumb", &["GET"], Session, || {
            get(routes::folders::breadcrumb)
        }),
        r("/trash", &["GET"], Session, || {
            get(routes::folders::list_trash)
        }),
        r(
            "/nodes/{id}/shares",
            &["PUT", "DELETE", "GET"],
            Session,
            || {
                put(routes::shares::share_node)
                    .delete(routes::shares::unshare_node)
                    .get(routes::shares::list_shares)
            },
        ),
        r("/shares/shared-with-me", &["GET"], Session, || {
            get(routes::shares::shared_with_me)
        }),
        r("/directory/users", &["GET"], Session, || {
            get(routes::directory::search_users)
        }),
        r("/directory/roles", &["GET"], Session, || {
            get(routes::directory::search_roles)
        }),
        // --- SessionLongRunning（300s。finalize は staging のサーバ側ハッシュ＋コピーが
        //     大容量で 30s を超え、バイトは MinIO にあるのに file が作れない事故を防ぐ） ---
        r("/files", &["POST"], SessionLongRunning, || {
            post(routes::files::begin_upload)
        }),
        r(
            "/files/{id}",
            &["GET", "PATCH", "DELETE"],
            SessionLongRunning,
            || {
                get(routes::files::get_file)
                    .patch(routes::files::update_file)
                    .delete(routes::files::delete_file)
            },
        ),
        r(
            "/files/{upload_id}/finalize",
            &["POST"],
            SessionLongRunning,
            || post(routes::files::finalize_upload),
        ),
        r(
            "/files/{id}/download-url",
            &["GET"],
            SessionLongRunning,
            || get(routes::files::download_url),
        ),
        r("/files/{id}/restore", &["POST"], SessionLongRunning, || {
            post(routes::files::restore_file)
        }),
        r("/files/{id}/versions", &["GET"], SessionLongRunning, || {
            get(routes::files::list_versions)
        }),
        r(
            "/files/{id}/versions/{version}/download-url",
            &["GET"],
            SessionLongRunning,
            || get(routes::files::version_download_url),
        ),
        r(
            "/files/{id}/versions/{version}/restore",
            &["POST"],
            SessionLongRunning,
            || post(routes::files::restore_version),
        ),
        // --- Provisioner（admin プレーン・SAAS.2。削除は Keycloak/FGA/オブジェクト走査で 300s） ---
        r("/admin/tenants", &["POST"], Provisioner, || {
            post(routes::admin::create_tenant)
        }),
        r(
            "/admin/tenants/{tenant_id}",
            &["DELETE"],
            Provisioner,
            || delete(routes::admin::delete_tenant),
        ),
    ]
}

/// アプリの axum ルータを構築する（テストからも利用）。
///
/// [`route_table`] をポリシー種別ごとに束ね、認証 middleware とタイムアウトを
/// **グループ単位で一律適用**する（ハンドラ個別のチェックを持たない）。
pub fn build_router(state: AppState) -> Router {
    let session_layer = middleware::from_fn_with_state(state.clone(), require_session);
    let standard_timeout =
        || TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30));
    let long_timeout =
        || TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_mins(5));

    let mut public = Router::new();
    let mut session_std = Router::new();
    let mut session_long = Router::new();
    let mut admin = Router::new();
    // admin ルート（SAAS.2）: **config（provisioner 資格情報＋admin base）が揃っている時のみ
    // 組み込む**（未設定なら 404 = fail-closed）。
    let admin_enabled = state.config.auth.provisioner_credentials().is_some()
        && state.config.auth.admin_base().is_some();
    for decl in route_table() {
        let method_router = (decl.handler)();
        match decl.policy {
            AccessPolicy::Public => public = public.route(decl.path, method_router),
            AccessPolicy::Session => session_std = session_std.route(decl.path, method_router),
            AccessPolicy::SessionLongRunning => {
                session_long = session_long.route(decl.path, method_router);
            }
            AccessPolicy::Provisioner => {
                if admin_enabled {
                    admin = admin.route(decl.path, method_router);
                }
            }
        }
    }

    let public = public.layer(standard_timeout());
    let session_std = session_std
        .route_layer(session_layer.clone())
        .layer(standard_timeout());
    let session_long = session_long
        .route_layer(session_layer)
        .layer(long_timeout());
    let admin = if admin_enabled {
        admin
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                routes::admin::require_provisioner,
            ))
            .layer(long_timeout())
    } else {
        admin
    };

    let router = public
        .merge(session_std)
        .merge(session_long)
        .merge(admin)
        // observe は span 内で動く必要があるため TraceLayer より内側（先に追加）。
        .layer(middleware::from_fn(telemetry::observe))
        .layer(TraceLayer::new_for_http().make_span_with(make_request_span));

    // CORS: 同一オリジン配信が既定（レイヤ無し）。別オリジン dev のみ、設定された
    // オリジンに限定して credential 付きを許可する（permissive はセッション Cookie と
    // 併用すると危険なので使わない）。
    let router = match cors_layer(&state.config.server.cors_allowed_origins) {
        Some(cors) => router.layer(cors),
        None => router,
    };

    router.with_state(state)
}

/// 設定されたオリジンに限定した CORS レイヤを構築する（空なら `None` = レイヤ無効）。
fn cors_layer(origins: &[String]) -> Option<CorsLayer> {
    if origins.is_empty() {
        return None;
    }
    let parsed: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| o.parse::<HeaderValue>().ok())
        .collect();
    if parsed.is_empty() {
        tracing::warn!("cors_allowed_origins が全て不正なため CORS を無効化");
        return None;
    }
    Some(
        CorsLayer::new()
            .allow_origin(parsed)
            .allow_credentials(true)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                header::CONTENT_TYPE,
                HeaderName::from_static("x-csrf-token"),
            ]),
    )
}

/// リクエスト span。`trace_id` は [`telemetry::observe`] が後から記録するため Empty 宣言する。
fn make_request_span(req: &Request) -> tracing::Span {
    tracing::info_span!(
        "http_request",
        method = %req.method(),
        path = %req.uri().path(),
        trace_id = tracing::field::Empty,
    )
}

async fn openapi_handler() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        openapi::openapi_json(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// route_table の (path, METHOD) 集合。
    fn declared(policy_filter: impl Fn(AccessPolicy) -> bool) -> BTreeSet<(String, String)> {
        route_table()
            .iter()
            .filter(|d| policy_filter(d.policy))
            .flat_map(|d| {
                d.methods
                    .iter()
                    .map(|m| (d.path.to_string(), m.to_string()))
            })
            .collect()
    }

    /// OpenAPI 仕様の (path, METHOD) 集合。
    fn openapi_ops() -> BTreeSet<(String, String)> {
        let spec: serde_json::Value =
            serde_json::from_str(&openapi::openapi_json()).expect("OpenAPI JSON");
        let mut out = BTreeSet::new();
        let paths = spec["paths"].as_object().expect("paths object");
        for (path, ops) in paths {
            for method in ops.as_object().expect("ops object").keys() {
                out.insert((path.clone(), method.to_uppercase()));
            }
        }
        out
    }

    #[test]
    fn route_table_has_no_duplicate_ops() {
        // 同一 (path, method) の二重宣言はマージ時に panic するため表の段階で検出する。
        let mut seen = BTreeSet::new();
        for decl in route_table() {
            assert!(!decl.methods.is_empty(), "{}: methods が空", decl.path);
            for m in decl.methods {
                assert!(
                    seen.insert((decl.path, *m)),
                    "重複宣言: {} {}",
                    m,
                    decl.path
                );
            }
        }
    }

    /// 宣言的スコープマップと OpenAPI（codegen の正）の相互網羅（#91 M-1）。
    ///
    /// - OpenAPI に載る全操作は route_table に**非 Public** で宣言されていること
    ///   （API を増やすときポリシー宣言を強制する）。
    /// - 逆に、非 Public の全宣言は OpenAPI に載っていること
    ///   （utoipa 注釈＝TS 型生成の漏れを防ぐ）。
    #[test]
    fn route_table_matches_openapi() {
        let declared = declared(|p| p != AccessPolicy::Public);
        let spec = openapi_ops();
        let missing_policy: Vec<_> = spec.difference(&declared).collect();
        assert!(
            missing_policy.is_empty(),
            "OpenAPI にあるが route_table に非 Public 宣言が無い: {missing_policy:?}"
        );
        let missing_spec: Vec<_> = declared.difference(&spec).collect();
        assert!(
            missing_spec.is_empty(),
            "route_table にあるが OpenAPI（utoipa 注釈）に無い: {missing_spec:?}"
        );
    }

    #[test]
    fn admin_routes_are_provisioner_scoped() {
        // /admin/* は必ず Provisioner ポリシー（BFF セッションで到達できない）こと。
        for decl in route_table() {
            if decl.path.starts_with("/admin/") {
                assert_eq!(
                    decl.policy,
                    AccessPolicy::Provisioner,
                    "{} は Provisioner であること",
                    decl.path
                );
            } else {
                assert_ne!(
                    decl.policy,
                    AccessPolicy::Provisioner,
                    "{} が Provisioner になっている",
                    decl.path
                );
            }
        }
    }
}
