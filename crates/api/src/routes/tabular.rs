//! CSV クエリ/パッチ API（Phase 11-pre Task 11P.7・design §4.8.2）。
//!
//! すべて `TabularService`（単一チョークポイント・AuthContext 必須）を通す。DuckDB 実行は
//! 非特権別プロセスに隔離される（api プロセスは CSV を解釈しない・PIT-39）。

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthContextExt, TraceIdExt};
use crate::server::RouteDecl;
use crate::state::AppState;

/// tabular のルート宣言（route_table から分離）。
pub(crate) fn tabular_route_decls() -> Vec<RouteDecl> {
    use crate::server::AccessPolicy::Session;
    let r = RouteDecl::new;
    vec![
        r("/files/{id}/tabular/schema", &["GET"], Session, || {
            get(get_schema)
        }),
        r("/files/{id}/tabular/rows", &["GET"], Session, || {
            get(get_rows)
        }),
        r("/files/{id}/tabular/query", &["POST"], Session, || {
            post(post_query)
        }),
        r("/files/{id}/tabular/patch", &["POST"], Session, || {
            post(post_patch)
        }),
        r("/tabular/save", &["POST"], Session, || post(post_save)),
        r("/tabular/reserved-words", &["GET"], Session, || {
            get(get_reserved_words)
        }),
    ]
}

/// SQL の予約語の一覧。
#[derive(Debug, Serialize, ToSchema)]
pub struct ReservedWordsResponse {
    /// 予約語（小文字・昇順）。追加テーブルの名前にはこれらを使えない。
    pub words: Vec<String>,
}

