//! ルータ構築。**全エンドポイントは [`route_table`] の宣言的マップからのみ登録される**
//! （「エンドポイント→必要スコープ」の一律強制・architecture-invariants / #91 M-1）。
//! 表に載せずにルートを増やすことはできず、非 Public エントリは OpenAPI 仕様との
//! 整合テスト（`route_table_matches_openapi`）で宣言漏れを検出する。
//!
//! 唯一の例外は WOPI（`/wopi/**`・トークン認証の別面・Task 11.6）。cookie セッションを
//! 使わず WOPI 仕様の access_token クエリで認証するため表のポリシー語彙に載らず、
//! `office.enabled` 時のみ `crates/office` のルータを merge する。認証＋毎呼び出し
//! ReBAC はルータ内部の共通前段が一律に強制する（ハンドラ個別チェックではない）。

use axum::{
    http::header,
    response::IntoResponse,
    routing::{delete, get, patch, post, put, MethodRouter},
};

mod build;
mod http_util;
pub use build::build_router;

use crate::{health, openapi, routes, state::AppState};

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
    /// BFF セッション必須・**タイムアウト無し**（SSE ストリーミング。生成の逐次配信で
    /// 接続が長時間開いたままになるため 30s/300s のタイムアウトと衝突させない）。
    SessionStreaming,
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

impl RouteDecl {
    /// ルート宣言を組む（route_table と分離宣言ファイルの共通コンストラクタ）。
    pub(crate) fn new(
        path: &'static str,
        methods: &'static [&'static str],
        policy: AccessPolicy,
        handler: fn() -> MethodRouter<AppState>,
    ) -> Self {
        RouteDecl {
            path,
            methods,
            policy,
            handler,
        }
    }
}

