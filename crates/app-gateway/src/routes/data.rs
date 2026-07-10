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
    let tables = state.caps.data.list_tables(&ctx.auth, 200).await?;
    let own = tables
        .into_iter()
        .filter(|t| t.app_id == Some(ctx.installation.app_id))
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
    let t = require_app_table(&state, &ctx, table_id).await?;
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
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GwRecordList {
    pub items: Vec<DataRecord>,
    /// 個別共有集合が上限（PIT-18）で切り詰められた（可視減方向のフォールバック）。
    pub shares_truncated: bool,
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
                offset: q.offset.unwrap_or(0),
            },
            None,
        )
        .await?;
    Ok(Json(GwRecordList {
        items: page.items,
        shares_truncated: page.shares_truncated,
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
    Json(q): Json<DataQuery>,
) -> Result<Json<QueryResult>, GatewayError> {
    require_app_table(&state, &ctx, table_id).await?;
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
    let table = require_app_table(&state, &ctx, table_id).await?;
    let fsm_ref = table.schema.fsm_ref.clone().ok_or_else(|| {
        GatewayError::Invalid("このテーブルは FSM 管理されていません（fsm_ref 未設定）".into())
    })?;
    let status_field =
        table.schema.status_field.clone().ok_or_else(|| {
            GatewayError::Invalid("このテーブルに status_field がありません".into())
        })?;
    // ピンされたバージョンの FSM 定義を artifact チョークポイント（FsmStore）経由で解決する。
    let (_, fsm) = state
        .caps
        .fsms
        .get(&ctx.auth, fsm_ref.artifact_id, Some(fsm_ref.version), None)
        .await?;
    let rec = state
        .caps
        .data
        .transition_record(
            &ctx.auth,
            table_id,
            record_id,
            &req.to,
            req.expected_rev,
            &fsm,
            &status_field,
            None,
        )
        .await?;
    Ok(Json(rec))
}
