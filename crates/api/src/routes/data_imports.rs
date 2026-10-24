//! 構造化データ API — 一括取込（Task 15.11・data-platform §16.7・監査 B2-6）。
//!
//! - `POST /data/tables/{id}/imports`: JSON の行（`{ rows: [{ key?, data }] }`）で取込を始める。
//! - `POST /data/tables/{id}/imports/csv`: CSV（`text/csv`・UTF-8・見出し行あり）で取込を始める。
//!   列の照合と値の変換の規則は `data::bulk::csv`（参照名 → 表示名・`_key` 列は行キー・空のセルは値なし・
//!   multi_select / record_ref は `;` 区切りか JSON 配列）。
//! - `GET /data/tables/{id}/imports/{import_id}`: 取込の進捗（件数・状態・parity）。
//! - `GET /data/tables/{id}/imports/{import_id}/errors`: 不合格・拒否された行（行番号の昇順・keyset）。
//!
//! 取込はステージング → 正規検証 → チャンク適用の 3 段（`DataStore::start_bulk_upsert`）で、
//! 開始は **202**（検証と適用はジョブが進める）。editor の検査・監査（`data.record.bulk_import`）・
//! 行数の上限（[`data::MAX_BULK_ROWS`]）は `DataStore`（単一チョークポイント）が担う。本文の大きさの上限は
//! [`MAX_IMPORT_BODY_BYTES`]（超えたら構造化 413 `import_too_large`）。入力の形の誤り（未知の列・行数超過・
//! JSON / CSV の構文）は構造化 400 `import_invalid`（文言は利用者の入力についてだけ・値や内部キーを含めない）。

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json,
};
use data::{BulkImportOptions, BulkImportRow, BulkImportStatus, BulkIndexMode, BulkRowError};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

/// 取込の本文の最大バイト数（CSV の上限 [`data::MAX_BULK_CSV_BYTES`] と同じ。JSON も同じ上限）。
pub const MAX_IMPORT_BODY_BYTES: usize = data::MAX_BULK_CSV_BYTES;
/// 行エラーの 1 ページの既定件数。
const DEFAULT_ERRORS_LIMIT: i64 = 100;

pub(crate) fn data_import_route_decls() -> Vec<RouteDecl> {
    // 本文は自前で上限付きに読む（超過を構造化 413 にするため・axum 既定の 2MB 上限は外す）。
    // 開始は最大 32MiB の受信とステージングへの COPY を含むので長時間許容（300s）。
    vec![
        RouteDecl::new(
            "/data/tables/{id}/imports",
            &["POST"],
            AccessPolicy::SessionLongRunning,
            || post(start_import).layer(axum::extract::DefaultBodyLimit::disable()),
        ),
        RouteDecl::new(
            "/data/tables/{id}/imports/csv",
            &["POST"],
            AccessPolicy::SessionLongRunning,
            || post(start_import_csv).layer(axum::extract::DefaultBodyLimit::disable()),
        ),
        RouteDecl::new(
            "/data/tables/{id}/imports/{import_id}",
            &["GET"],
            AccessPolicy::Session,
            || get(get_import),
        ),
        RouteDecl::new(
            "/data/tables/{id}/imports/{import_id}/errors",
            &["GET"],
            AccessPolicy::Session,
            || get(list_import_errors),
        ),
    ]
}

/// 取込の 1 行（外部形の値・create と同じ形）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct ImportRowInput {
    /// 呼び出し側の行キー（任意・取込内で一意・行エラーで行を特定する）。
    pub key: Option<String>,
    /// レコードの値（参照名 → 値）。
    pub data: serde_json::Value,
}

/// JSON の行で取込を始める。
#[derive(Debug, Deserialize, ToSchema)]
pub struct StartImportRequest {
    pub rows: Vec<ImportRowInput>,
    /// 索引の作り方（既定 `auto`: 2 万行以上かつ既存行数以上なら値索引を遅延構築）。
    #[serde(default)]
    pub index_mode: BulkIndexMode,
    /// 不合格の行が 1 つでもあれば何も適用しない（既定は合格した行だけ適用）。
    #[serde(default)]
    pub abort_on_invalid: bool,
}

/// CSV の取込の選択（クエリ文字列）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct CsvImportQuery {
    /// 索引の作り方（既定 `auto`）。
    #[serde(default)]
    #[param(value_type = Option<BulkIndexMode>)]
    pub index_mode: BulkIndexMode,
    /// 不合格の行が 1 つでもあれば何も適用しない。
    #[serde(default)]
    pub abort_on_invalid: bool,
}

