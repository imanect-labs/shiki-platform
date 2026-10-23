//! 残した結合（INNER JOIN）の降下と実行（Task 15.10・data-platform §16.1 の JOIN の束縛規則・§16.2）。
//!
//! 半結合へ書き換えられなかった結合（結合先の列を射影・集計に使う／多対一以外で行が重複し得る）を
//! **結合の組（タプル）**として作る。組は「FROM の行（行述語・条件・結合先の存在を検査済み）」から
//! `data_link` を結合元の `table_int` で辿り（パーティション枝刈り・逆方向も同じ形）、結合先の生存・条件・
//! **結合先の行述語（閲覧者本人の材料・`Guard(P)`）**を通った行だけを足していく。参照先の行述語を省いた
//! 組は作れない（[`super::lower::Lowerer::link_sql`] と同じ降下）。
//!
//! - 集計（[`Lowerer::joined_aggregate_sql`]）: 候補 → 検査 → 組 → 列ごとの点検索 → GROUP BY。
//! - 行（[`Lowerer::expand_sql`]）: ページの FROM の行（`record_int` の配列）から組を作る。各段は
//!   「結合元の並び・リンクの並び（`ord`）」で並べ、上限 `cap` で切る（各段の行は必ず完全な組へ伸びるので、
//!   先頭 `cap` 件の組は各段の先頭 `cap` 件から作れる）。

use sqlx::Row;

use super::exec::Exec;
use super::lower::Lowerer;
use super::plans::{checks_for, conjuncts};
use crate::policy::compile_index::field_i16;
use crate::query::bound::{ColRef, Expr};
use crate::query::ir::CountResult;
use crate::query::sql::{Bind, SqlBuf};
use crate::{map_db, DataError};

impl Lowerer<'_> {
    /// 結合の各段の CTE（`t{i}` の列 `r0..r{i}`・`ordered` なら `o0..o{i}`）。`tph` は結合先ごとの
    /// `table_int` のプレースホルダ。返り値は CTE の定義の並び。
    fn join_ctes(
        &self,
        tph: &[String],
        base_t: &str,
        ordered: bool,
        cap: Option<&str>,
        sql: &mut SqlBuf,
    ) -> Vec<String> {
        let mut out = Vec::with_capacity(self.q.joins.len());
        for (i, j) in self.q.joins.iter().enumerate() {
            let k = i + 1;
            let Some(sc) = self.scopes.get(j.scope) else {
                // 結合先の材料が無い組は作らない（fail-closed）。
                out.push(format!(
                    "t{k} AS MATERIALIZED (SELECT t.*, NULL::int8 AS r{k}{} FROM t{i} t WHERE FALSE)",
                    if ordered {
                        format!(", NULL::int8 AS o{k}")
                    } else {
                        String::new()
                    }
                ));
                continue;
            };
            let parent_t = if j.from == 0 {
                base_t.to_string()
            } else {
                tph[j.from - 1].clone()
            };
            let ct = tph[i].clone();
            let fph = sql.typed(Bind::I16(field_i16(j.field)));
            let child = Lowerer {
                q: sc.q,
                links: self.links,
                scopes: self.scopes,
            };
            let (cond, guard) = sql.within(&ct, |sql| {
                let cond = child.expr_sql(&j.filter, "jl.dst_record", sql);
                let guard = sc
                    .guard
                    .lower("jl.dst_record", sql)
                    .map_or_else(|_| "FALSE".to_string(), |g| g.as_str().to_string());
                (cond, guard)
            });
            let (ocol, order) = if ordered {
                let prev: Vec<String> = (0..k).map(|x| format!("t.o{x}")).collect();
                let lim = cap.map_or_else(String::new, |c| format!(" LIMIT {c}"));
                (
                    format!(", jl.ord::int8 AS o{k}"),
                    format!(" ORDER BY {}, jl.ord{lim}", prev.join(", ")),
                )
            } else {
                (String::new(), String::new())
            };
            out.push(format!(
                "t{k} AS MATERIALIZED (SELECT t.*, jl.dst_record AS r{k}{ocol} FROM t{i} t \
                 JOIN data_link jl ON jl.tenant_int = $1 AND jl.table_int = {parent_t} \
                 AND jl.field_id = {fph} AND jl.src_record = t.r{from} AND jl.dst_table_int = {ct} \
                 WHERE EXISTS (SELECT 1 FROM data_record jd WHERE jd.tenant_int = $1 \
                 AND jd.table_int = {ct} AND jd.record_int = jl.dst_record) \
                 AND {cond} AND ({guard}){order})",
                from = j.from,
            ));
        }
        out
    }

    /// 結合先ごとの `table_int` のバインド。
    fn join_tables(&self, sql: &mut SqlBuf) -> Vec<String> {
        self.q
            .joins
            .iter()
            .map(|j| {
                let ti = self.scopes.get(j.scope).map_or(0, |s| s.table_int);
                sql.typed(Bind::I64(ti))
            })
            .collect()
    }

    /// 結合を残した集計（列は [`Self::aggregate_sql`] と同じ: `grp_count`, `g0..`, `m0..`）。
    pub(crate) fn joined_aggregate_sql(
        &self,
        src: &super::lower::Source<'_>,
        checks: &[&Expr],
        guard: &super::guard::GuardSql,
        sql: &mut SqlBuf,
    ) -> String {
        let source = self.source_sql(src, sql);
        let check = self.case_chain(checks, Some(guard), "c.record_int", sql);
        let t = sql.t();
        let tph = self.join_tables(sql);
        let ctes = self.join_ctes(&tph, &t, false, None, sql);
        let last = self.q.joins.len();
        let at = |c: ColRef| -> (String, String) {
            let table = if c.src == 0 {
                t.clone()
            } else {
                tph.get(c.src - 1).cloned().unwrap_or_else(|| t.clone())
            };
            (table, format!("z0.r{}", c.src))
        };
        let (inner, select, group) = self.agg_columns(&at, sql);
        format!(
            "WITH cand AS MATERIALIZED ({source}), \
             pass AS MATERIALIZED (SELECT c.record_int FROM cand c WHERE {check}), \
             t0 AS MATERIALIZED (SELECT p.record_int AS r0 FROM pass p), {} \
             SELECT {} FROM (SELECT {} FROM t{last} z0) z{group}",
            ctes.join(", "),
            select.join(", "),
            inner.join(", ")
        )
    }

    /// 結合を残した行の件数（組の数・`cap + 1` で打ち切る）。
    pub(crate) fn joined_count_sql(
        &self,
        src: &super::lower::Source<'_>,
        checks: &[&Expr],
        guard: &super::guard::GuardSql,
        cap: i64,
        sql: &mut SqlBuf,
    ) -> String {
        let source = self.source_sql(src, sql);
        let check = self.case_chain(checks, Some(guard), "c.record_int", sql);
        let t = sql.t();
        let tph = self.join_tables(sql);
        let ctes = self.join_ctes(&tph, &t, false, None, sql);
        let last = self.q.joins.len();
        let ph = sql.typed(Bind::I64(cap + 1));
        format!(
            "WITH cand AS MATERIALIZED ({source}), \
             pass AS MATERIALIZED (SELECT c.record_int FROM cand c WHERE {check}), \
             t0 AS MATERIALIZED (SELECT p.record_int AS r0 FROM pass p), {} \
             SELECT count(*) FROM (SELECT 1 FROM t{last} LIMIT {ph}) s",
            ctes.join(", ")
        )
    }

    /// ページの FROM の行（`ids` の順）から結合の組を作る（列 `r0..`・先頭 `cap` 件）。
    pub(crate) fn expand_sql(&self, ids: Vec<i64>, cap: i64, sql: &mut SqlBuf) -> String {
        let t = sql.t();
        let iph = sql.push(Bind::I64Array(ids));
        let cph = sql.typed(Bind::I64(cap));
        let tph = self.join_tables(sql);
        let ctes = self.join_ctes(&tph, &t, true, Some(&cph), sql);
        let n = self.q.joins.len();
        let cols: Vec<String> = (0..=n).map(|k| format!("r{k}")).collect();
        let order: Vec<String> = (0..=n).map(|k| format!("o{k}")).collect();
        format!(
            "WITH t0 AS MATERIALIZED (SELECT u.r AS r0, u.o AS o0 FROM unnest({iph}::int8[]) \
             WITH ORDINALITY AS u(r, o)), {} SELECT {} FROM t{n} ORDER BY {} LIMIT {cph}",
            ctes.join(", "),
            cols.join(", "),
            order.join(", ")
        )
    }
}

