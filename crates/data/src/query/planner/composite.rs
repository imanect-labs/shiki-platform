//! 複合索引駆動（`CompositeDrive`・query-optimizer.md §5.1・data-platform §5.3）。
//!
//! 材料化済み（active）の複合索引 `data_index_composite` を、ソートの組み合わせの全順序のまま
//! （符号化したキーの昇順）歩く。第 1 キーの同値群の大きさに依らず何ページ目でも同じコストになる。
//! 形はソート駆動と同じ（予算付き・`pass` / `is_last` / `pos` を 1 往復で返す §6.1）で、各行に
//! 行述語の検査（[`GuardSql`]）が付く。カーソルは戦略に依存しない位置（[`Pos`]）を符号化して続きを決める。

use std::fmt::Write as _;

use sqlx::Row;

use super::exec::{CompositeSet, Exec, SortState, Step};
use super::guard::GuardSql;
use super::keys::{encode_pos, Pos, SortVal};
use super::lower::Lowerer;
use crate::query::bound::Expr;
use crate::query::sql::{Bind, SqlBuf};
use crate::{map_db, DataError};

impl Lowerer<'_> {
    /// 複合索引を予算付きで歩く文。列: `record_int`, `cv`（符号化キー）, `pos`, `pass`, `is_last`, `k0..`。
    #[allow(clippy::too_many_arguments)] // 1 文の部品（組み合わせ・位置・検査・Guard・予算・件数）を明示で渡す。
    pub(crate) fn composite_drive_sql(
        &self,
        set_id: i16,
        after: Option<&[u8]>,
        checks: &[&Expr],
        guard: &GuardSql,
        budget: i64,
        limit: i64,
        sql: &mut SqlBuf,
    ) -> String {
        let sph = sql.typed(Bind::I16(set_id));
        let keyset = after.map_or_else(String::new, |a| {
            let ph = sql.typed(Bind::Bytes(a.to_vec()));
            format!(" AND i.val > {ph}")
        });
        let bph = sql.typed(Bind::I64(budget.max(1)));
        let check = self.case_chain(checks, Some(guard), "d.record_int", sql);
        // 外側の別名は `w`（ソート値の相関副問合せは内側で `s` を使う）。
        let mut cols = String::new();
        for (i, k) in self.q.sort.iter().enumerate() {
            let v = self.sort_value_sql(*k, "w.record_int", sql);
            let _ = write!(cols, ", {v}::text AS k{i}");
        }
        let lph = sql.typed(Bind::I64(limit));
        format!(
            "SELECT w.record_int, w.cv, w.pos, w.pass, w.is_last{cols} FROM ( \
               SELECT d.record_int, d.val AS cv, row_number() OVER w AS pos, \
                      lead(d.record_int) OVER w IS NULL AS is_last, {check} AS pass \
               FROM (SELECT i.record_int, i.val FROM data_index_composite i \
                     WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.sort_set_id = {sph}{keyset} \
                     ORDER BY i.val LIMIT {bph}) d \
               WINDOW w AS (ORDER BY d.val) \
             ) w WHERE w.pass OR w.is_last LIMIT {lph}"
        )
    }
}

impl Exec<'_, '_> {
    /// 複合索引駆動を 1 回（予算 1 つ分）進める。
    pub(super) async fn composite_step(
        &mut self,
        checks: &[&Expr],
        st: &mut SortState,
        limit: i64,
        budget: i64,
        comp: &CompositeSet,
    ) -> Result<Step, DataError> {
        if !st.comp_started {
            st.comp_started = true;
            st.comp_after = st.after.as_ref().map(|p| encode_pos(&comp.1, p));
        }
        let need = limit + 1 - st.collected.len() as i64;
        if need <= 0 || st.done {
            st.done = true;
            return Ok(Step::Done);
        }
        let mut sql = self.sql();
        let guard = self.guard.lower("d.record_int", &mut sql)?;
        let after = st.comp_after.clone();
        let text = self.low.composite_drive_sql(
            comp.0,
            after.as_deref(),
            checks,
            &guard,
            budget,
            need + 1,
            &mut sql,
        );
        let rows = self.fetch("composite_drive", text, sql.binds).await?;
        let mut last_pos = 0i64;
        let mut last_is_end = false;
        let mut passed = 0u64;
        for r in &rows {
            let rint: i64 = r.try_get("record_int").map_err(map_db)?;
            let cv: Vec<u8> = r.try_get("cv").map_err(map_db)?;
            let pos: i64 = r.try_get("pos").map_err(map_db)?;
            let pass: bool = r.try_get("pass").map_err(map_db)?;
            let is_last: bool = r.try_get("is_last").map_err(map_db)?;
            last_pos = pos;
            last_is_end = is_last;
            st.comp_after = Some(cv);
            if pass {
                passed += 1;
                let mut vals = Vec::with_capacity(comp.1.len());
                for (i, (kind, _)) in comp.1.iter().enumerate() {
                    let v: Option<String> = r.try_get(format!("k{i}").as_str()).map_err(map_db)?;
                    vals.push(SortVal::from_db(*kind, v));
                }
                st.collected.push((rint, Pos { vals, rint }));
            }
        }
        let scanned = last_pos.max(0) as u64;
        self.stats.scanned += scanned;
        self.stats.strategy = "composite";
        let end = rows.is_empty() || (last_is_end && last_pos < budget);
        if st.collected.len() as i64 > limit || end {
            st.done = true;
            return Ok(Step::Done);
        }
        Ok(Step::Exhausted { scanned, passed })
    }
}
