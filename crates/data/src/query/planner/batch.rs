//! バッチ駆動（O3・query-optimizer.md §6.4）と Rust 側の集合演算（Roaring ビットマップ）。
//!
//! ```text
//! 段 1: 駆動（ソートキーの値索引・値なしの区間は擬似列 created_at）から record_int を最大 B 件取る
//! 段 2: 条件の全原子式と行述語を `record_int = ANY($ids)` で一括検査する（1 文に UNION ALL で並べる）
//!       → 原子式ごとの通過集合を Roaring ビットマップに入れ、And = 積・Or = 和・Not = 差で条件木を評価
//! 段 3: 足りなければ段 1 の続きから
//! ```
//!
//! - 配列長は [`BATCH_ARRAY_CAP`]（既定 1,000）まで。超えるものは段を分ける。点検索になることは
//!   EXPLAIN 回帰（tests/query_plan_it.rs・配列長 1 / 100 / 上限 × `plan_cache_mode`）で確かめる。
//! - 行述語は**段ごとに全件へ同じ形で検査する**（`Guard(P)` を省かない・§7.1）。
//! - 単発 SQL が使える形では単発 SQL を優先する（往復 1 回）。バッチ駆動は自動選択では選ばず、
//!   リンクを辿る解決（ルックアップ・対称列・[`crate::derived`]）と強制戦略の検証で使う。
//! - 項の多い OR 駆動（[`UNION_BITMAP_TERMS`] 超）は、各項の範囲を 1 文の `UNION ALL` で取り、
//!   Roaring の和で合わせてから候補にする（往復を増やさない）。

use roaring::RoaringTreemap;
use sqlx::Row;

use super::exec::{Exec, PageOut, K_PREFETCH};
use super::guard::GuardSql;
use super::keys::{Pos, SortVal};
use super::lower::{sort_val_ph, Lowerer};
use crate::model::SYSTEM_FIELD_CREATED_AT;
use crate::policy::compile_index::field_i16;
use crate::query::bound::{Atom, BSort, Expr, Leaf};
use crate::query::sql::{Bind, SqlBuf};
use crate::{map_db, DataError};

/// 1 段の `record_int` の配列長の上限（§6.4）。
pub(crate) const BATCH_ARRAY_CAP: usize = 1_000;
/// OR 駆動でビットマップに切り替える項数（これを超えたら Rust 側で合わせる・§5.1）。
pub(crate) const UNION_BITMAP_TERMS: usize = 3;

/// 正の `record_int` をビットマップのキーへ（負は存在しない・採番は正）。
fn key(r: i64) -> Option<u64> {
    u64::try_from(r).ok()
}

/// 配列長の上限で区切る（段を分ける）。
pub(crate) fn stages<T>(v: &[T]) -> std::slice::Chunks<'_, T> {
    v.chunks(BATCH_ARRAY_CAP)
}

/// `record_int` の集合（Roaring）。
pub(crate) fn bitmap(ints: impl IntoIterator<Item = i64>) -> RoaringTreemap {
    ints.into_iter().filter_map(key).collect()
}

