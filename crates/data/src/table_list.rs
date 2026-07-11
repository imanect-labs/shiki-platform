//! テーブル一覧（Task 9.2/9.8）。`store.rs` から分離（1 ファイル 500 行規約）。
//!
//! FGA viewer 実効集合 → DB 突合の二段。app-gateway の所有束縛（`list_app_tables`）は
//! `app_id` の絞り込みを LIMIT より前に SQL で行う（上限超過時の欠落を防ぐ）。

use authz::{AuthContext, ObjectType, Relation};
use uuid::Uuid;

use crate::model::DataTable;
use crate::store::{DataStore, TableRow};
use crate::{map_db, DataError};

impl DataStore {
    /// 自分が使えるテーブル一覧（FGA viewer 実効集合 → DB 突合の二段・owner 含む）。
    pub async fn list_tables(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        self.list_tables_filtered(ctx, None, limit).await
    }

    /// アプリ所有 ∩ 自分が viewer のテーブル一覧（app-gateway の所有束縛・Task 9.8）。
    ///
    /// `app_id` の絞り込みを **LIMIT より前に SQL で**行う（可視テーブルが上限を超えても
    /// アプリ所有分が一覧から欠落しない）。
    pub async fn list_app_tables(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        self.list_tables_filtered(ctx, Some(app_id), limit).await
    }

    /// アプリが所有する**全**テーブル ID（FGA 突合なし・Task 9.13b アンインストール撤去用）。
    ///
    /// 呼び出し側（InstallService）が mini_app_code アーティファクトの owner を検証してから
    /// 使うこと。ユーザー可視性でフィルタすると失効済み共有の残骸テーブルを撤去できないため、
    /// ここは意図的に tenant＋app_id 束縛のみとする。
    pub async fn table_ids_owned_by_app(
        &self,
        ctx: &AuthContext,
        app_id: Uuid,
    ) -> Result<Vec<Uuid>, DataError> {
        let rows: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM data_table \
             WHERE tenant_id = $1 AND app_id = $2 AND deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(app_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn list_tables_filtered(
        &self,
        ctx: &AuthContext,
        app_id: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        let limit = limit.clamp(1, 200);
        let objs = self
            .authz
            .list_objects(&ctx.subject(), Relation::Viewer, ObjectType::DataTable)
            .await
            .map_err(|e| DataError::Internal(e.to_string()))?;
        let mut ids: Vec<Uuid> = Vec::new();
        for o in objs {
            let Some((_, id_part)) = o.split_once(':') else {
                continue;
            };
            if let Some(local) = ctx.ns().strip_object_id(id_part) {
                if let Ok(id) = Uuid::parse_str(local) {
                    ids.push(id);
                }
            }
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<TableRow> = sqlx::query_as(
            "SELECT id, name, app_id, schema, schema_version, created_by, created_at, updated_at \
             FROM data_table \
             WHERE tenant_id = $1 AND org = $2 AND id = ANY($3) AND deleted_at IS NULL \
               AND ($5::uuid IS NULL OR app_id = $5) \
             ORDER BY updated_at DESC, id DESC LIMIT $4",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ids)
        .bind(limit)
        .bind(app_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(TableRow::into_table).collect())
    }
}
