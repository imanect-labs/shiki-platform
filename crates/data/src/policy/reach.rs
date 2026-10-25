//! 到達してよいテーブルの天井（ワークフローの委譲・ミニアプリの所有テーブル束縛）。
//!
//! data は通常、閲覧者本人の ReBAC だけで参照先テーブル（結合・半結合・lookup / rollup・リンク値の
//! 表示名・対称列）を開く。ところが委譲で動く主体（schedule / event run のワークフロー）や、所有
//! テーブルに束縛されたミニアプリは、**本人が読めても辿ってはいけない**テーブルを持つ
//! （engine.md §6.8「委譲していないテーブルは、委譲元が読めても読めない」）。入口のテーブルだけを
//! 確かめても、リンクの先で本人の権限を丸ごと使えてしまう。
//!
//! [`TableReach`] はこの天井を表す。[`crate::DataStore::within_reach`] で付けたストアは、リクエストの
//! 認可メモ（[`super::memo::AuthzMemo`]）が `data_table` への問いを**天井と本人の ReBAC の積**で
//! 答える。参照先の可否はすべてメモの問いを通るので、天井の外のテーブルは「本人が viewer でない」の
//! と同じ扱いになる（結合・半結合は 403・lookup / rollup は null・リンク値は落とす）。応答は本人が
//! 読めないテーブルと区別できない（存在オラクルを作らない）。
//!
//! 天井は認可材料ではなく呼出側（ワークフローの実行・ゲートウェイ）が毎回組む入力で、リクエストを
//! 跨いで持ち越さない（PIT-18）。

use std::collections::HashSet;

use authz::{AuthContext, FgaObject, PrincipalKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 到達してよいテーブルの天井（[モジュール文書](self)）。
#[derive(Debug, Clone)]
pub struct TableReach {
    /// 到達してよいテーブル（入口のテーブルも含める）。
    tables: HashSet<Uuid>,
    /// 併せて同じ relation を持つべき主体（ワークフロー主体の委譲タプル。棚卸しの撤去を即時に効かせる）。
    co_principal: Option<AuthContext>,
}

impl TableReach {
    /// `tables` だけに到達してよい天井。
    pub fn only(tables: impl IntoIterator<Item = Uuid>) -> Self {
        TableReach {
            tables: tables.into_iter().collect(),
            co_principal: None,
        }
    }

    /// 天井のテーブルに、`co` も同じ relation を持つことを併せて要求する（委譲タプルの確認）。
    #[must_use]
    pub fn also_held_by(mut self, co: AuthContext) -> Self {
        self.co_principal = Some(co);
        self
    }

    /// 天井に入るテーブルか（主体の ReBAC は別に問う）。
    pub(crate) fn admits(&self, table_id: Uuid) -> bool {
        self.tables.contains(&table_id)
    }

    /// 併せて relation を持つべき主体（同じテナントのときだけ。違えば天井は何も通さない）。
    pub(crate) fn co_principal(&self) -> Option<&AuthContext> {
        self.co_principal.as_ref()
    }
}

/// 保存形（一括取込の `data_bulk_import.reach`・ジョブのワーカーが復元する）。
#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    tables: Vec<Uuid>,
    co: Option<StoredPrincipal>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredPrincipal {
    kind: PrincipalKind,
    id: String,
}

impl TableReach {
    /// 保存形へ（テーブルは並びを固定する）。
    pub(crate) fn to_stored(&self) -> serde_json::Value {
        let mut tables: Vec<Uuid> = self.tables.iter().copied().collect();
        tables.sort_unstable();
        let co = self.co_principal.as_ref().map(|c| StoredPrincipal {
            kind: c.principal.kind,
            id: c.principal.id.clone(),
        });
        serde_json::to_value(Stored { tables, co }).unwrap_or(serde_json::Value::Null)
    }

    /// 保存形から復元する（併せて要る主体は取込のテナント・org で組み直す）。壊れた保存形は
    /// 何も通さない天井にする（fail-closed）。
    pub(crate) fn from_stored(v: &serde_json::Value, tenant_id: &str, org: &str) -> Self {
        let Ok(st) = serde_json::from_value::<Stored>(v.clone()) else {
            return TableReach::only([]);
        };
        let reach = TableReach::only(st.tables);
        match st.co {
            Some(p) => reach.also_held_by(AuthContext::new(
                authz::Principal {
                    kind: p.kind,
                    id: p.id,
                    email: None,
                    groups: Vec::new(),
                    roles: Vec::new(),
                    tenant_id: Some(tenant_id.to_string()),
                },
                org.to_string(),
                tenant_id.to_string(),
            )),
            None => reach,
        }
    }
}

/// `data_table` の object なら、そのテーブル ID（`ctx` のテナントのものだけ）。
pub(crate) fn data_table_id(ctx: &AuthContext, object: &FgaObject) -> Option<Uuid> {
    let rest = object.as_str().strip_prefix("data_table:")?;
    let local = ctx.ns().strip_object_id(rest)?;
    Uuid::parse_str(local).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use authz::{Principal, PrincipalKind};

    fn ctx(tenant: &str) -> AuthContext {
        AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
                id: "u1".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(tenant.into()),
            },
            tenant.into(),
            tenant.into(),
        )
    }

    #[test]
    fn stored_form_round_trips_and_broken_form_admits_nothing() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut wf = ctx("t1");
        wf.principal.kind = PrincipalKind::Workflow;
        wf.principal.id = "wf1".into();
        let reach = TableReach::only([a, b]).also_held_by(wf);
        let back = TableReach::from_stored(&reach.to_stored(), "t1", "t1");
        assert!(back.admits(a) && back.admits(b));
        let co = back.co_principal().unwrap();
        assert_eq!(co.principal.kind, PrincipalKind::Workflow);
        assert_eq!(co.principal.id, "wf1");
        assert_eq!(co.tenant_id, "t1");
        let only = TableReach::from_stored(&TableReach::only([a]).to_stored(), "t1", "t1");
        assert!(only.admits(a) && !only.admits(b) && only.co_principal().is_none());
        let broken = TableReach::from_stored(&serde_json::json!({"x": 1}), "t1", "t1");
        assert!(!broken.admits(a));
    }

    #[test]
    fn data_table_id_reads_only_own_tenant_tables() {
        let a = ctx("t1");
        let id = Uuid::new_v4();
        assert_eq!(
            data_table_id(&a, &a.ns().data_table(&id.to_string())),
            Some(id)
        );
        // 他テナントの object・他の型は天井の判定に掛けない（None）。
        let b = ctx("t2");
        assert_eq!(data_table_id(&a, &b.ns().data_table(&id.to_string())), None);
        assert_eq!(
            data_table_id(&a, &a.ns().data_record(&id.to_string())),
            None
        );
        assert_eq!(data_table_id(&a, &a.ns().role("mgr")), None);
    }
}