impl Lowerer<'_> {
    /// 段 1: 駆動の索引を位置の後ろから `b` 件（列 `record_int`, `v`）。区間 2 は擬似列 created_at を
    /// `record_int` 順に歩く（値なしの検査は段 2 の条件に足す）。
    pub(crate) fn batch_drive_sql(
        &self,
        key: BSort,
        segment: u8,
        start: Option<(&SortVal, i64)>,
        b: i64,
        sql: &mut SqlBuf,
    ) -> String {
        let dir = if key.desc { "DESC" } else { "ASC" };
        let op = if key.desc { "<" } else { ">" };
        if segment == 2 {
            let cph = sql.typed(Bind::I16(field_i16(SYSTEM_FIELD_CREATED_AT)));
            let keyset = start.map_or_else(String::new, |(_, r)| {
                let ph = sql.typed(Bind::I64(r));
                format!(" AND i.record_int {op} {ph}")
            });
            let bph = sql.typed(Bind::I64(b));
            return format!(
                "SELECT i.record_int, NULL::text AS v FROM data_index_text i \
                 WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = {cph}{keyset} \
                 ORDER BY i.record_int {dir} LIMIT {bph}"
            );
        }
        let f = self.q.field(key.field);
        let rel = f.value_table().relation();
        let fph = sql.typed(Bind::I16(field_i16(f.id)));
        let keyset = start.map_or_else(String::new, |(v, r)| {
            let vph = sort_val_ph(f.kind, v, sql);
            let rph = sql.typed(Bind::I64(r));
            format!(" AND (i.val, i.record_int) {op} ({vph}, {rph})")
        });
        let bph = sql.typed(Bind::I64(b));
        format!(
            "SELECT i.record_int, i.val::text AS v FROM {rel} i \
             WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = {fph}{keyset} \
             ORDER BY i.val {dir}, i.record_int {dir} LIMIT {bph}"
        )
    }

    /// 段 2: 原子式ごとに `record_int = ANY($ids)` の一括検査（列 `k` = 原子式の番号, `r`）と、
    /// 行述語を通る id（`k = -1`）。`guard` は `u.r` について降ろしたもの。
    pub(crate) fn batch_check_sql(
        &self,
        atoms: &[&Atom],
        ids_ph: &str,
        guard: &GuardSql,
        sql: &mut SqlBuf,
    ) -> String {
        let mut parts = Vec::with_capacity(atoms.len() + 1);
        for (i, a) in atoms.iter().enumerate() {
            let table = self.atom_table(a);
            let rel = table.relation();
            let col = table.record_column();
            let fph = sql.typed(Bind::I16(field_i16(a.field)));
            let cond = self.leaf_cond(a, "x", sql);
            parts.push(format!(
                "SELECT {i}::int4 AS k, x.{col} AS r FROM {rel} x \
                 WHERE x.tenant_int = $1 AND x.table_int = $2 AND x.field_id = {fph} \
                   AND x.{col} = ANY({ids_ph}::int8[]) AND {cond}"
            ));
        }
        parts.push(format!(
            "SELECT -1::int4 AS k, u.r FROM unnest({ids_ph}::int8[]) AS u(r) WHERE {}",
            guard.as_str()
        ));
        parts.join(" UNION ALL ")
    }

    /// 項の多い OR 駆動: 各項の範囲（上限 `cap + 1` 件ずつ・列 `k`, `r`）を 1 文で。
    pub(crate) fn union_terms_sql(&self, atoms: &[&Atom], cap: i64, sql: &mut SqlBuf) -> String {
        let lim = sql.typed(Bind::I64(cap.saturating_add(1)));
        atoms
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let src = self.atom_source(a, sql);
                format!("(SELECT {i}::int4 AS k, s.record_int AS r FROM ({src} LIMIT {lim}) s)")
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ")
    }
}

/// 条件木を原子式の通過集合から評価する（2 値論理: Not は全体からの差）。
fn eval(
    e: &Expr,
    atoms: &[&Atom],
    sets: &[RoaringTreemap],
    all: &RoaringTreemap,
) -> RoaringTreemap {
    match e {
        Expr::True => all.clone(),
        Expr::False => RoaringTreemap::new(),
        Expr::Atom(a) => atoms
            .iter()
            .position(|x| std::ptr::eq(*x, a))
            .map(|i| &sets[i] & all)
            .unwrap_or_default(),
        Expr::Not(x) => all - eval(x, atoms, sets, all),
        Expr::And(v) => {
            let mut acc = all.clone();
            for x in v {
                acc &= eval(x, atoms, sets, &acc);
                if acc.is_empty() {
                    break;
                }
            }
            acc
        }
        Expr::Or(v) => {
            let mut acc = RoaringTreemap::new();
            for x in v {
                acc |= eval(x, atoms, sets, all);
            }
            acc
        }
    }
}

