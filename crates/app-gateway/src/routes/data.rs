//! data.read / data.write / data.schema 能力アダプタ（Task 9.8）。
//!
//! `data::DataStore`（行述語・フィールドマスク・監査込みの単一チョークポイント）へ委譲する。
//! 全ルートがアプリ所有テーブル束縛（[`super::require_app_table`]）を通る。

use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use data::{DataQuery, DataRecord, ListRecordsOptions, QueryResult, TableSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    router::{GatewayCtx, GatewayState},
    GatewayError,
};

use super::require_app_table;

/// アプリへ見せるテーブルの最小メタデータ。
#[derive(Debug, Serialize)]
pub(crate) struct GwTable {
    pub id: Uuid,
    pub name: String,
    pub schema_version: i64,
    pub updated_at: DateTime<Utc>,
}

/// アプリ所有かつ呼出ユーザーが viewer のテーブル一覧。
pub(crate) async fn list_tables(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
) -> Result<Json<Vec<GwTable>>, GatewayError> {
    let tables = state
        .caps
        .data
        .list_app_tables(&ctx.auth, ctx.installation.app_id, 200)
        .await?;
    let own = tables
        .into_iter()
        .map(|t| GwTable {
            id: t.id,
            name: t.name,
            schema_version: t.schema_version,
            updated_at: t.updated_at,
        })
        .collect();
    Ok(Json(own))
}

/// スキーマ応答（data.schema・参照のみ。additive 変更はアップグレード同意フロー＝PR9）。
#[derive(Debug, Serialize)]
pub(crate) struct GwTableSchema {
    pub id: Uuid,
    pub name: String,
    pub schema_version: i64,
    pub schema: TableSchema,
}