/// 全エンドポイントの単一定義（宣言的スコープマップ）。
///
/// ルータは本表からのみ構築されるため、「表に無いエンドポイント」は存在できない。
/// 追加時はポリシーの宣言が必須になり、認可レイヤの適用漏れが構造的に起きない。
#[allow(clippy::too_many_lines)] // 全エンドポイントの宣言的マップ（分割すると一覧性を損なう）。
pub fn route_table() -> Vec<RouteDecl> {
    use AccessPolicy::{Provisioner, Public, Session, SessionLongRunning, SessionStreaming};
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
    let mut table = vec![
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
        // 共有リンク（#342）。発行/一覧/失効/延長は owner ゲート、redeem は認証のみ（失敗は一律 403）。
        r("/nodes/{id}/share-links", &["GET", "POST"], Session, || {
            get(routes::share_links::list_share_links).post(routes::share_links::create_share_link)
        }),
        r(
            "/share-links/{link_id}",
            &["DELETE", "PATCH"],
            Session,
            || {
                delete(routes::share_links::revoke_share_link)
                    .patch(routes::share_links::extend_share_link)
            },
        ),
        r("/share-links/redeem", &["POST"], Session, || {
            post(routes::share_links::redeem_share_link)
        }),
        r("/directory/users", &["GET"], Session, || {
            get(routes::directory::search_users)
        }),
        r("/directory/roles", &["GET"], Session, || {
            get(routes::directory::search_roles)
        }),
        // permission-aware 検索（Phase 2・二段 authz は rag::SearchService 内で一律強制）。
        r("/search", &["POST"], Session, || {
            post(routes::search::search)
        }),
        // --- アーティファクト共通枠（Task 6.1・バージョン付き共有本文） ---
        r("/artifacts", &["GET", "POST"], Session, || {
            get(routes::artifacts::list_artifacts).post(routes::artifacts::create_artifact)
        }),
        r("/artifacts/{id}", &["GET", "DELETE"], Session, || {
            get(routes::artifacts::get_artifact).delete(routes::artifacts::delete_artifact)
        }),
        r(
            "/artifacts/{id}/versions",
            &["GET", "POST"],
            Session,
            || get(routes::artifacts::list_versions).post(routes::artifacts::append_version),
        ),
        r(
            "/artifacts/{id}/versions/{version}",
            &["GET"],
            Session,
            || get(routes::artifacts::get_version),
        ),
        r(
            "/artifacts/{id}/shares",
            &["PUT", "DELETE", "GET"],
            Session,
            || {
                put(routes::artifacts::share_artifact)
                    .delete(routes::artifacts::unshare_artifact)
                    .get(routes::artifacts::list_artifact_shares)
            },
        ),
        // --- 構造化データ（Task 9.2/9.3/9.5）: 宣言は data_route_decls に分離 ---
        // --- generative UI（Phase 6・保存時検証つき ui_spec ＋ 宣言的アクション） ---
        r("/ui-specs", &["POST"], Session, || {
            post(routes::ui_specs::create_ui_spec)
        }),
        r("/ui-specs/{id}", &["GET", "PUT"], Session, || {
            get(routes::ui_specs::get_ui_spec).put(routes::ui_specs::update_ui_spec)
        }),
        r(
            "/ui-specs/{id}/versions/{version}",
            &["GET"],
            Session,
            || get(routes::ui_specs::get_ui_spec_version),
        ),
        r(
            "/threads/{thread_id}/messages/{message_id}/ui-actions",
            &["POST"],
            Session,
            || post(routes::ui_actions::invoke_chat_ui_action),
        ),
        // --- skill（Task 6.7・保存時検証つき。共有は /artifacts/{id}/shares を流用） ---
        r("/skills", &["POST"], Session, || {
            post(routes::skills::create_skill)
        }),
        r("/skills/{id}", &["GET", "PUT"], Session, || {
            get(routes::skills::get_skill).put(routes::skills::update_skill)
        }),
        r("/skills/{id}/versions/{version}", &["GET"], Session, || {
            get(routes::skills::get_skill_version)
        }),
        // --- ミニアプリ（Task 6.10・部品はバンドル権限で解決） ---
        r("/mini-apps", &["POST"], Session, || {
            post(routes::mini_apps::create_mini_app)
        }),
        r("/mini-apps/{id}", &["PUT"], Session, || {
            put(routes::mini_apps::update_mini_app)
        }),
        r("/mini-apps/{id}/resolved", &["GET"], Session, || {
            get(routes::mini_apps::resolve_mini_app)
        }),
        r("/mini-apps/{id}/ui-actions", &["POST"], Session, || {
            post(routes::mini_apps::invoke_mini_app_action)
        }),
        // --- ワークフロー IR（Task 10.1a・保存時検証 V1〜V7・artifact の上） ---
        r("/workflows", &["GET", "POST"], Session, || {
            get(routes::workflows::list_workflows).post(routes::workflows::create_workflow)
        }),
        r("/workflows/validate", &["POST"], Session, || {
            post(routes::workflows::validate_workflow)
        }),
        r("/workflows/{id}", &["GET", "PUT"], Session, || {
            get(routes::workflows::get_workflow).put(routes::workflows::update_workflow)
        }),
        r(
            "/workflows/{id}/versions/{version}",
            &["GET"],
            Session,
            || get(routes::workflows::get_workflow_version),
        ),
        // 対話トリガの run 起動＋実行履歴（Stage A W3）。
        r("/workflows/{id}/registration", &["GET"], Session, || {
            get(routes::workflows::get_registration)
        }),
        r(
            "/workflows/{id}/versions/{version}/consent-plan",
            &["GET"],
            Session,
            || get(routes::workflows::consent_plan),
        ),
        r("/workflows/{id}/enable", &["POST"], Session, || {
            post(routes::workflows::enable_workflow)
        }),
        r("/workflows/{id}/disable", &["POST"], Session, || {
            post(routes::workflows::disable_workflow)
        }),
        r("/workflows/{id}/layout", &["GET", "PUT"], Session, || {
            get(routes::workflows::get_workflow_layout).put(routes::workflows::put_workflow_layout)
        }),
        r("/workflows/{id}/runs", &["GET", "POST"], Session, || {
            get(routes::workflows::list_workflow_runs).post(routes::workflows::start_workflow_run)
        }),
        r("/workflows/{id}/runs/{run_id}", &["GET"], Session, || {
            get(routes::workflows::get_workflow_run)
        }),
        r(
            "/workflows/{id}/runs/{run_id}/steps",
            &["GET"],
            Session,
            || get(routes::workflows::get_workflow_step),
        ),
        r(
            "/workflows/{id}/runs/{run_id}/events",
            &["GET"],
            Session,
            || get(routes::workflows::list_workflow_run_events),
        ),
        r(
            "/workflows/{id}/runs/{run_id}/events/stream",
            &["GET"],
            SessionStreaming,
            || get(routes::workflows::stream_workflow_run_events),
        ),
        r(
            "/workflows/{id}/runs/{run_id}/cancel",
            &["POST"],
            Session,
            || post(routes::workflows::cancel_workflow_run),
        ),
        r(
            "/workflows/{id}/runs/{run_id}/retry",
            &["POST"],
            Session,
            || post(routes::workflows::retry_workflow_run),
        ),
        // --- シークレット（Task 10.9・write-only / use-only・平文の読み返しルートは無い） ---
        r("/secrets", &["GET", "POST"], Session, || {
            get(routes::secrets::list_secrets).post(routes::secrets::create_secret)
        }),
        r("/secrets/{id}", &["GET", "PUT", "DELETE"], Session, || {
            get(routes::secrets::get_secret)
                .put(routes::secrets::rotate_secret)
                .delete(routes::secrets::delete_secret)
        }),
        r("/secrets/{id}/binding", &["PATCH"], Session, || {
            patch(routes::secrets::update_binding)
        }),
        // --- チャット（Phase 3）。生成は接続非依存ジョブ（Task 3.11）で SSE は別ポリシ。 ---
        r("/threads", &["GET", "POST"], Session, || {
            get(routes::chat::list_threads).post(routes::chat::create_thread)
        }),
        r("/threads/{id}", &["GET", "PATCH"], Session, || {
            get(routes::chat::get_thread).patch(routes::chat_notes::set_thread_origin_note)
        }),
        r("/threads/{id}/messages", &["GET", "POST"], Session, || {
            get(routes::chat::get_messages).post(routes::chat::post_message)
        }),
        // SSE ストリーム（replay-then-subscribe）。長時間開くためタイムアウト無しの専用ポリシ。
        r("/threads/{id}/stream", &["GET"], SessionStreaming, || {
            get(routes::chat::stream_thread)
        }),
        r(
            "/threads/{id}/runs/{run_id}/cancel",
            &["POST"],
            Session,
            || post(routes::chat::cancel_run),
        ),
        r(
            "/threads/{id}/runs/{run_id}/approvals",
            &["POST"],
            Session,
            || post(routes::chat_approval::submit_approval),
        ),
        r(
            "/threads/{id}/shares",
            &["POST", "DELETE", "GET"],
            Session,
            || {
                post(routes::chat::share_thread)
                    .delete(routes::chat::unshare_thread)
                    .get(routes::chat::list_thread_shares)
            },
        ),
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
            get(routes::file_versions::list_versions)
        }),
        r(
            "/files/{id}/versions/{version}/download-url",
            &["GET"],
            SessionLongRunning,
            || get(routes::file_versions::version_download_url),
        ),
        r(
            "/files/{id}/versions/{version}/restore",
            &["POST"],
            SessionLongRunning,
            || post(routes::file_versions::restore_version),
        ),
        r(
            "/files/{id}/versions/{version}/adopt",
            &["POST"],
            SessionLongRunning,
            || post(routes::file_versions::adopt_version),
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
    ];
    table.extend(routes::collab::collab_route_decls());
    table.extend(routes::documents::documents_route_decls());
    table.extend(routes::tabular::tabular_route_decls());
    table.extend(routes::data::data_route_decls());
    table.extend(routes::data_views::data_view_route_decls());
    table.extend(routes::data_fsm::data_fsm_route_decls());
    table.extend(routes::app_platform::app_platform_route_decls());
    table.extend(routes::app_install::app_install_route_decls());
    table.extend(routes::app_install::trusted_key_route_decls());
    table.extend(routes::office::office_route_decls());
    table
}

async fn openapi_handler() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        openapi::openapi_json(),
    )
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