impl<'a> Exec<'a, '_> {
    /// 段 2: `ids` のうち条件（`checks` の積）と行述語を満たすもの。
    async fn batch_pass(
        &mut self,
        checks: &[&Expr],
        ids: &[i64],
    ) -> Result<RoaringTreemap, DataError> {
        let mut atoms: Vec<&Atom> = Vec::new();
        for c in checks {
            c.atoms(&mut atoms);
        }
        let mut sql = self.sql();
        let ids_ph = sql.push(Bind::I64Array(ids.to_vec()));
        let guard = self.guard.lower("u.r", &mut sql)?;
        let text = self.low.batch_check_sql(&atoms, &ids_ph, &guard, &mut sql);
        let rows = self.fetch("batch_check", text, sql.binds).await?;
        let mut sets = vec![RoaringTreemap::new(); atoms.len()];
        let mut guard_set = RoaringTreemap::new();
        for r in rows {
            let k: i32 = r.try_get("k").map_err(map_db)?;
            let rid: i64 = r.try_get("r").map_err(map_db)?;
            let Some(x) = key(rid) else { continue };
            match usize::try_from(k) {
                Ok(i) if i < sets.len() => {
                    sets[i].insert(x);
                }
                _ => {
                    guard_set.insert(x);
                }
            }
        }
        let all = bitmap(ids.iter().copied());
        let mut pass = all.clone();
        for c in checks {
            pass &= eval(c, &atoms, &sets, &all);
        }
        Ok(pass & guard_set)
    }

    /// バッチ駆動（単一キーのソート）: 段 1 で駆動から B 件 → 段 2 で一括検査 → 足りなければ続き。
    pub(super) async fn batch_drive(
        &mut self,
        conj: &[&'a Expr],
        key: BSort,
        after: Option<&Pos>,
        limit: i64,
    ) -> Result<PageOut, DataError> {
        let kind = self.low.q.field(key.field).kind;
        let missing = Expr::Not(Box::new(Expr::atom(key.field, Leaf::Exists)));
        let b = (limit * K_PREFETCH).clamp(64, BATCH_ARRAY_CAP as i64);
        let mut segment = after.map_or(1, Pos::segment);
        let mut start: Option<(SortVal, i64)> =
            after.map(|p| (p.vals.first().cloned().unwrap_or(SortVal::Missing), p.rint));
        let mut out: Vec<(i64, Pos)> = Vec::new();
        loop {
            let mut sql = self.sql();
            let text = self.low.batch_drive_sql(
                key,
                segment,
                start.as_ref().map(|(v, r)| (v, *r)),
                b,
                &mut sql,
            );
            let rows = self.fetch("batch_drive", text, sql.binds).await?;
            let mut entries = Vec::with_capacity(rows.len());
            for r in &rows {
                let rint: i64 = r.try_get("record_int").map_err(map_db)?;
                let v: Option<String> = r.try_get("v").map_err(map_db)?;
                entries.push((rint, SortVal::from_db(kind, v)));
            }
            self.stats.scanned += entries.len() as u64;
            if !entries.is_empty() {
                let mut checks: Vec<&Expr> = conj.to_vec();
                if segment == 2 {
                    checks.push(&missing);
                }
                let ids: Vec<i64> = entries.iter().map(|(r, _)| *r).collect();
                let pass = self.batch_pass(&checks, &ids).await?;
                for (r, v) in &entries {
                    if key_in(&pass, *r) {
                        out.push((
                            *r,
                            Pos {
                                vals: vec![v.clone()],
                                rint: *r,
                            },
                        ));
                    }
                }
                start = entries.last().map(|(r, v)| (v.clone(), *r));
            }
            if out.len() as i64 > limit {
                break;
            }
            if (entries.len() as i64) < b {
                if segment == 1 {
                    segment = 2;
                    start = None;
                    continue;
                }
                break;
            }
        }
        self.stats.strategy = "batch";
        let more = out.len() as i64 > limit;
        out.truncate(limit.max(0) as usize);
        let next = if more {
            out.last().map(|r| r.1.clone())
        } else {
            None
        };
        Ok(PageOut { rows: out, next })
    }

    /// 項の多い OR 駆動の和集合（Roaring）。上限を超えたら `None`。
    pub(super) async fn union_ids(
        &mut self,
        atoms: &[&Atom],
        cap: Option<i64>,
    ) -> Result<Option<Vec<i64>>, DataError> {
        let cap = cap.unwrap_or(i64::MAX / 2);
        let mut sql = self.sql();
        let text = self.low.union_terms_sql(atoms, cap, &mut sql);
        let rows = self.fetch("union_terms", text, sql.binds).await?;
        let mut acc = RoaringTreemap::new();
        for r in rows {
            let rid: i64 = r.try_get("r").map_err(map_db)?;
            if let Some(x) = key(rid) {
                acc.insert(x);
            }
        }
        if acc.len() > u64::try_from(cap).unwrap_or(u64::MAX) {
            self.stats.scanned += acc.len();
            return Ok(None);
        }
        Ok(Some(
            acc.iter().filter_map(|x| i64::try_from(x).ok()).collect(),
        ))
    }
}

fn key_in(set: &RoaringTreemap, r: i64) -> bool {
    key(r).is_some_and(|x| set.contains(x))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::bound::{Atom, Leaf, Val};

    #[test]
    fn tree_is_evaluated_with_set_algebra() {
        let a = Atom {
            field: 16,
            leaf: Leaf::Eq(Val::Text("x".into())),
        };
        let b = Atom {
            field: 17,
            leaf: Leaf::Exists,
        };
        let e = Expr::Or(vec![
            Expr::Atom(a.clone()),
            Expr::Not(Box::new(Expr::Atom(b.clone()))),
        ]);
        let mut atoms = Vec::new();
        e.atoms(&mut atoms);
        let all = bitmap([1, 2, 3, 4]);
        let sets = vec![bitmap([1]), bitmap([1, 2, 3])];
        let got: Vec<u64> = eval(&e, &atoms, &sets, &all).iter().collect();
        // a = {1}・¬b = {4} → {1, 4}
        assert_eq!(got, vec![1, 4]);
        let and = Expr::And(vec![Expr::Atom(a), Expr::Atom(b)]);
        let mut atoms2 = Vec::new();
        and.atoms(&mut atoms2);
        let got: Vec<u64> = eval(&and, &atoms2, &sets, &all).iter().collect();
        assert_eq!(got, vec![1]);
        assert_eq!(stages(&[0u8; 2_500]).count(), 3);
    }
}
