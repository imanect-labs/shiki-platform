//! 作成したレコードの書込副作用（索引行・材料化・リビジョン）の**単一の入口**。
//!
//! 1 件の create（[`crate::record`]）と一括書込パス（[`crate::bulk`]・data-platform §16.7）は、本体の
//! INSERT の後に**同じこの関数**を同じ Tx で呼ぶ。経路ごとに副作用を書き分けないので、一括経路だけが
//! 索引・`rev`・リビジョン・`Invariant` 材料化の規則から外れることが構造的に起きない。
//!
//! - 索引行: [`crate::index::write::IndexWriter::on_create_many`]（表ごとに 1 文・1 件でも同じ文）
//! - 材料化: [`crate::derived::materialize::after_records_written`]（`Invariant` 列をまとめて計算）
//! - リビジョン: `rev = 1`・`change_kind = create`・差分は `{}` → 書込後の値（[`crate::revision`]）
//!
//! **レコード変更の outbox / SSE（Task 15.6）はここで発行すること。** 1 件の create と一括取込の
//! 両方が自動的に覆われる（一括経路に別の発行口を作らない）。監査は経路ごと（1 件は 1 行・一括は
//! チャンクごとに 1 行）で、ここでは書かない。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::index::state::IndexPlan;
use crate::index::write::IndexedRecord;
use crate::index::IndexTable;
use crate::model::{DataTable, FieldId};
use crate::revision::{diff_fields, insert_revisions, RevisionInsert};
use crate::store::DataStore;
use crate::DataError;

/// 本体を INSERT 済みの新規レコード（書込 Tx 上）。
pub(crate) struct CreatedRecord<'a> {
    pub record_int: i64,
    pub id: Uuid,
    pub owner: &'a str,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 外部形（`key → 値`）の正規化済みの値（書込検証の出力）。
    pub data: &'a Map<String, Value>,
}

impl DataStore {
    /// 新規レコードの副作用を同じ Tx で書く（索引行 → 材料化 → リビジョン）。
    ///
    /// `defer(field, table)` が真の組の索引行は書かない（一括取込の索引の遅延構築・その組は
    /// `building` で、適用後のバックフィルが本体から作る）。1 件の create は常に偽を渡す。
    #[allow(clippy::too_many_arguments)] // 書込 Tx・主体・テーブル・計画・対象・遅延・trace は全て別物。
    pub(crate) async fn after_records_created(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        table: &DataTable,
        plan: &IndexPlan,
        recs: &[CreatedRecord<'_>],
        defer: impl Fn(FieldId, IndexTable) -> bool,
        trace_id: Option<&str>,
    ) -> Result<(), DataError> {
        if recs.is_empty() {
            return Ok(());
        }
        let indexed: Vec<IndexedRecord<'_>> = recs
            .iter()
            .map(|r| IndexedRecord {
                record_int: r.record_int,
                owner: r.owner,
                created_at: r.created_at,
                updated_at: r.updated_at,
                data: r.data,
            })
            .collect();
        self.index_writer(plan, &table.schema, &ctx.tenant_id)
            .on_create_many(conn, &indexed, defer)
            .await?;
        // 材料化した派生列の値と、このテーブルを参照先にする列の再計算待ち（Task 15.4）。
        let written: Vec<(i64, &Map<String, Value>)> =
            recs.iter().map(|r| (r.record_int, r.data)).collect();
        crate::derived::materialize::after_records_written(conn, &ctx.tenant_id, table, &written)
            .await?;
        let patches: Vec<_> = recs
            .iter()
            .map(|r| diff_fields(&Map::new(), r.data))
            .collect();
        let revisions: Vec<RevisionInsert<'_>> = recs
            .iter()
            .zip(&patches)
            .map(|(r, patch)| RevisionInsert {
                table_id: table.id,
                record_id: r.id,
                rev: 1,
                change_kind: "create",
                patch,
                trace_id,
            })
            .collect();
        insert_revisions(conn, ctx, &revisions).await
    }
}