/// 取込の開始の応答（202）。
#[derive(Debug, Serialize, ToSchema)]
pub struct StartImportResponse {
    pub import_id: Uuid,
    /// 開始直後の進捗（`validating`）。
    pub status: BulkImportStatus,
}

/// 行エラーのページ。
#[derive(Debug, Deserialize, IntoParams)]
pub struct ImportErrorsQuery {
    /// この行番号より後ろから（前ページの `next_after_row`）。
    pub after_row: Option<i64>,
    /// 件数（1〜999・既定 100）。
    pub limit: Option<i64>,
}

/// 不合格・拒否された行の一覧。
#[derive(Debug, Serialize, ToSchema)]
pub struct ImportErrorsResponse {
    pub items: Vec<BulkRowError>,
    /// 続きがあるときの次の `after_row`。
    pub next_after_row: Option<i64>,
}

/// 入力の形の誤り（構造化 400・`import_invalid`）。
fn invalid(message: &str) -> ApiError {
    ApiError::UnprocessableJson(crate::error_body::to_body(
        &crate::error_body::ImportInvalidBody {
            status: 400,
            title: "Bad Request".into(),
            code: "import_invalid".into(),
            message: message.to_string(),
        },
    ))
}

/// 取込の開始で起きた data のエラー（入力の誤りは文言つきの構造化 400・それ以外は共通の対応）。
fn start_error(e: data::DataError) -> ApiError {
    match e {
        data::DataError::Invalid(m) => invalid(&m),
        data::DataError::InvalidInput(e) => invalid(e.message()),
        other => ApiError::from(other),
    }
}

/// 構造化 413（`import_too_large`）。
fn too_large() -> ApiError {
    ApiError::PayloadTooLargeJson(crate::error_body::to_body(
        &crate::error_body::PayloadTooLargeBody {
            status: 413,
            title: "Payload Too Large".into(),
            code: "import_too_large".into(),
            max_bytes: MAX_IMPORT_BODY_BYTES,
        },
    ))
}

/// 上限を超えた本文を読み捨てる量の上限（これを超えたら読まずに 413 を返して接続を閉じる）。
const MAX_DRAIN_BYTES: usize = 8 * MAX_IMPORT_BODY_BYTES;
/// 上限を超えた本文を読み捨てる時間の上限。
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 本文を上限付きで読む（超えたら構造化 413）。
///
/// 上限を超えても**残りを読み捨ててから** 413 を返す（有界・[`MAX_DRAIN_BYTES`]・[`DRAIN_TIMEOUT`]）。
/// 送信の途中で応答して接続を閉じると、前段のプロキシ（web の BFF `/api`）が本文の送信で EPIPE になり、
/// 413 ではなく 500 を返していた（検証 A P3）。
async fn read_body(body: Body) -> Result<axum::body::Bytes, ApiError> {
    use futures::StreamExt;
    let mut stream = body.into_data_stream();
    let read = async {
        let mut buf: Vec<u8> = Vec::new();
        let mut over = false;
        let mut seen = 0usize;
        while let Some(chunk) = stream.next().await {
            let Ok(data) = chunk else {
                return Err(invalid("本文を読めませんでした"));
            };
            seen = seen.saturating_add(data.len());
            if over {
                // 上限を超えた後は読み捨てる（読み捨ての上限を超えたら打ち切る）。
                if seen > MAX_DRAIN_BYTES {
                    break;
                }
                continue;
            }
            if buf.len() + data.len() > MAX_IMPORT_BODY_BYTES {
                over = true;
                buf = Vec::new();
                continue;
            }
            buf.extend_from_slice(&data);
        }
        if over {
            Err(too_large())
        } else {
            Ok(axum::body::Bytes::from(buf))
        }
    };
    tokio::time::timeout(DRAIN_TIMEOUT, read)
        .await
        .unwrap_or_else(|_| Err(too_large()))
}

async fn started(
    state: &AppState,
    ctx: &authz::AuthContext,
    id: Uuid,
    import_id: Uuid,
) -> Result<(StatusCode, Json<StartImportResponse>), ApiError> {
    let status = state.data.bulk_import_status(ctx, id, import_id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(StartImportResponse { import_id, status }),
    ))
}

