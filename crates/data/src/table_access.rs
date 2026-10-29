//! テーブルの「追加だけ」の共有（D-113・GAP-49・OpenFGA の `data_table#submitter`）。
//!
//! 追加だけの人（submitter で viewer でない）は、
//!
//! 1. テーブルの定義を読める（読める列だけ・持ち主でない閲覧者と同じ・[`crate::DataStore::get_table_for_viewer`]）
//! 2. レコードを作れる（読めない列には書けない・作成の監査を残す・[`crate::DataStore::create_record`]）
//! 3. **自分が作ったレコードだけ**を読める。行述語があればその積で絞り、個別共有された行は今の規則どおり
//!    足す（`(作成者 = 自分 AND 行述語) OR 個別共有`）
//! 4. 書き換え・削除・状態を進める・一括取込はできない（これまでどおり editor が要る）
//!
//! # 絞り方（単一の表現）
//!
//! 読取の入口（[`DataStore::open_for_reader`] など）が、追加だけの人に渡すテーブルの**行述語そのもの**を
//! `IsOwner AND 元の行述語` に置き換える（[`narrow_to_own`]）。行述語を評価する経路（索引上の SQL・本体の
//! SQL・Rust の事後検査・SSE の購読者ごとの判定・件数・集計・リンク先の表示・ルックアップ・ロールアップ・
//! 履歴）はすべてテーブルの `row_policy` を読むので、どの経路でも同じ絞り込みになる（pre-filter と
//! post-filter の両方に同じ式が効く）。絞ったテーブルは**このリクエストの中だけ**のもので、スキーマの
//! キャッシュ・保存した定義は変えない。
//!
//! 追加だけを通す入口はここで名指しした読取だけで、それ以外（RAG の検索・SQL・保存ビュー・テーブルの
//! ReBAC を直接問う経路）は従来どおり viewer を要求する（追加だけの人には「見えない」と同じ応答）。
//!
//! # 材料化の前提は絞る前の定義で検査する
//!
//! 絞った版を材料化の前提の検査（[`crate::derived`]）にそのまま渡すと、行述語が付いたように見えて、閲覧者
//! 1 人の読取で全員の材料化列を降格させてしまう。絞る前の行述語を [`Narrowed`] に残し、検査はそれで行う。

use authz::{AuthContext, Relation};
use storage::audit::AuditAction;
use uuid::Uuid;

use crate::model::DataTable;
use crate::policy::ast::PolicyExpr;
use crate::policy::memo::AuthzMemo;
use crate::policy::RowPolicy;
use crate::store::DataStore;
use crate::DataError;

/// 追加だけの閲覧者向けに絞る前の行述語（[`narrow_to_own`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Narrowed {
    pub row_policy: Option<RowPolicy>,
}

/// 第 1 層（テーブルの ReBAC）で決まる読取の範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadScope {
    /// テーブルの viewer: 行述語・個別共有のとおり。
    All,
    /// 追加だけ（submitter で viewer でない）: 自分が作ったレコードだけ（行述語との積・個別共有は足す）。
    OwnRecords,
}

/// テーブルを追加だけの閲覧者の見え方にする（冪等）。
///
/// 読取の行述語を `IsOwner AND 元の read`（行述語が無ければ `IsOwner`）に、書込の行述語を
/// `IsOwner AND 元の write` に置き換える。書込は table editor が別に要るので、ここは念のための絞り込み。
pub(crate) fn narrow_to_own(table: &mut DataTable) {
    if table.narrowed.is_some() {
        return;
    }
    let original = table.schema.row_policy.take();
    let own = |e: Option<&PolicyExpr>| match e {
        None => PolicyExpr::IsOwner,
        Some(e) => PolicyExpr::All(vec![PolicyExpr::IsOwner, e.clone()]),
    };
    table.schema.row_policy = Some(RowPolicy {
        read: own(original.as_ref().map(|p| &p.read)),
        write: Some(own(original.as_ref().map(RowPolicy::write_expr))),
    });
    table.narrowed = Some(Narrowed {
        row_policy: original,
    });
}

/// 絞る前のスキーマ（材料化の前提の検査用）。絞っていなければそのまま。
pub(crate) fn unnarrowed_schema(table: &DataTable) -> crate::model::TableSchema {
    let mut schema = table.schema.clone();
    if let Some(n) = &table.narrowed {
        schema.row_policy.clone_from(&n.row_policy);
    }
    schema
}

