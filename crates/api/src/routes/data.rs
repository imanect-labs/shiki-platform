//! 構造化データ API — テーブル面（Task 9.2）。
//!
//! スキーマレジストリ（作成・一覧・取得・additive 改訂・論理削除）。レコード面は
//! [`super::data_records`]。権限・検証・監査は `DataStore`（単一チョークポイント）が担う。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use data::{DataTable, NewDataTable, TableListQuery, TableSchema, MAX_TABLE_PAGE};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use axum::routing::{get, put};

use crate::server::RouteDecl;
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    state::AppState,
};

/// テーブル作成リクエスト。
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTableRequest {
    pub name: String,
    pub schema: TableSchema,
}

/// スキーマ改訂リクエスト（additive のみ・楽観ロック）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSchemaRequest {
    pub schema: TableSchema,
    pub expected_schema_version: Option<i64>,
}

/// テーブル一覧レスポンス（更新の新しい順）。
#[derive(Debug, Serialize, ToSchema)]
pub struct TableListResponse {
    pub items: Vec<DataTable>,
    /// 続きの位置（`cursor` に渡すと次のページを返す・無ければ null で、これが最後のページ）。
    pub next_cursor: Option<String>,
}

/// テーブル一覧の絞り込みと続き（GAP-42）。
#[derive(Debug, Deserialize, IntoParams)]
pub struct TableListParams {
    /// 名前に含む文字（大文字小文字を区別しない・前後の空白は除く・128 文字まで）。
    pub q: Option<String>,
    /// 前のページの `next_cursor`。
    pub cursor: Option<String>,
    /// 1 ページの件数（1〜200・既定 200）。
    pub limit: Option<i64>,
}

