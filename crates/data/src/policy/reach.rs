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

use authz::{AuthContext, FgaObject};
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