impl<'a> Exec<'a, '_> {
    /// 結合を残した集計の行。
    pub(crate) async fn joined_aggregate(
        &mut self,
        filter: &'a Expr,
    ) -> Result<Vec<sqlx::postgres::PgRow>, DataError> {
        let conj = conjuncts(filter);
        let (src, covered) = self.scan_source(&conj).await?;
        let checks = checks_for(&conj, &covered);
        let mut sql = self.sql();
        let guard = self.guard.lower("c.record_int", &mut sql)?;
        let src = self.materialize(&src, &mut sql);
        let text = self
            .low
            .joined_aggregate_sql(&src, &checks, &guard, &mut sql);
        self.fetch("join_aggregate", text, sql.binds).await
    }

    /// 結合を残した行の上限付き件数（組の数）。
    pub(crate) async fn joined_count(
        &mut self,
        filter: &'a Expr,
        cap: u64,
    ) -> Result<CountResult, DataError> {
        if *filter == Expr::False || self.guard.is_empty() {
            return Ok(CountResult::Exact(0));
        }
        let conj = conjuncts(filter);
        let (src, covered) = self.scan_source(&conj).await?;
        let checks = checks_for(&conj, &covered);
        let mut sql = self.sql();
        let guard = self.guard.lower("c.record_int", &mut sql)?;
        let src = self.materialize(&src, &mut sql);
        let cap_i = i64::try_from(cap).unwrap_or(i64::MAX - 1);
        let text = self
            .low
            .joined_count_sql(&src, &checks, &guard, cap_i, &mut sql);
        self.stats.count_issued = true;
        let rows = self.fetch("join_count", text, sql.binds).await?;
        let n: i64 = rows
            .first()
            .map(|r| r.try_get::<i64, _>(0))
            .transpose()
            .map_err(map_db)?
            .unwrap_or(0);
        let n = n.max(0) as u64;
        Ok(if n > cap {
            CountResult::AtLeast(cap)
        } else {
            CountResult::Exact(n)
        })
    }

    /// ページの FROM の行から結合の組（各組は `r0..` の `record_int`）を作る。
    pub(crate) async fn expand(
        &mut self,
        ids: &[i64],
        cap: i64,
    ) -> Result<Vec<Vec<i64>>, DataError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut sql = self.sql();
        let text = self.low.expand_sql(ids.to_vec(), cap.max(1), &mut sql);
        let rows = self.fetch("join_expand", text, sql.binds).await?;
        let n = self.low.q.joins.len();
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut tuple = Vec::with_capacity(n + 1);
            for k in 0..=n {
                tuple.push(
                    r.try_get::<i64, _>(format!("r{k}").as_str())
                        .map_err(map_db)?,
                );
            }
            out.push(tuple);
        }
        Ok(out)
    }
}