/// テーブルを作成する（スキーマ検証＋索引台帳の初期化・201）。
#[utoipa::path(
    post,
    path = "/data/tables",
    request_body = CreateTableRequest,
    responses(
        (status = 201, description = "作成した", body = DataTable),
        (status = 400, description = "スキーマが不正", body = crate::error_body::BadRequestBody),
        (status = 401, description = "未認証"),
        (status = 409, description = "同名テーブルが既に存在する", body = crate::error_body::ConflictBody),
    ),
    security(("session" = [])),
)]
pub async fn create_table(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<CreateTableRequest>,
) -> Result<(StatusCode, Json<DataTable>), ApiError> {
    let created = state
        .data
        .create_table(
            &ctx,
            NewDataTable {
                name: req.name,
                schema: req.schema,
            },
            trace.as_deref(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// 自分が使えるテーブル一覧（ReBAC viewer 実効集合・更新の新しい順）。
///
/// `q` で名前を探し、`next_cursor` で続きを読む（エディタのテーブルの選択が、先頭の 200 件より先の
/// テーブルを名前で探せるように・GAP-42）。検索と続きは viewer 実効集合の中だけで絞る。
#[utoipa::path(
    get,
    path = "/data/tables",
    params(TableListParams),
    responses(
        (status = 200, description = "一覧", body = TableListResponse),
        (status = 400, description = "cursor が不正（code=cursor_invalid）・q が長すぎる", body = crate::error_body::BadRequestBody),
        (status = 401, description = "未認証"),
    ),
    security(("session" = [])),
)]
pub async fn list_tables(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(p): Query<TableListParams>,
) -> Result<Json<TableListResponse>, ApiError> {
    let query = TableListQuery {
        q: p.q,
        cursor: p.cursor,
        limit: p.limit.unwrap_or(MAX_TABLE_PAGE),
    };
    // スキーマ管理者（owner）以外には読める列だけ・ポリシーの定義なしで返す（検証 C-P2-5）。
    let page = state.data.list_tables_for_viewer_page(&ctx, &query).await?;
    Ok(Json(TableListResponse {
        items: page.items,
        next_cursor: page.next_cursor,
    }))
}

/// テーブルのメタ＋スキーマを取得する（viewer）。
///
/// スキーマ管理者（owner）以外には**読める列だけ**を返し、`field_policy`・`row_policy`・RAG の投影設定は返さない。
#[utoipa::path(
    get,
    path = "/data/tables/{id}",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    responses(
        (status = 200, description = "テーブル", body = DataTable),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_table(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<DataTable>, ApiError> {
    // スキーマ管理者（owner）以外には読める列だけ・ポリシーの定義なしで返す（検証 C-P2-5）。
    Ok(Json(
        state
            .data
            .get_table_for_viewer(&ctx, id, trace.as_deref())
            .await?,
    ))
}

/// スキーマを改訂する（owner・additive のみ・増えた索引は既存レコードをバックフィル）。
/// テーブルに対する自分の文脈（読めない列・編集/スキーマ変更の可否・ビューのルックアップ列の上限）。
///
/// 画面の出し分け専用（実際の読み書きは各 API が改めて認可する）。
#[utoipa::path(
    get,
    path = "/data/tables/{id}/viewer-context",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    responses(
        (status = 200, description = "閲覧者の文脈", body = data::TableViewerContext),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn get_viewer_context(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<data::TableViewerContext>, ApiError> {
    Ok(Json(
        state
            .data
            .table_viewer_context(&ctx, id, trace.as_deref())
            .await?,
    ))
}

#[utoipa::path(
    put,
    path = "/data/tables/{id}/schema",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    request_body = UpdateSchemaRequest,
    responses(
        (status = 200, description = "改訂後のテーブル", body = DataTable),
        (status = 400, description = "スキーマが不正（型変更・削除を含む）", body = crate::error_body::BadRequestBody),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner でない）"),
        (status = 404, description = "存在しない"),
        (status = 409, description = "expected_schema_version 不一致", body = crate::error_body::ConflictBody),
    ),
    security(("session" = [])),
)]
pub async fn update_table_schema(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSchemaRequest>,
) -> Result<Json<DataTable>, ApiError> {
    let updated = state
        .data
        .update_table_schema(
            &ctx,
            id,
            req.schema,
            req.expected_schema_version,
            trace.as_deref(),
        )
        .await?;
    Ok(Json(updated))
}

/// テーブルを論理削除する（owner）。
#[utoipa::path(
    delete,
    path = "/data/tables/{id}",
    params(("id" = Uuid, Path, description = "テーブル ID")),
    responses(
        (status = 204, description = "削除した"),
        (status = 401, description = "未認証"),
        (status = 403, description = "権限がない（owner でない）"),
        (status = 404, description = "存在しない"),
    ),
    security(("session" = [])),
)]
pub async fn delete_table(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    state.data.delete_table(&ctx, id, trace.as_deref()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 構造化データ（Task 9.2/9.3/9.5）のルート宣言（route_table から分離・同じ宣言的マップの一部）。
pub(crate) fn data_route_decls() -> Vec<RouteDecl> {
    use crate::server::AccessPolicy::Session;
    let r = crate::server::RouteDecl::new;
    vec![
        r("/data/tables", &["GET", "POST"], Session, || {
            get(self::list_tables).post(self::create_table)
        }),
        r("/data/tables/{id}", &["GET", "DELETE"], Session, || {
            get(self::get_table).delete(self::delete_table)
        }),
        r("/data/tables/{id}/schema", &["PUT"], Session, || {
            put(self::update_table_schema)
        }),
        r(
            "/data/tables/{id}/viewer-context",
            &["GET"],
            Session,
            || get(self::get_viewer_context),
        ),
        r(
            "/data/tables/{id}/records",
            &["GET", "POST"],
            Session,
            || get(super::data_records::list_records).post(super::data_records::create_record),
        ),
        r(
            "/data/tables/{id}/records/{record_id}",
            &["GET", "PATCH", "DELETE"],
            Session,
            || {
                get(super::data_records::get_record)
                    .patch(super::data_records::update_record)
                    .delete(super::data_records::delete_record)
            },
        ),
        r(
            "/data/tables/{id}/records/{record_id}/revisions",
            &["GET"],
            Session,
            || get(super::data_records::list_revisions),
        ),
        r("/data/tables/{id}/records/count", &["GET"], Session, || {
            get(super::data_records::count_records)
        }),
        r("/data/tables/{id}/stream", &["GET"], Session, || {
            get(super::data_stream::stream_records)
        }),
        r("/data/tables/{id}/awareness", &["POST"], Session, || {
            axum::routing::post(super::data_stream::post_awareness)
        }),
        r(
            "/data/tables/{id}/records/{record_id}/shares",
            &["PUT", "DELETE"],
            Session,
            || put(super::data_records::share_record).delete(super::data_records::unshare_record),
        ),
    ]
}
