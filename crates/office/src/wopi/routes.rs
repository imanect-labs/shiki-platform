//! WOPI エンドポイント（axum Router・StorageService の一クライアント）。
//!
//! 認証は WOPI 仕様どおり **access_token クエリパラメータ**（cookie セッションとは
//! 別の認証面。api 側で Session ミドルウェアを通さず独立にマウントされる）。
//!
//! 全ハンドラ共通の前段（[`authenticate`]）で
//! ① トークン検証（署名・期限・URL の file_id との一致）
//! ② クレームからの AuthContext 再構成
//! ③ **毎呼び出しの OpenFGA check（`HigherConsistency`）**
//! を行う。トークンが残存していても relation 剥奪は次の呼び出しで即時反映される
//! （PIT-11・fail-closed）。失敗は 401（トークン不正）/ 404（読めない・存在秘匿）/
//! 409（ロック競合・X-WOPI-Lock に現 lock_id）へ写像する。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use storage::{NodeKind, StorageError, StorageService};
use uuid::Uuid;

use crate::error::OfficeError;
use crate::wopi::{lock, token};

/// WOPI ルータの共有状態。
#[derive(Clone)]
pub struct WopiState {
    /// ストレージの単一チョークポイント（直バケット禁止・認可/監査/バージョニング込み）。
    pub storage: Arc<StorageService>,
    /// 認可チョークポイント（毎呼び出しの ReBAC 再チェック）。
    pub authz: Arc<dyn AuthzClient>,
    /// `office_lock` 用のプール（WOPI ロックは StorageService の関心外の助言的状態）。
    pub pool: PgPool,
    /// access_token の署名鍵。
    pub token_key: token::OfficeTokenKey,
    /// CheckFileInfo の PostMessageOrigin（web の origin・iframe postMessage 検証用）。
    pub web_origin: Option<String>,
    /// PutFile 本文の上限（storage.max_upload_size_bytes と揃える。axum 既定 2MB を
    /// Office 文書向けに引き上げるための明示値）。
    pub max_body_bytes: usize,
}

/// WOPI ルータを構築する（`/wopi/files/...`）。
///
/// api 側は cookie セッションの middleware を**通さず**にこのルータを merge する
/// （WOPI は access_token クエリの別認証面）。
pub fn build_wopi_router(state: WopiState) -> Router {
    let body_limit = DefaultBodyLimit::max(state.max_body_bytes);
    Router::new()
        .route(
            "/wopi/files/{file_id}",
            get(check_file_info).post(lock_operations),
        )
        .route(
            "/wopi/files/{file_id}/contents",
            get(get_file).post(put_file),
        )
        .layer(body_limit)
        .with_state(state)
}

/// WOPI 仕様の認証クエリ（`?access_token=...`）。
#[derive(Deserialize)]
struct AccessTokenQuery {
    access_token: String,
}

/// 実行主体に許すアクセスモード（毎呼び出しの ReBAC 判定結果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessMode {
    Editor,
    Viewer,
}

/// 全 WOPI ハンドラ共通の前段: トークン検証 → AuthContext 再構成 → 毎回 ReBAC。
///
/// editor → 読み書き / viewer → 読み取りのみ / どちらも無ければ 404（存在秘匿）。
/// 整合性は常に `HigherConsistency`（共有解除の即時反映・PIT-11）。
async fn authenticate(
    state: &WopiState,
    file_id: Uuid,
    access_token: &str,
) -> Result<(AuthContext, AccessMode), WopiFailure> {
    let claims = token::verify(&state.token_key, access_token, file_id)?;
    let ctx = claims.to_auth_context();
    let object = ctx.ns().file(&file_id.to_string());
    let subject = ctx.subject();
    if state
        .authz
        .check(
            &subject,
            Relation::Editor,
            &object,
            Consistency::HigherConsistency,
        )
        .await
        .map_err(OfficeError::from)?
    {
        return Ok((ctx, AccessMode::Editor));
    }
    if state
        .authz
        .check(
            &subject,
            Relation::Viewer,
            &object,
            Consistency::HigherConsistency,
        )
        .await
        .map_err(OfficeError::from)?
    {
        return Ok((ctx, AccessMode::Viewer));
    }
    Err(WopiFailure(OfficeError::NotFound))
}