/// JSON の行で一括取込を始める（editor・202・検証と適用はジョブ）。
#[utoipa::path(
    post,
    path = "/data/tables/{id}/imports",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = StartImportRequest,
    responses(
        (status = 202, description = "取込を始めた（検証と適用は非同期）", body = StartImportResponse),
        (status = 400, description = "入力の形の誤り（code=import_invalid・行数超過・行キーの重複・JSON の構文）", body = crate::error_body::ImportInvalidBody),
        (status = 401, description = "未認証"),
        (status = 403, description = "editor でない"),
        (status = 404, description = "テーブルが存在しない"),
        (status = 413, description = "本文が大きすぎる（code=import_too_large）", body = crate::error_body::PayloadTooLargeBody),
    ),
    security(("session" = [])),
)]
pub async fn start_import(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    body: Body,
) -> Result<(StatusCode, Json<StartImportResponse>), ApiError> {
    let bytes = read_body(body).await?;
    let req: StartImportRequest = serde_json::from_slice(&bytes)
        // serde の英語の文言・内部の型名を出さない（検証 N10）。
        .map_err(|e| {
            invalid(&format!(
                "取込の本文を読めません: {}",
                crate::server::rejection::serde_message(&e.to_string())
            ))
        })?;
    let rows = req
        .rows
        .into_iter()
        .map(|r| BulkImportRow {
            key: r.key,
            data: r.data,
        })
        .collect();
    let opts = BulkImportOptions {
        index_mode: req.index_mode,
        abort_on_invalid: req.abort_on_invalid,
    };
    let import_id = state
        .data
        .start_bulk_import(&ctx, id, rows, opts, trace.as_deref())
        .await
        .map_err(start_error)?;
    started(&state, &ctx, id, import_id).await
}

/// CSV で一括取込を始める（editor・202・見出し行の列はフィールドの参照名か表示名）。
#[utoipa::path(
    post,
    path = "/data/tables/{id}/imports/csv",
    params(("id" = Uuid, Path, description = "テーブル ID"), CsvImportQuery),
    request_body(content = String, content_type = "text/csv", description = "UTF-8 の CSV（見出し行あり・`_key` 列は行キー）"),
    responses(
        (status = 202, description = "取込を始めた（検証と適用は非同期）", body = StartImportResponse),
        (status = 400, description = "入力の形の誤り（code=import_invalid・未知の列・派生列・行数超過・文字コード）", body = crate::error_body::ImportInvalidBody),
        (status = 401, description = "未認証"),
        (status = 403, description = "editor でない"),
        (status = 404, description = "テーブルが存在しない"),
        (status = 413, description = "本文が大きすぎる（code=import_too_large）", body = crate::error_body::PayloadTooLargeBody),
    ),
    security(("session" = [])),
)]
pub async fn start_import_csv(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(q): Query<CsvImportQuery>,
    body: Body,
) -> Result<(StatusCode, Json<StartImportResponse>), ApiError> {
    let bytes = read_body(body).await?;
    let opts = BulkImportOptions {
        index_mode: q.index_mode,
        abort_on_invalid: q.abort_on_invalid,
    };
    let import_id = state
        .data
        .start_bulk_import_csv(&ctx, id, &bytes, opts, trace.as_deref())
        .await
        .map_err(start_error)?;
    started(&state, &ctx, id, import_id).await
}

/// 取込の進捗（editor）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/imports/{import_id}",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("import_id" = Uuid, Path, description = "取込 ID"),
    ),
    responses(
        (status = 200, description = "進捗", body = BulkImportStatus),
        (status = 401, description = "未認証"),
        (status = 403, description = "editor でない"),
        (status = 404, description = "テーブル・取込が存在しない（別のテーブルの取込を含む）"),
    ),
    security(("session" = [])),
)]
pub async fn get_import(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path((id, import_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<BulkImportStatus>, ApiError> {
    Ok(Json(
        state.data.bulk_import_status(&ctx, id, import_id).await?,
    ))
}

/// 不合格・拒否された行（editor・行番号の昇順）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/imports/{import_id}/errors",
    params(
        ("id" = Uuid, Path, description = "テーブル ID"),
        ("import_id" = Uuid, Path, description = "取込 ID"),
        ImportErrorsQuery,
    ),
    responses(
        (status = 200, description = "行エラー", body = ImportErrorsResponse),
        (status = 401, description = "未認証"),
        (status = 403, description = "editor でない"),
        (status = 404, description = "テーブル・取込が存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn list_import_errors(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Path((id, import_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<ImportErrorsQuery>,
) -> Result<Json<ImportErrorsResponse>, ApiError> {
    let limit = q.limit.unwrap_or(DEFAULT_ERRORS_LIMIT).clamp(1, 999);
    // 1 件多く取って続きの有無を決める（data 側の上限 1,000 の内側で）。
    let mut items = state
        .data
        .bulk_import_errors(&ctx, id, import_id, q.after_row.unwrap_or(0), limit + 1)
        .await?;
    let more = items.len() as i64 > limit;
    items.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_after_row = if more {
        items.last().map(|r| r.row_no)
    } else {
        None
    };
    Ok(Json(ImportErrorsResponse {
        items,
        next_after_row,
    }))
}
