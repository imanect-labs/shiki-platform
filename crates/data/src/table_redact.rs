//! 閲覧者に返すテーブル定義の絞り込み（検証 C-P2-5・PIT-19）。
//!
//! テーブルを見られる人全員にマスク列・`field_policy`・`row_policy` の式（ロール名入り）を返すと、
//! 読めない列の存在と名前・行の見え方の規則が漏れる（チャットの `data_schema` と SQL 面は隠しているので
//! 列の存在オラクルになる）。スキーマ管理者（テーブルの owner・スキーマを改訂できる人）には定義の全体を、
//! それ以外には**読める列だけ**を、ポリシーの定義・RAG の投影設定を外して返す。
//! 認可材料はこの呼び出しの中だけで使い捨てる（PIT-18）。

use authz::{AuthContext, Relation};
use storage::audit::AuditAction;
use uuid::Uuid;

use crate::model::DataTable;
use crate::policy::memo::AuthzMemo;
use crate::store::DataStore;
use crate::DataError;

impl DataStore {
    /// 閲覧者に見せるテーブル（viewer・スキーマ管理者以外は読める列だけ・ポリシーの定義なし）。
    pub async fn get_table_for_viewer(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        let az = self.authz_memo();
        self.require_in(
            &az,
            ctx,
            id,
            Relation::Viewer,
            AuditAction::DataTableGet,
            trace_id,
        )
        .await?;
        let table = self.fetch_live(ctx, id).await?;
        self.redact_table(ctx, &az, table).await
    }

    /// 閲覧者に見せるテーブル一覧（各テーブルを [`Self::get_table_for_viewer`] と同じ規則で絞る）。
    pub async fn list_tables_for_viewer(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        let tables = self.list_tables(ctx, limit).await?;
        let az = self.authz_memo();
        let mut out = Vec::with_capacity(tables.len());
        for t in tables {
            out.push(self.redact_table(ctx, &az, t).await?);
        }
        Ok(out)
    }

    /// テーブル定義を閲覧者向けに絞る（owner はそのまま）。
    pub(crate) async fn redact_table(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        mut table: DataTable,
    ) -> Result<DataTable, DataError> {
        let obj = ctx.ns().data_table(&table.id.to_string());
        let owner = az
            .check_all(ctx, &[(Relation::Owner, obj)])
            .await?
            .first()
            .copied()
            .unwrap_or(false);
        if owner {
            return Ok(table);
        }
        let masked = self.masked_fields(ctx, az, &table).await?;
        let s = &mut table.schema;
        s.fields.retain(|f| !masked.contains(&f.key));
        if s.status_field.as_ref().is_some_and(|k| masked.contains(k)) {
            s.status_field = None;
        }
        s.field_policy.clear();
        s.row_policy = None;
        s.rag = None;
        Ok(table)
    }
}