impl DataStore {
    /// 読取の範囲（viewer → 全行の規則・追加だけ → 自分の行・どちらでもない → `None`）。
    ///
    /// viewer と submitter を 1 往復の BatchCheck で問う（リクエストのメモ・天井があれば天井との積）。
    pub(crate) async fn read_scope(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table_id: Uuid,
    ) -> Result<Option<ReadScope>, DataError> {
        let obj = ctx.ns().data_table(&table_id.to_string());
        let got = az
            .check_all(
                ctx,
                &[(Relation::Viewer, obj.clone()), (Relation::Submitter, obj)],
            )
            .await?;
        Ok(match (got.first().copied(), got.get(1).copied()) {
            (Some(true), _) => Some(ReadScope::All),
            (_, Some(true)) => Some(ReadScope::OwnRecords),
            _ => None,
        })
    }

    /// 読取の入口（追加だけも通す）: viewer なら [`Self::open_for_read`] と同じテーブルを、追加だけなら
    /// 自分が作ったレコードだけに絞ったテーブル（[`narrow_to_own`]）を返す。どちらでもなければ
    /// [`Self::open_for_read`] と同じ拒否（監査に deny・存在を明かさない 404）。
    pub(crate) async fn open_for_reader(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table_id: Uuid,
        action: AuditAction,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        let peeked = self.peek_live(ctx, table_id).await?;
        let obj = ctx.ns().data_table(&table_id.to_string());
        let mut items = vec![(Relation::Viewer, obj.clone()), (Relation::Submitter, obj)];
        if let Some(t) = &peeked {
            self.read_prefetch(ctx, t, &mut items).await?;
        }
        az.check_all(ctx, &items).await?;
        let Some(scope) = self.read_scope(ctx, az, table_id).await? else {
            // viewer でも追加だけでもない: open_for_read と同じ拒否（deny の監査・存在を明かさない 404）。
            self.require_in(az, ctx, table_id, Relation::Viewer, action, trace_id)
                .await?;
            return Err(DataError::NotFound);
        };
        let mut table = match peeked {
            Some(t) => t,
            None => self.fetch_live(ctx, table_id).await?,
        };
        if scope == ReadScope::OwnRecords {
            narrow_to_own(&mut table);
        }
        Ok(table)
    }

    /// 開いたテーブルを読み直す（スキーマの改訂・降格の後）。前のテーブルが追加だけの見え方なら、読み直した
    /// テーブルも同じに絞る（絞り込みを読み直しで落とさない）。
    pub(crate) async fn reopen_like(
        &self,
        ctx: &AuthContext,
        prev: &DataTable,
    ) -> Result<DataTable, DataError> {
        let mut table = self.fetch_live(ctx, prev.id).await?;
        if prev.narrowed.is_some() {
            narrow_to_own(&mut table);
        }
        Ok(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{TableKeys, TableSchema};
    use crate::policy::ast::{CmpOp, PolicyOperand};

    fn table(row_policy: Option<RowPolicy>) -> DataTable {
        let ts = chrono::DateTime::from_timestamp(0, 0).unwrap_or_default();
        DataTable {
            id: Uuid::nil(),
            name: "t".into(),
            app_id: None,
            schema: TableSchema {
                fields: vec![],
                status_field: None,
                row_policy,
                field_policy: vec![],
                aggregate_min_rows: None,
                fsm_ref: None,
                rag: None,
            },
            schema_version: 1,
            created_by: "u".into(),
            created_at: ts,
            updated_at: ts,
            add_only: false,
            narrowed: None,
            keys: TableKeys::default(),
            has_record_shares: false,
            planner_fixed: false,
        }
    }

    fn dept_eq() -> PolicyExpr {
        PolicyExpr::FieldCmp {
            field: "dept".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::Lit(serde_json::json!("営業")),
        }
    }

    #[test]
    fn narrowing_without_policy_is_owner_only() {
        let mut t = table(None);
        narrow_to_own(&mut t);
        let p = t.schema.row_policy.clone().expect("絞った行述語");
        assert_eq!(p.read, PolicyExpr::IsOwner);
        assert_eq!(p.write_expr(), &PolicyExpr::IsOwner);
        assert_eq!(t.narrowed, Some(Narrowed { row_policy: None }));
        assert_eq!(
            unnarrowed_schema(&t).row_policy,
            None,
            "検査は絞る前の定義で"
        );
    }

    #[test]
    fn narrowing_keeps_the_policy_as_a_conjunct_and_is_idempotent() {
        let original = RowPolicy {
            read: dept_eq(),
            write: Some(PolicyExpr::Public),
        };
        let mut t = table(Some(original.clone()));
        narrow_to_own(&mut t);
        narrow_to_own(&mut t);
        let p = t.schema.row_policy.clone().expect("絞った行述語");
        assert_eq!(
            p.read,
            PolicyExpr::All(vec![PolicyExpr::IsOwner, dept_eq()])
        );
        assert_eq!(
            p.write_expr(),
            &PolicyExpr::All(vec![PolicyExpr::IsOwner, PolicyExpr::Public])
        );
        assert_eq!(unnarrowed_schema(&t).row_policy, Some(original));
    }
}
