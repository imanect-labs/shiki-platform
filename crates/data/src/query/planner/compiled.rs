//! 実行の文のコンパイル（プランキャッシュの命中で降下を省く）と検査の並び（監査 B2-1）。
//!
//! - [`Exec::compile`]: 同じ（形状, 文の種類, 計画の変種）に命中すれば、降下でテキストを組まずに
//!   バインドの値だけを集める（[`super::plan_cache::compile`]）。計画の変種は [`Exec::variant`] から積む。
//! - [`Exec::order`]: 最上位の積の検査を「(1 − 選択率) / 単価」の大きい順に並べる（query-optimizer §5.2・
//!   統計が無ければ葉の種類の静的順位）。行述語（Guard）は並べ替えず常に最後に付く（§7.1）。

use std::sync::Arc;

use super::cost::UnitCosts;
use super::exec::{Captured, Exec};
use super::lower::{check_rank, order_checks};
use super::plan_cache::{Variant, VariantKey};
use crate::query::bound::Expr;
use crate::query::sql::{bind_query, Bind, SqlBuf};
use crate::{map_db, DataError};

impl Exec<'_, '_> {
    /// 計画の変種（この文脈から始める・`kind` は文の種類）。
    pub(crate) fn variant(&self, kind: u8) -> Variant {
        Variant::new(&self.ctx_key).tag(kind)
    }

    /// 文をコンパイルする（同じ変種に命中すれば降下でテキストを組まずにバインドだけを集める・
    /// [`super::plan_cache::compile`]）。
    pub(crate) fn compile(
        &self,
        label: &'static str,
        v: &VariantKey,
        build: impl Fn(&mut SqlBuf) -> Result<String, DataError>,
    ) -> Result<(Arc<str>, Vec<Bind>), DataError> {
        super::plan_cache::compile(self.plan.as_ref(), label, Some(v), self.keys, build)
    }

    /// コンパイル済みの文を実行する（記録つき）。
    pub(crate) async fn fetch_compiled(
        &mut self,
        label: &'static str,
        text: Arc<str>,
        binds: Vec<Bind>,
    ) -> Result<Vec<sqlx::postgres::PgRow>, DataError> {
        self.stats.statements += 1;
        let rows = bind_query(sqlx::query(&text), &binds)
            .fetch_all(&mut *self.conn)
            .await
            .map_err(map_db)?;
        self.captured.push(Captured {
            label,
            sql: text.to_string(),
            binds,
        });
        Ok(rows)
    }

    /// 索引の並びで読むように書いた文（ソート駆動）を、並べ替えを選ばせずに実行する
    /// （`ordered` のとき・文の前後で `enable_sort` を切り替える）。
    ///
    /// 取込直後の古い統計（空に近いときのまま）だと、Postgres は「主キーで列の全行を拾って並べ替える」計画を
    /// 選び、10 万行で 1 秒を超えていた（検証 2-D-P2-1）。この文だけ並べ替えを罰則にして、並びを作れる値索引の
    /// 走査を選ばせる（プランナ耐性 SQL・query-optimizer.md §6.1）。他の文（最終の並び・集計・本体経路）の
    /// 計画には影響させない。
    pub(crate) async fn fetch_index_ordered(
        &mut self,
        label: &'static str,
        text: Arc<str>,
        binds: Vec<Bind>,
        ordered: bool,
    ) -> Result<Vec<sqlx::postgres::PgRow>, DataError> {
        if !ordered {
            return self.fetch_compiled(label, text, binds).await;
        }
        sqlx::query("SELECT set_config('enable_sort', 'off', true)")
            .execute(&mut *self.conn)
            .await
            .map_err(map_db)?;
        let rows = self.fetch_compiled(label, text, binds).await;
        sqlx::query("SELECT set_config('enable_sort', 'on', true)")
            .execute(&mut *self.conn)
            .await
            .map_err(map_db)?;
        rows
    }

    /// 検査の並び（query-optimizer §5.2）。統計があれば「(1 − 選択率) / 単価」の大きい順（よく落ちて安い
    /// 述語を先に・同点は静的順位）、無ければ葉の種類の静的順位。行述語（Guard）は常に最後（並べ替えない）。
    pub(crate) fn order<'e>(&self, v: Vec<&'e Expr>) -> Vec<&'e Expr> {
        let Some(stats) = self.catalog.as_deref() else {
            return order_checks(v);
        };
        let est = self.estimator(stats);
        order_by_selectivity(v, |e| est.expr(e).mid, &self.units)
    }

    /// 最上位の積の項のうち、入口で満たされない項の検査（[`Self::order`] の並び）。
    pub(crate) fn checks<'e>(&self, conj: &[&'e Expr], covered: &[usize]) -> Vec<&'e Expr> {
        self.order(
            conj.iter()
                .enumerate()
                .filter(|(i, _)| !covered.contains(i))
                .map(|(_, e)| *e)
                .collect(),
        )
    }
}

/// 「(1 − 選択率) / 単価」の大きい順（同点は葉の種類の静的順位・元の並び）。`pass` は検査の通過率（選択率）。
pub(crate) fn order_by_selectivity<'e>(
    v: Vec<&'e Expr>,
    pass: impl Fn(&Expr) -> f64,
    units: &UnitCosts,
) -> Vec<&'e Expr> {
    let mut keyed: Vec<(f64, u8, usize, &'e Expr)> = v
        .into_iter()
        .enumerate()
        .map(|(i, e)| {
            let s = pass(e).clamp(0.0, 1.0);
            let c = units.check(e).max(1e-9);
            ((1.0 - s) / c, check_rank(e), i, e)
        })
        .collect();
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    keyed.into_iter().map(|k| k.3).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::many_single_char_names)]
    use super::*;
    use crate::query::bound::{Leaf, Val};

    fn eq(f: crate::model::FieldId) -> Expr {
        Expr::atom(f, Leaf::Eq(Val::Text("x".into())))
    }

    /// 統計があれば選択率と単価で並べる（静的順位と逆になる場合を含む）・同点は静的順位。
    #[test]
    fn checks_are_ordered_by_rejection_rate_per_cost() {
        let u = UnitCosts::default();
        let status = eq(16); // 通過率 0.9（ほとんど落ちない）
        let range = Expr::atom(
            17,
            Leaf::Range {
                lo: Some((Val::Text("a".into()), true)),
                hi: None,
            },
        ); // 0.2
        let like = Expr::atom(18, Leaf::Contains("x".into())); // 0.1 だが 4 倍重い
        let rate = |e: &Expr| match e {
            Expr::Atom(a) if a.field == 16 => 0.9,
            Expr::Atom(a) if a.field == 17 => 0.2,
            _ => 0.1,
        };
        let v = vec![&status, &range, &like];
        assert_eq!(
            order_checks(v.clone()),
            vec![&status, &range, &like],
            "静的順位"
        );
        // (1−s)/c: range 0.8/5 = 0.16 > like 0.9/20 = 0.045 > status 0.1/5 = 0.02。
        assert_eq!(
            order_by_selectivity(v, rate, &u),
            vec![&range, &like, &status]
        );
        // 同点は静的順位（Eq が Range より先）→ 元の並び。
        let a = eq(20);
        let b = Expr::atom(
            21,
            Leaf::Range {
                lo: None,
                hi: Some((Val::Text("z".into()), false)),
            },
        );
        let c = eq(22);
        assert_eq!(
            order_by_selectivity(vec![&b, &c, &a], |_| 0.5, &u),
            vec![&c, &a, &b]
        );
    }
}