/// CheckFileInfo 応答（WOPI 仕様の PascalCase プロパティ）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct CheckFileInfo {
    base_file_name: String,
    size: i64,
    /// node.version の文字列表現（PutFile 応答の X-WOPI-ItemVersion と同系）。
    version: String,
    user_id: String,
    user_friendly_name: String,
    user_can_write: bool,
    supports_locks: bool,
    supports_update: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_message_origin: Option<String>,
}

/// GET /wopi/files/{file_id} — CheckFileInfo（viewer 必須）。
async fn check_file_info(
    State(state): State<WopiState>,
    Path(file_id): Path<Uuid>,
    Query(q): Query<AccessTokenQuery>,
) -> Result<Response, WopiFailure> {
    let (ctx, mode) = authenticate(&state, file_id, &q.access_token).await?;
    let node = state
        .storage
        .get_metadata(&ctx, file_id, None)
        .await
        .map_err(conceal)?;
    if node.kind != NodeKind::File {
        return Err(WopiFailure(OfficeError::NotFound));
    }
    let info = CheckFileInfo {
        base_file_name: node.name,
        size: node.size_bytes.unwrap_or(0),
        version: node.version.to_string(),
        user_id: ctx.principal.id.clone(),
        user_friendly_name: ctx.principal.id,
        user_can_write: mode == AccessMode::Editor,
        supports_locks: true,
        supports_update: true,
        post_message_origin: state.web_origin.clone(),
    };
    Ok(Json(info).into_response())
}

