//! テーブルの閲覧者文脈（Task 15.5・グリッドとビュー編集の表示制御）。
//!
//! 画面が「どの列が自分に読めないか」「編集・スキーマ変更ができるか」を知るための入口。
//! 判定は読取・書込の各 API と同じ（マスクは [`DataStore::masked_field_keys`]・権限は OpenFGA の
//! relation）で、**表示の出し分けのためだけ**に使う（実際の読み書きは各 API が改めて認可する）。
//! 認可材料はこの呼び出しの中だけで使い捨てる（PIT-18）。

use authz::{AuthContext, Relation};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::store::DataStore;
use crate::DataError;

/// テーブルに対する閲覧者本人の文脈。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TableViewerContext {
    /// 閲覧者本人に**読めない**列の参照名（フィールドマスク・ビューの列設定に出せない）。
    pub masked_fields: Vec<String>,
    /// レコードを編集できる（テーブルの editor 以上）。行述語の書込条件は別途各行で効く。
    pub can_edit: bool,
    /// スキーマを変えられる（テーブルの owner）。
    pub can_manage: bool,
    /// 1 ビューで表示できるルックアップ・ロールアップ列の上限（テナント設定）。
    pub max_view_lookup_columns: u32,
}

impl DataStore {
    /// テーブルの閲覧者文脈（viewer）。
    pub async fn table_viewer_context(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<TableViewerContext, DataError> {
        let az = self.authz_memo();
        self.require_in(
            &az,
            ctx,
            table_id,
            Relation::Viewer,
            "data.table.get",
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, table_id).await?;
        let obj = ctx.ns().data_table(&table_id.to_string());
        let rel = az
            .check_all(
                ctx,
                &[(Relation::Editor, obj.clone()), (Relation::Owner, obj)],
            )
            .await?;
        let mut masked: Vec<String> = self
            .masked_fields(ctx, &az, &table)
            .await?
            .into_iter()
            .collect();
        masked.sort();
        Ok(TableViewerContext {
            masked_fields: masked,
            can_edit: rel.first().copied().unwrap_or(false),
            can_manage: rel.get(1).copied().unwrap_or(false),
            max_view_lookup_columns: self.view_lookup_cap(ctx).await?,
        })
    }
}