pub(crate) async fn get_schema(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path(table_id): Path<Uuid>,
) -> Result<Json<GwTableSchema>, GatewayError> {
    require_app_table(&state, &ctx, table_id).await?;
    // 呼出ユーザーが読めない列・ポリシーの定義は返さない（スキーマ管理者以外・検証 C-P2-5）。
    let t = state
        .caps
        .data
        .get_table_for_viewer(&ctx.auth, table_id, None)
        .await?;
    Ok(Json(GwTableSchema {
        id: t.id,
        name: t.name,
        schema_version: t.schema_version,
        schema: t.schema,
    }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListRecordsQuery {
    pub limit: Option<i64>,
    /// 前ページの `next_cursor`（OFFSET は廃止・keyset ページング）。
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GwRecordList {
    pub items: Vec<DataRecord>,
    /// 個別共有集合が上限（PIT-18）で切り詰められた（可視減方向のフォールバック）。
    pub shares_truncated: bool,
    /// 次ページのカーソル（無ければ終端）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// レコード一覧（行述語適用済み。フィルタ/集計は `POST .../query` を使う）。
pub(crate) async fn list_records(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path(table_id): Path<Uuid>,
    Query(q): Query<ListRecordsQuery>,
) -> Result<Json<GwRecordList>, GatewayError> {
    require_app_table(&state, &ctx, table_id).await?;
    let page = state
        .caps
        .data
        .list_records(
            &ctx.auth,
            table_id,
            &ListRecordsOptions {
                filter: None,
                sort: None,
                limit: q.limit.unwrap_or(50),
                cursor: q.cursor,
            },
            None,
        )
        .await?;
    Ok(Json(GwRecordList {
        items: page.items,
        shares_truncated: page.shares_truncated,
        next_cursor: page.next_cursor,
    }))
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateRecordRequest {
    pub data: Value,
}

pub(crate) async fn create_record(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path(table_id): Path<Uuid>,
    Json(req): Json<CreateRecordRequest>,
) -> Result<Json<DataRecord>, GatewayError> {
    require_app_table(&state, &ctx, table_id).await?;
    let rec = state
        .caps
        .data
        .create_record(&ctx.auth, table_id, req.data, None)
        .await?;
    Ok(Json(rec))
}

pub(crate) async fn get_record(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path((table_id, record_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<DataRecord>, GatewayError> {
    require_app_table(&state, &ctx, table_id).await?;
    let rec = state
        .caps
        .data
        .get_record(&ctx.auth, table_id, record_id, None)
        .await?;
    Ok(Json(rec))
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateRecordRequest {
    /// merge patch（`null` はフィールド除去・required は除去不可）。
    pub patch: Value,
    /// 楽観ロック（不一致は 409）。
    pub expected_rev: i64,
}

pub(crate) async fn update_record(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path((table_id, record_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateRecordRequest>,
) -> Result<Json<DataRecord>, GatewayError> {
    require_app_table(&state, &ctx, table_id).await?;
    let rec = state
        .caps
        .data
        .update_record(
            &ctx.auth,
            table_id,
            record_id,
            req.patch,
            req.expected_rev,
            None,
        )
        .await?;
    Ok(Json(rec))
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeleteRecordQuery {
    pub expected_rev: i64,
}

pub(crate) async fn delete_record(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path((table_id, record_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<DeleteRecordQuery>,
) -> Result<Json<Value>, GatewayError> {
    require_app_table(&state, &ctx, table_id).await?;
    state
        .caps
        .data
        .delete_record(&ctx.auth, table_id, record_id, q.expected_rev, None)
        .await?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// 宣言的クエリ（filter/sort/page/aggregate・行述語＋フィールドマスク＋集計抑制込み）。
pub(crate) async fn query(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path(table_id): Path<Uuid>,
    Json(raw): Json<Value>,
) -> Result<Json<QueryResult>, GatewayError> {
    require_app_table(&state, &ctx, table_id).await?;
    // 最上位の未知のキー（綴り誤り）は黙って無視せず 400。
    let q = DataQuery::from_request(raw)?;
    let result = state
        .caps
        .data
        .run_query(&ctx.auth, table_id, &q, None)
        .await?;
    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
pub(crate) struct TransitionRequest {
    pub to: String,
    pub expected_rev: i64,
}

/// FSM 遷移（唯一の status 変更経路・actor 述語＋outbox＋監査は DataStore 内）。
pub(crate) async fn transition_record(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path((table_id, record_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<TransitionRequest>,
) -> Result<Json<DataRecord>, GatewayError> {
    require_app_table(&state, &ctx, table_id).await?;
    // テーブルにピンされた FSM をテーブルの editor として読み、当該行で遷移する（HTTP・ワークフローと
    // 同じ単一の入口・FsmStore::transition）。
    let rec = state
        .caps
        .fsms
        .transition(
            &ctx.auth,
            table_id,
            record_id,
            &req.to,
            req.expected_rev,
            None,
        )
        .await?;
    Ok(Json(rec))
}

/// レコード変更の購読の開始位置（`Last-Event-ID` ヘッダが優先）。
#[derive(Debug, Deserialize)]
pub(crate) struct StreamQuery {
    pub since: Option<i64>,
}

/// レコード変更の SSE（data.read・Task 15.7 で app-gateway へ公開）。
///
/// ミニアプリ（B1 のグリッド等）が自分の所有テーブルの変更をリアルタイムに受ける入口。中身は
/// data の配信（`DataStore::subscribe_records`）そのもので、**呼出ユーザーの行述語とフィールドマスクを
/// 購読者ごとに評価**し、可視→不可視・削除は `record.removed`（id のみ）を送る（data-platform §9.3）。
/// テーブル単位の束縛しか持たない `events.subscribe`（ドメインイベントのライブテール）には
/// レコード変更を流さず、こちらへ分ける（見えない行の id・変更の事実を漏らさない）。
pub(crate) async fn stream_records(
    State(state): State<GatewayState>,
    Extension(ctx): Extension<GatewayCtx>,
    Path(table_id): Path<Uuid>,
    Query(q): Query<StreamQuery>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response, GatewayError> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use axum::response::IntoResponse;

    require_app_table(&state, &ctx, table_id).await?;
    let since = match headers.get("last-event-id") {
        None => q.since,
        Some(v) => Some(
            v.to_str()
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .ok_or_else(|| GatewayError::Invalid("Last-Event-ID が不正です".into()))?,
        ),
    };
    let rx = state
        .caps
        .data
        .subscribe_records(&ctx.auth, table_id, since, None)
        .await?;
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        let m = rx.next().await?;
        let ev = Event::default()
            .event(m.event.name())
            .data(m.event.data_json().to_string());
        let ev = match m.id {
            Some(id) => ev.id(id.to_string()),
            None => ev,
        };
        Some((Ok::<Event, std::convert::Infallible>(ev), rx))
    });
    let mut resp = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    // プロキシに溜めさせない（api の SSE と同じ）。
    resp.headers_mut().insert(
        "x-accel-buffering",
        axum::http::HeaderValue::from_static("no"),
    );
    Ok(resp)
}
