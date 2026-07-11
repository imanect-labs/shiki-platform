//! 能力アダプタのルート群（Task 9.8）。
//!
//! 各ハンドラは二重ゲート通過後の [`GatewayCtx`]（呼出ユーザーの AuthContext）で内部
//! チョークポイント（`DataStore` / `StorageService` / [`crate::RagPort`]）へ**薄く委譲**する。
//! per-call OpenFGA（第4ゲート）は各ストア内部の既存実装が担い、ここで独自の権限判定は
//! 書かない（単一チョークポイント・アーキ不変条件）。
//!
//! **data.\* のリソース束縛**: アプリ所有テーブル（`data_table.app_id = installation.app_id`）
//! のみ操作可。スコープが付与されていても非所有テーブルは 403（[`require_app_table`]）。

pub(crate) mod ai;
pub(crate) mod data;
pub(crate) mod events;
pub(crate) mod functions;
pub(crate) mod identity;
pub(crate) mod notify;
pub(crate) mod rag;
pub(crate) mod storage;

use axum::{
    routing::{get, post},
    Router,
};
use uuid::Uuid;

use crate::{
    router::{GatewayCtx, GatewayState},
    GatewayError,
};

/// 能力アダプタの全ルート（[`crate::scope_map::GATEWAY_ROUTES`] と 1:1 対応）。
pub(crate) fn capability_router() -> Router<GatewayState> {
    Router::new()
        // data.*（アプリ所有テーブル束縛）
        .route("/gw/data/tables", get(data::list_tables))
        .route("/gw/data/tables/{table_id}/schema", get(data::get_schema))
        .route(
            "/gw/data/tables/{table_id}/records",
            get(data::list_records).post(data::create_record),
        )
        .route(
            "/gw/data/tables/{table_id}/records/{record_id}",
            get(data::get_record)
                .patch(data::update_record)
                .delete(data::delete_record),
        )
        .route("/gw/data/tables/{table_id}/query", post(data::query))
        .route(
            "/gw/data/tables/{table_id}/records/{record_id}/transition",
            post(data::transition_record),
        )
        // storage.*（StorageService 経由・個人 ReBAC）
        .route("/gw/storage/nodes/{node_id}", get(storage::get_metadata))
        .route(
            "/gw/storage/nodes/{node_id}/children",
            get(storage::list_children),
        )
        .route(
            "/gw/storage/nodes/{node_id}/download-url",
            get(storage::download_url),
        )
        .route("/gw/storage/folders", post(storage::create_folder))
        // rag.query（permission-aware・port 越し）
        .route("/gw/rag/query", post(rag::query))
        // identity.read（最小 DTO）
        .route("/gw/identity/me", get(identity::me))
        // events.subscribe（SSE ライブテール）
        .route("/gw/events/subscribe", get(events::subscribe))
        // notify.send（台帳記録）
        .route("/gw/notify/send", post(notify::send))
        // llm.invoke / agent.invoke（SSE・Task 9.9）
        .route("/gw/ai/llm/invoke", post(ai::llm_invoke))
        .route("/gw/ai/agent/invoke", post(ai::agent_invoke))
        // B2 関数のユーザー起点起動（Task 9.12）
        .route(
            "/gw/apps/functions/{function}/invoke",
            post(functions::invoke),
        )
}

/// アプリ所有テーブル束縛（data.\* の第5のリソースゲート）。
///
/// 呼出ユーザーの viewer ReBAC（`get_table` 内・不可視は 404＝存在オラクルなし）を通した上で、
/// `app_id` がインストール中アプリと一致しないテーブルは**スコープが付与されていても 403**。
pub(crate) async fn require_app_table(
    state: &GatewayState,
    ctx: &GatewayCtx,
    table_id: Uuid,
) -> Result<data_crate::DataTable, GatewayError> {
    let table = state.caps.data.get_table(&ctx.auth, table_id, None).await?;
    if table.app_id != Some(ctx.installation.app_id) {
        return Err(GatewayError::Forbidden(
            "このアプリが所有していないテーブルです".into(),
        ));
    }
    Ok(table)
}

// lib 名の衝突回避（モジュール `data` とクレート `data` を区別する）。
use ::data as data_crate;