/// SQL の予約語の一覧を返す（#1061）。
///
/// 複数 CSV クエリの追加テーブル名に予約語（`order` など）は使えない。画面側の名前の補完と
/// 事前検証のために、判定と同じ語彙（DuckDB 自身の `duckdb_keywords()`）を返す。ファイルは
/// 読まない（公開情報）が、ログイン中の利用者に限る。
#[utoipa::path(
    get, path = "/tabular/reserved-words",
    responses((status = 200, body = ReservedWordsResponse)),
    security(("session" = [])),
)]
pub async fn get_reserved_words(
    State(state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
) -> Result<Json<ReservedWordsResponse>, ApiError> {
    let words = state.tabular.reserved_words().await.map_err(to_api_error)?;
    Ok(Json(ReservedWordsResponse {
        words: words.to_vec(),
    }))
}

/// tabular のエラーを HTTP へ写す（fail-closed・存在秘匿）。
fn to_api_error(err: tabular::TabularError) -> ApiError {
    use tabular::TabularError as TE;
    match err {
        TE::Forbidden | TE::Authz(_) => ApiError::Forbidden,
        TE::NotFound(_) => ApiError::NotFound,
        TE::Storage(e) => ApiError::from(e),
        // SQL の誤りと、追加のテーブルの指定の誤り（JOIN・#1059）は、文言をそのまま返す 400。
        TE::SqlRejected(m) | TE::InvalidTables(m) => ApiError::BadRequest(m),
        // 編集の誤り（元の文字コードで書けない文字など）と、文字コードを決められない・文章でない CSV、読めない Excel、
        // CSV の編集・保存に渡した Excel は、文言つきの構造化 400（画面がそのまま出す・GAP-45・D-17）。
        TE::InvalidPatch(m) | TE::NotText(m) | TE::Unsupported(m) => ApiError::invalid_input(&m),
        TE::QuotaExceeded(m) => ApiError::BadRequest(format!("クォータ超過: {m}")),
        TE::RevConflict { base, current } => ApiError::ConflictJson(serde_json::json!({
            "status": 409,
            "title": "競合しています（他の編集で更新されました）",
            "base_rev": base,
            "current_rev": current,
        })),
        TE::Runner(m) => ApiError::Internal(format!("tabular runner: {m}")),
        // ユーザー SQL の実行失敗は 400 で理由（DuckDB のエラー文）をそのまま返す。
        // `BadRequest` は body が {status,title} に潰れるため、詳細を保つ構造化 400 を使う。
        TE::QueryFailed(m) => ApiError::UnprocessableJson(serde_json::json!({
            "status": 400,
            "title": m,
        })),
        TE::Internal(m) => ApiError::Internal(m),
    }
}

/// 新規保存の誤りを HTTP へ写す。
///
/// 新規保存の名前の衝突（[`storage::StorageError::Conflict`]）は、編集の重なり（`rev` の衝突）と分けて
/// `name_taken` の構造化 409 にする。画面は「同じ名前のファイルがある」と言い、番号を付けて保存する道を出す
/// （BUG-150）。新規作成なので、版の衝突はここでは起きない。
fn save_error(err: tabular::TabularError) -> ApiError {
    use crate::error_body::{to_body, ConflictBody, ConflictCode};
    match err {
        tabular::TabularError::Storage(storage::StorageError::Conflict) => {
            ApiError::ConflictJson(to_body(&ConflictBody::new(ConflictCode::NameTaken)))
        }
        other => to_api_error(other),
    }
}

/// 結果テーブル（クエリ/行取得の共通レスポンス）。
#[derive(Debug, Serialize, ToSchema)]
pub struct TableResponse {
    pub columns: Vec<String>,
    #[schema(value_type = Vec<Vec<Option<String>>>)]
    pub rows: Vec<Vec<Option<String>>>,
    #[schema(value_type = Option<u64>)]
    pub total_rows: Option<u64>,
    pub truncated: bool,
    /// 次ページの offset（rows 取得時・None なら末尾）。
    #[schema(value_type = Option<u64>)]
    pub next_offset: Option<u64>,
}

impl TableResponse {
    fn from_runner(resp: tabular::RunnerResponse, next_offset: Option<u64>) -> Self {
        TableResponse {
            columns: resp.columns,
            rows: resp.rows,
            total_rows: resp.total_rows,
            truncated: resp.truncated,
            next_offset,
        }
    }
}

/// スキーマ（列名・総行数）。Excel はシートの一覧と、表題の行を飛ばしたときの見出しの行も返す。
#[derive(Debug, Serialize, ToSchema)]
pub struct SchemaResponse {
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    #[schema(value_type = Option<u64>)]
    pub total_rows: Option<u64>,
    /// Excel のシート（表のシートだけ・ブックの順・CSV は空・GAP-84）。`data` のシートが列の案内の表。
    pub sheets: Vec<SheetResponse>,
    /// 列の名前にした行（Excel の行の番号・1 から）。表の上の表題の行を飛ばしたときだけ（GAP-83）。
    #[schema(value_type = Option<u32>)]
    pub header_row: Option<u32>,
}

/// Excel のシート（[`tabular::SheetInfo`]）。
#[derive(Debug, Serialize, ToSchema)]
pub struct SheetResponse {
    /// シートの名前。
    pub name: String,
    /// 隠したシート。
    pub hidden: bool,
    /// SQL の `data` として読むシート（表示している左端のシート）。
    pub data: bool,
    /// SQL に `FROM "シート名"` と書いて読めるか（`data` と同じ名前のほかのシートなどは読めない）。
    pub usable: bool,
}

impl From<tabular::SheetInfo> for SheetResponse {
    fn from(s: tabular::SheetInfo) -> Self {
        SheetResponse {
            name: s.name,
            hidden: s.hidden,
            data: s.data,
            usable: s.usable,
        }
    }
}

/// CSV スキーマを返す（viewer 認可）。
#[utoipa::path(
    get, path = "/files/{id}/tabular/schema",
    params(("id" = Uuid, Path, description = "CSV ファイル ID")),
    responses((status = 200, body = SchemaResponse), (status = 403), (status = 404)),
    security(("session" = [])),
)]
pub async fn get_schema(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
) -> Result<Json<SchemaResponse>, ApiError> {
    let resp = state
        .tabular
        .schema(&ctx, id, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    Ok(Json(SchemaResponse {
        columns: resp.columns,
        column_types: resp.column_types,
        total_rows: resp.total_rows,
        sheets: resp.sheets.into_iter().map(SheetResponse::from).collect(),
        header_row: resp.header_row,
    }))
}

/// 行ページ取得のクエリパラメータ。
#[derive(Debug, Deserialize)]
pub struct RowsParams {
    #[serde(default)]
    pub offset: u64,
}

/// CSV の 1 ページを返す（viewer 認可・無限スクロール）。
#[utoipa::path(
    get, path = "/files/{id}/tabular/rows",
    params(
        ("id" = Uuid, Path, description = "CSV ファイル ID"),
        ("offset" = Option<u64>, Query, description = "取得開始行（0 始まり）"),
    ),
    responses((status = 200, body = TableResponse), (status = 403), (status = 404)),
    security(("session" = [])),
)]
pub async fn get_rows(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Query(params): Query<RowsParams>,
) -> Result<Json<TableResponse>, ApiError> {
    let resp = state
        .tabular
        .rows(&ctx, id, params.offset, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    // 次ページ offset: このページが埋まっていれば続きがある。
    let page = u64::from(state.tabular.page_size());
    let got = resp.rows.len() as u64;
    let next = (got >= page).then_some(params.offset + got);
    Ok(Json(TableResponse::from_runner(resp, next)))
}

/// RO SQL クエリ。
#[derive(Debug, Deserialize, ToSchema)]
pub struct QueryRequest {
    /// 読み取り専用 SELECT。パスの CSV はテーブル `data`、`tables` の各 CSV は指定名で参照する。
    pub sql: String,
    /// JOIN 用に追加でロードする CSV（各ファイルにも閲覧権限が要る・最大 8 個・#1037）。
    #[serde(default)]
    pub tables: Vec<QueryTable>,
}

/// 追加テーブル 1 件（SQL から参照する名前 → CSV ファイル）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct QueryTable {
    /// テーブル名（英小文字・数字・`_`、先頭は数字以外、63 文字以内。`data` は不可）。
    pub name: String,
    /// CSV ファイル ID。
    pub file_id: Uuid,
}

