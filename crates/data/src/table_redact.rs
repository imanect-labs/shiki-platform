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
use crate::table_access::ReadScope;
use crate::DataError;

impl DataStore {
    /// 閲覧者に見せるテーブル（viewer か追加だけ・スキーマ管理者以外は読める列だけ・ポリシーの定義なし）。
    ///
    /// 追加だけの人（D-113）には `add_only` を立てて返す（持ち主でない閲覧者と同じく読める列だけ）。
    pub async fn get_table_for_viewer(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        let az = self.authz_memo();
        let table = self
            .open_for_reader(ctx, &az, id, AuditAction::DataTableGet, trace_id)
            .await?;
        self.redact_table(ctx, &az, table).await
    }

    /// 閲覧者に見せるテーブル一覧（各テーブルを [`Self::get_table_for_viewer`] と同じ規則で絞る）。
    ///
    /// 追加だけで共有されたテーブルも含め、`add_only` を立てる（画面の一覧の印・D-113）。
    pub async fn list_tables_for_viewer(
        &self,
        ctx: &AuthContext,
        limit: i64,
    ) -> Result<Vec<DataTable>, DataError> {
        let tables = self.list_tables_with_add_only(ctx, limit).await?;
        let az = self.authz_memo();
        prefetch_table_relations(ctx, &az, &tables).await?;
        let mut out = Vec::with_capacity(tables.len());
        for t in tables {
            out.push(self.redact_table(ctx, &az, t).await?);
        }
        Ok(out)
    }

    /// 閲覧者に見せるテーブル一覧の 1 ページ（名前の検索・続きの位置・GAP-42）。各テーブルは
    /// [`Self::list_tables_for_viewer`] と同じ規則で絞る（検索は名前だけを見るので、列の絞り込みとは独立）。
    pub async fn list_tables_for_viewer_page(
        &self,
        ctx: &AuthContext,
        query: &crate::TableListQuery,
    ) -> Result<crate::TablePage, DataError> {
        let mut page = self.list_tables_page_with_add_only(ctx, query).await?;
        let az = self.authz_memo();
        prefetch_table_relations(ctx, &az, &page.items).await?;
        let mut out = Vec::with_capacity(page.items.len());
        for t in page.items {
            out.push(self.redact_table(ctx, &az, t).await?);
        }
        page.items = out;
        Ok(page)
    }

    /// テーブル定義を閲覧者向けに絞る（owner はそのまま）。追加だけ（viewer でない）なら `add_only` を立てる。
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
        table.add_only = self.read_scope(ctx, az, table.id).await? == Some(ReadScope::OwnRecords);
        // 絞った行述語（追加だけの見え方）も、ほかの閲覧者と同じく定義から外す（下で row_policy を消す）。
        table.narrowed = None;
        let masked = self.masked_fields(ctx, az, &table).await?;
        let s = &mut table.schema;
        s.fields.retain(|f| !masked.contains(&f.key));
        compact_order(&mut s.fields);
        if s.status_field.as_ref().is_some_and(|k| masked.contains(k)) {
            s.status_field = None;
        }
        s.field_policy.clear();
        s.row_policy = None;
        s.rag = None;
        Ok(table)
    }
}

/// 一覧の各テーブルで問う relation（持ち主・閲覧・追加だけ）を BatchCheck にまとめてメモへ入れる（テーブルごとに
/// OpenFGA を往復しない）。
pub(crate) async fn prefetch_table_relations(
    ctx: &AuthContext,
    az: &AuthzMemo<'_>,
    tables: &[DataTable],
) -> Result<(), DataError> {
    let items: Vec<(Relation, authz::FgaObject)> = tables
        .iter()
        .flat_map(|t| {
            let obj = ctx.ns().data_table(&t.id.to_string());
            [
                (Relation::Owner, obj.clone()),
                (Relation::Viewer, obj.clone()),
                (Relation::Submitter, obj),
            ]
        })
        .collect();
    az.check_all(ctx, &items).await.map(|_| ())
}

/// 表示順（`order`）を詰め直す（読めない列を外した後の欠番で、その位置に列があることを見せない・
/// 検証 2-B-P2-1）。相対順は保つ。フィールドの `id` は外部の識別子なので詰めない（残余リスク・
/// data-platform §16.1）。
fn compact_order(fields: &mut [crate::model::FieldDef]) {
    fields.sort_by_key(|f| (f.order, f.id));
    for (i, f) in fields.iter_mut().enumerate() {
        f.order = i32::try_from(i).unwrap_or(i32::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::compact_order;
    use crate::model::{FieldDef, FieldType};

    #[test]
    fn order_is_compacted_and_relative_order_kept() {
        let mut a = FieldDef::new("a", FieldType::Text);
        a.order = 5;
        let mut b = FieldDef::new("b", FieldType::Text);
        b.order = 1;
        let mut c = FieldDef::new("c", FieldType::Text);
        c.order = 9;
        let mut v = vec![a, b, c];
        compact_order(&mut v);
        let got: Vec<(&str, i32)> = v.iter().map(|f| (f.key.as_str(), f.order)).collect();
        assert_eq!(got, vec![("b", 0), ("a", 1), ("c", 2)]);
    }
}