/// GET /wopi/files/{file_id}/contents — GetFile（viewer 必須・StorageService 経由）。
async fn get_file(
    State(state): State<WopiState>,
    Path(file_id): Path<Uuid>,
    Query(q): Query<AccessTokenQuery>,
) -> Result<Response, WopiFailure> {
    let (ctx, _mode) = authenticate(&state, file_id, &q.access_token).await?;
    let (node, bytes) = state
        .storage
        .read_file_internal(&ctx, file_id, None)
        .await
        .map_err(conceal)?;
    let content_type = node
        .content_type
        .unwrap_or_else(|| "application/octet-stream".to_string());
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::HeaderName::from_static("x-wopi-itemversion"),
                node.version.to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// POST /wopi/files/{file_id}/contents — PutFile（editor 必須＋X-WOPI-Lock 一致検証）。
///
/// 書込は `update_file_content_internal`（版・監査・書込イベント outbox→RAG 再索引を
/// 同一 txn で担う既存チョークポイント）。content_type は node の既存値を維持する。
async fn put_file(
    State(state): State<WopiState>,
    Path(file_id): Path<Uuid>,
    Query(q): Query<AccessTokenQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, WopiFailure> {
    let (ctx, mode) = authenticate(&state, file_id, &q.access_token).await?;
    if mode != AccessMode::Editor {
        // viewer は存在を知り得る（読める）ため 403 で明示する。
        return Err(WopiFailure(OfficeError::Forbidden));
    }
    // ロック保持者のみ書ける（未ロック時は許可＝Collabora の初回保存互換）。
    lock::check_write_lock(&state.pool, &ctx.tenant_id, file_id, wopi_lock(&headers)?).await?;
    // content_type は既存値を維持（WOPI PutFile は型を運ばない）。
    let node = state
        .storage
        .get_metadata(&ctx, file_id, None)
        .await
        .map_err(conceal)?;
    let content_type = node
        .content_type
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let updated = state
        .storage
        .update_file_content_internal(&ctx, file_id, &body, &content_type, None)
        .await
        .map_err(conceal)?;
    Ok((
        StatusCode::OK,
        [(
            header::HeaderName::from_static("x-wopi-itemversion"),
            updated.version.to_string(),
        )],
    )
        .into_response())
}

/// POST /wopi/files/{file_id} — X-WOPI-Override: LOCK/UNLOCK/REFRESH_LOCK/GET_LOCK
/// （editor 必須）。
async fn lock_operations(
    State(state): State<WopiState>,
    Path(file_id): Path<Uuid>,
    Query(q): Query<AccessTokenQuery>,
    headers: HeaderMap,
) -> Result<Response, WopiFailure> {
    let (ctx, mode) = authenticate(&state, file_id, &q.access_token).await?;
    if mode != AccessMode::Editor {
        return Err(WopiFailure(OfficeError::Forbidden));
    }
    let operation = headers
        .get("x-wopi-override")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| WopiFailure(OfficeError::Invalid("X-WOPI-Override が必要です".into())))?;
    let provided = wopi_lock(&headers)?;
    let require_lock_id = || {
        provided.ok_or_else(|| WopiFailure(OfficeError::Invalid("X-WOPI-Lock が必要です".into())))
    };
    let pool = &state.pool;
    let tenant = &ctx.tenant_id;
    match operation {
        "LOCK" => {
            let lock_id = require_lock_id()?;
            let locked_by = ctx.subject();
            // UnlockAndRelock（X-WOPI-OldLock 付き LOCK）: 旧 lock を解除してから取得する。
            if let Some(old) = header_str(&headers, "x-wopi-oldlock")? {
                lock::unlock(pool, tenant, file_id, old).await?;
            }
            lock::lock(pool, tenant, file_id, lock_id, locked_by.as_str()).await?;
        }
        "UNLOCK" => lock::unlock(pool, tenant, file_id, require_lock_id()?).await?,
        "REFRESH_LOCK" => lock::refresh(pool, tenant, file_id, require_lock_id()?).await?,
        "GET_LOCK" => {
            let current = lock::current_lock(pool, tenant, file_id)
                .await?
                .map(|l| l.lock_id)
                .unwrap_or_default();
            return Ok((StatusCode::OK, [(X_WOPI_LOCK, current)]).into_response());
        }
        other => {
            return Err(WopiFailure(OfficeError::Invalid(format!(
                "未対応の X-WOPI-Override: {other}"
            ))));
        }
    }
    Ok(StatusCode::OK.into_response())
}

const X_WOPI_LOCK: header::HeaderName = header::HeaderName::from_static("x-wopi-lock");

/// X-WOPI-Lock ヘッダを取り出す（非 ASCII は 400）。
fn wopi_lock(headers: &HeaderMap) -> Result<Option<&str>, WopiFailure> {
    header_str(headers, "x-wopi-lock")
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, WopiFailure> {
    headers
        .get(name)
        .map(|v| {
            v.to_str()
                .map_err(|_| WopiFailure(OfficeError::Invalid(format!("{name} が不正です"))))
        })
        .transpose()
}

/// StorageService のエラーを存在秘匿へ写像する。
///
/// Forbidden は 404 に潰す（authenticate を通過した後の剥奪競合や relation 変化でも
/// 対象の存在を漏らさない）。
fn conceal(err: StorageError) -> WopiFailure {
    match err {
        StorageError::Forbidden | StorageError::NotFound => WopiFailure(OfficeError::NotFound),
        other => WopiFailure(OfficeError::from(other)),
    }
}

/// WOPI の HTTP 応答へのエラー写像（401/403/404/409/400/500）。
struct WopiFailure(OfficeError);

impl From<OfficeError> for WopiFailure {
    fn from(err: OfficeError) -> Self {
        WopiFailure(err)
    }
}

impl IntoResponse for WopiFailure {
    fn into_response(self) -> Response {
        match self.0 {
            OfficeError::Unauthorized => StatusCode::UNAUTHORIZED.into_response(),
            OfficeError::NotFound => StatusCode::NOT_FOUND.into_response(),
            OfficeError::Forbidden => StatusCode::FORBIDDEN.into_response(),
            OfficeError::LockConflict { current_lock_id } => {
                // WOPI 準拠: 409 に現 lock_id（無ロック起因は空文字）を添える。
                (StatusCode::CONFLICT, [(X_WOPI_LOCK, current_lock_id)]).into_response()
            }
            OfficeError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            OfficeError::Discovery(detail) => {
                tracing::warn!(error = %detail, "WOPI: discovery 失敗");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
            OfficeError::Storage(e) => {
                tracing::error!(error = %e, "WOPI: storage エラー");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
            OfficeError::Authz(e) => {
                tracing::error!(error = %e, "WOPI: authz エラー");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
            OfficeError::Db(e) => {
                tracing::error!(error = %e, "WOPI: DB エラー");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}