/// 読み取り専用 SQL を実行する（viewer 認可・SQL 検証必須）。
///
/// `tables` を渡すと複数 CSV を JOIN できる。対象の**全ファイル**に閲覧権限が要り、1 つでも
/// 欠ければ 404（存在秘匿＝無いファイルと区別しない・部分結果は返さない）。
#[utoipa::path(
    post, path = "/files/{id}/tabular/query", request_body = QueryRequest,
    params(("id" = Uuid, Path, description = "CSV ファイル ID")),
    responses((status = 200, body = TableResponse), (status = 400), (status = 403), (status = 404)),
    security(("session" = [])),
)]
pub async fn post_query(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<TableResponse>, ApiError> {
    let tables: Vec<tabular::TableRef> = req
        .tables
        .into_iter()
        .map(|t| tabular::TableRef {
            name: t.name,
            file_id: t.file_id,
        })
        .collect();
    let resp = state
        .tabular
        .query_tables(&ctx, id, &tables, &req.sql, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    Ok(Json(TableResponse::from_runner(resp, None)))
}

/// パッチ適用リクエスト（rev 楽観ロック）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct PatchRequest {
    /// 編集開始時の版（node.version）。不一致は 409。
    pub base_rev: i64,
    /// パッチ操作列（cell_update / row_insert / row_delete / column_add / column_delete / column_rename）。
    #[schema(value_type = Vec<serde_json::Value>)]
    pub ops: Vec<tabular::PatchOp>,
}

/// パッチ適用結果。
#[derive(Debug, Serialize, ToSchema)]
pub struct PatchResponse {
    pub node_id: Uuid,
    pub version: i64,
    pub rows: usize,
    pub cols: usize,
}

/// セル/行/列パッチを適用して新バージョン保存する（editor 認可）。
#[utoipa::path(
    post, path = "/files/{id}/tabular/patch", request_body = PatchRequest,
    params(("id" = Uuid, Path, description = "CSV ファイル ID")),
    responses((status = 200, body = PatchResponse), (status = 400), (status = 403), (status = 404), (status = 409)),
    security(("session" = [])),
)]
pub async fn post_patch(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchRequest>,
) -> Result<Json<PatchResponse>, ApiError> {
    let applied = state
        .tabular
        .patch(&ctx, id, req.base_rev, &req.ops, trace.0.as_deref())
        .await
        .map_err(to_api_error)?;
    Ok(Json(PatchResponse {
        node_id: applied.node_id,
        version: applied.version,
        rows: applied.rows,
        cols: applied.cols,
    }))
}

/// 新規 CSV 保存リクエスト（SQL 結果の「新規 CSV として保存」等）。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveRequest {
    pub parent_id: Option<Uuid>,
    pub name: String,
    /// CSV 本文（そのまま保存）。
    pub csv: String,
    /// 同じ名前のファイルがあるとき（省略は失敗にする＝`name_taken` の 409）。`add_number` は `名前 (2).csv` から
    /// 空いている名前で保存する（ドライブの新規作成・ノートと同じ規則・D-112）。
    #[serde(default)]
    pub on_name_taken: storage::OnNameTaken,
}

/// 保存結果。
#[derive(Debug, Serialize, ToSchema)]
pub struct SaveResponse {
    pub node_id: Uuid,
    pub version: i64,
    pub name: String,
}

/// 新規 CSV を保存する（保存先フォルダの作成権限）。
#[utoipa::path(
    post, path = "/tabular/save", request_body = SaveRequest,
    responses(
        (status = 200, body = SaveResponse),
        (status = 400),
        (status = 403),
        (status = 409, description = "保存先に同じ名前のファイルがある（code=name_taken・番号を付けるときは (50) まで埋まっている）", body = crate::error_body::ConflictBody),
    ),
    security(("session" = [])),
)]
pub async fn post_save(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Json(req): Json<SaveRequest>,
) -> Result<Json<SaveResponse>, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::BadRequest("ファイル名を指定してください".into()));
    }
    let saved = state
        .tabular
        .save_new(
            &ctx,
            req.parent_id,
            name,
            req.csv.as_bytes(),
            req.on_name_taken,
            trace.0.as_deref(),
        )
        .await
        .map_err(save_error)?;
    Ok(Json(SaveResponse {
        node_id: saved.node_id,
        version: saved.version,
        name: saved.name,
    }))
}
