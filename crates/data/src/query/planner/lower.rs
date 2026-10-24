//! 降下（query-optimizer.md §6.1）: 選んだ戦略を**プランナ耐性 SQL** に落とす唯一の箇所（PIT-21・§6.4）。
//!
//! | 固定したいもの | 手段 |
//! |---|---|
//! | 駆動索引と走査順 | 駆動を「索引の並びと一致する `ORDER BY` ＋ `LIMIT`」を持つ部分問い合わせにする |
//! | 点検索であること | 検査の `EXISTS` 内に `OFFSET 0`（半結合への引き上げ・ハッシュ化 SubPlan を止める） |
//! | 検査の順序 | `CASE WHEN p₁ THEN (CASE WHEN p₂ THEN … ELSE false END) ELSE false END` |
//! | 候補集合 | `WITH cand AS MATERIALIZED (…)` |
//!
//! 全ての文は `$1 = tenant_int`・`$2 = table_int` で始まり（[`SqlBuf::scoped`]）、計画時に 1 パーティションへ
//! 枝刈りされる。値は全てバインドし、埋め込むのは固定の表名・別名だけ。
//! **全ての出力行に行述語の検査（[`GuardSql`]）が付く**（型で強制・§7.1）。

use std::collections::HashMap;
use std::fmt::Write as _;

use uuid::Uuid;

use super::guard::{Guard, GuardSql};
use super::keys::{Pos, SortVal};
use crate::index::{IndexTable, MAX_INDEX_KEY_BYTES};
use crate::model::{FieldId, SYSTEM_FIELD_CREATED_AT};
use crate::policy::compile_index::{
    body_text_cmp, exists_index, field_i16, text_eq_cond, text_in_cond,
};
use crate::query::bound::{
    Atom, BField, BSort, BoundQuery, ColRef, Expr, FieldKind, Leaf, LinkExpr, Val,
};
use crate::query::sql::{like_escape, sqlf, Bind, SqlBuf};

/// 長い境界値は切り詰め行の本体で比べる（プレフィクスの長さは最小で上限−3 バイト）。
pub(crate) const SAFE_BOUND_BYTES: usize = MAX_INDEX_KEY_BYTES - 4;

/// 降下の文脈（1 リクエスト分）。
pub(crate) struct Lowerer<'a> {
    pub q: &'a BoundQuery,
    /// record_ref の参照先 uuid → `record_int`（実行時に引き直した分だけ・外へ出さない）。
    pub links: &'a HashMap<Uuid, i64>,
    /// リンクの半結合の参照先（[`BoundQuery::scopes`] と同じ添字・入れ子も平坦）。
    pub scopes: &'a [ScopeLow<'a>],
}

/// 参照先テーブル 1 つの降下の材料（参照先のフィールド・`table_int`・閲覧者本人の行述語）。
pub(crate) struct ScopeLow<'a> {
    pub q: &'a BoundQuery,
    pub table_int: i64,
    pub guard: Guard<'a>,
}

/// 候補集合の入口（フィルタ駆動・OR 駆動・認可駆動・全件）。
#[derive(Debug, Clone)]
pub(crate) enum Source<'q> {
    /// 1 項の索引範囲。
    Atom(&'q Atom),
    /// 別の列どうしの OR の各項の和集合。
    Union(Vec<&'q Atom>),
    /// 行述語の選言の和集合（SQL 断片は [`super::guard`] が作る）。
    Policy(Vec<String>),
    /// 第 1 ソートキーの値が `[lo, hi]` に入る索引行（複数ソートの窓）。
    Window {
        key: BSort,
        lo: Option<(SortVal, bool)>,
        hi: Option<(SortVal, bool)>,
    },
    /// 全レコード（擬似列 created_at）。
    All,
    /// 等値範囲 2 つの交わり（交差駆動・O3）。両方の範囲を読み、`record_int` で突き合わせる。
    Intersect(&'q Atom, &'q Atom),
    /// Rust 側（Roaring ビットマップ）で合わせた `record_int` の集合（項の多い OR 駆動・バッチ駆動）。
    Ids(Vec<i64>),
}

/// 検査の静的順位（統計が無いときの並び・統計があるときの同点の決め手）。
pub(crate) fn check_rank(e: &Expr) -> u8 {
    match e {
        Expr::Atom(a) => match a.leaf {
            Leaf::Eq(_) => 0,
            Leaf::In(_) => 1,
            Leaf::Range { .. } | Leaf::StartsWith(_) => 2,
            Leaf::Contains(_) | Leaf::EndsWith(_) => 4,
            Leaf::Exists => 5,
        },
        Expr::Or(_) | Expr::And(_) => 3,
        Expr::Not(_) => 6,
        // 参照先の点検索と行述語を伴う（最も高い）。
        Expr::Link(_) => 7,
        Expr::True | Expr::False => 0,
    }
}

/// 検査の並び（安くてよく落ちる述語から・§5.2）。統計の無い O1 と、入れ子の積（`Or` / `Not` の中）は
/// 葉の種類の静的順位で並べる。最上位の積は統計があれば選択率で並べる（[`super::exec::Exec::order`]）。
pub(crate) fn order_checks(mut v: Vec<&Expr>) -> Vec<&Expr> {
    v.sort_by_key(|e| check_rank(e));
    v
}

impl Lowerer<'_> {
    fn field(&self, id: FieldId) -> &BField {
        self.q.field(id)
    }

    /// 原子式が読む索引テーブル（部分一致は `data_index_search`）。
    pub(crate) fn atom_table(&self, a: &Atom) -> IndexTable {
        match a.leaf {
            Leaf::Contains(_) | Leaf::EndsWith(_) => IndexTable::Search,
            _ => self.field(a.field).value_table(),
        }
    }

    /// 索引行 `x` についての葉の条件（`EXISTS (行 ∧ 条件)` ⇔ 原子式が真・2 値論理）。
    pub(crate) fn leaf_cond(&self, a: &Atom, x: &str, sql: &mut SqlBuf) -> String {
        let f = self.field(a.field);
        // 切り詰め行の再確認先（本体の完全値）。擬似列は長い値を持たず、材料化した派生列は本体に値が無い
        // （束縛で長い比較値を 422 にしているので、派生列の比較は切り詰め行の再確認を要しない）。
        let body_field = (!f.is_pseudo() && !f.derived).then_some(f.id);
        match (&a.leaf, f.kind) {
            (Leaf::Exists, _) => "TRUE".into(),
            (Leaf::Contains(s), _) => {
                let ph = sql.push(Bind::Text(format!("%{}%", like_escape(s))));
                sqlf!(sql, "{x}.val LIKE {ph}::text")
            }
            (Leaf::EndsWith(s), _) => {
                let ph = sql.push(Bind::Text(format!("%{}", like_escape(s))));
                sqlf!(sql, "{x}.val LIKE {ph}::text")
            }
            (Leaf::Eq(_) | Leaf::In(_), FieldKind::Link) => {
                let vals: Vec<&Val> = match &a.leaf {
                    Leaf::Eq(v) => vec![v],
                    Leaf::In(vs) => vs.iter().collect(),
                    _ => Vec::new(),
                };
                // 引き直せなかった参照先（存在しない・別テナント）は配列に入らないだけ（空の配列は偽）。
                // 引き直しの成否で SQL テキストを変えない（data-platform §16.3）。
                let ints: Vec<i64> = vals
                    .into_iter()
                    .filter_map(|v| match v {
                        Val::Ref(u) => self.links.get(u).copied(),
                        _ => None,
                    })
                    .collect();
                let ph = sql.push(Bind::I64Array(ints));
                sqlf!(sql, "{x}.dst_record = ANY({ph}::int8[])")
            }
            (Leaf::Eq(Val::Num(d)), FieldKind::Num) => {
                let ph = sql.typed(Bind::Numeric(d.canonical()));
                sqlf!(sql, "{x}.val = {ph}")
            }
            (Leaf::In(vs), FieldKind::Num) => {
                let arr: Vec<String> = vs
                    .iter()
                    .filter_map(|v| match v {
                        Val::Num(d) => Some(d.canonical()),
                        _ => None,
                    })
                    .collect();
                let ph = sql.push(Bind::NumericArray(arr));
                sqlf!(sql, "{x}.val = ANY({ph}::numeric[])")
            }
            (Leaf::Eq(Val::Text(s)), _) => text_eq_cond(x, s, body_field, sql),
            (Leaf::In(vs), _) => {
                let arr: Vec<String> = vs
                    .iter()
                    .filter_map(|v| v.as_text().map(str::to_string))
                    .collect();
                text_in_cond(x, &arr, body_field, sql)
            }
            (Leaf::Range { lo, hi }, kind) => {
                let mut parts = Vec::new();
                if let Some((v, inc)) = lo {
                    parts.push(Self::bound_cond(
                        x,
                        kind,
                        v,
                        if *inc { ">=" } else { ">" },
                        body_field,
                        sql,
                    ));
                }
                if let Some((v, inc)) = hi {
                    parts.push(Self::bound_cond(
                        x,
                        kind,
                        v,
                        if *inc { "<=" } else { "<" },
                        body_field,
                        sql,
                    ));
                }
                if parts.is_empty() {
                    "TRUE".into()
                } else {
                    parts.join(" AND ")
                }
            }
            (Leaf::StartsWith(p), _) => {
                // 長い接頭辞（切り詰め行はプレフィクスでは判定できない）は本体で再確認する。長短は真偽
                // スイッチで選び、値の長さで SQL テキストを変えない（data-platform §16.3）。
                let ph = sql.push(Bind::Text(p.clone()));
                let lf = sql.flag(p.len() > SAFE_BOUND_BYTES);
                let field = body_field.unwrap_or(0);
                let kph = sql.push(Bind::Text(crate::field_key::json_key(field)));
                let tbl = sql.t();
                sqlf!(sql,
                    "((starts_with({x}.val, {ph}::text) AND (NOT {lf} OR NOT {x}.truncated)) OR ({lf} AND {x}.truncated AND \
                     EXISTS (SELECT 1 FROM data_record b WHERE b.tenant_int = $1 AND b.table_int = {tbl} \
                     AND b.record_int = {x}.record_int AND starts_with(b.data ->> {kph}, {ph}::text))))"
                )
            }
            // 型の合わない組（束縛で弾くので通常来ない）は不成立へ倒す。
            _ => "FALSE".into(),
        }
    }

    fn bound_cond(
        x: &str,
        kind: FieldKind,
        v: &Val,
        op: &str,
        body_field: Option<FieldId>,
        sql: &mut SqlBuf,
    ) -> String {
        match (kind, v) {
            (FieldKind::Num, Val::Num(d)) => {
                let ph = sql.typed(Bind::Numeric(d.canonical()));
                sqlf!(sql, "{x}.val {op} {ph}")
            }
            (_, Val::Text(s)) => {
                // 長い境界: 切り詰め行はプレフィクスでは判定できないので本体で比べる。短い境界は
                // プレフィクスだけで決まる。長短は真偽スイッチ（値の長さで SQL テキストを変えない）。
                let ph = sql.push(Bind::Text(s.clone()));
                let lf = sql.flag(s.len() > SAFE_BOUND_BYTES);
                let body = match body_field {
                    Some(f) => body_text_cmp(x, f, op, s, sql),
                    None => "FALSE".into(),
                };
                sqlf!(sql,
                    "(({x}.val {op} {ph}::text COLLATE \"C\" AND (NOT {lf} OR NOT {x}.truncated)) OR ({lf} AND {x}.truncated AND {body}))"
                )
            }
            _ => "FALSE".into(),
        }
    }

    /// 原子式の点検索（駆動行 `rec` について）。
    pub(crate) fn atom_exists(&self, a: &Atom, rec: &str, sql: &mut SqlBuf) -> String {
        let cond = self.leaf_cond(a, "x", sql);
        exists_index(self.atom_table(a), a.field, rec, &cond, sql)
    }

    /// 条件式の検査（2 値論理。`And` は評価順を固定した入れ子の CASE）。
    pub(crate) fn expr_sql(&self, e: &Expr, rec: &str, sql: &mut SqlBuf) -> String {
        match e {
            Expr::True => "TRUE".into(),
            Expr::False => "FALSE".into(),
            Expr::Atom(a) => self.atom_exists(a, rec, sql),
            Expr::Not(inner) => {
                let inner = self.expr_sql(inner, rec, sql);
                sqlf!(sql, "NOT ({inner})")
            }
            Expr::Or(v) => {
                let parts: Vec<String> = v.iter().map(|c| self.expr_sql(c, rec, sql)).collect();
                sqlf!(sql, "({})", parts.join(" OR "))
            }
            Expr::And(v) => {
                let kids = order_checks(v.iter().collect());
                self.case_chain(&kids, None, rec, sql)
            }
            Expr::Link(l) => self.link_sql(l, rec, sql),
        }
    }

    /// リンクの半結合（駆動行 `rec` からリンクを辿り、参照先に「閲覧者に見えて条件を満たす行」があるか）。
    ///
    /// 参照先の条件と行述語（`Guard(P)`・閲覧者本人の材料）は、参照先の `table_int` のバインドへ切り替えて
    /// 降ろす（PIT-20: 参照先の行述語を省いた結合は存在しない）。辺の相手が削除済みなら成立しない。
    /// `OFFSET 0` で行ごとの点検索に固定する（query-optimizer.md §6.1）。
    pub(crate) fn link_sql(&self, l: &LinkExpr, rec: &str, sql: &mut SqlBuf) -> String {
        let Some(sc) = self.scopes.get(l.scope) else {
            // 参照先の材料が無い半結合は成立させない（fail-closed）。
            return "FALSE".into();
        };
        let parent = sql.t();
        let fph = sql.typed(Bind::I16(field_i16(l.field)));
        let cph = sql.typed(Bind::I64(sc.table_int));
        let n = sql.alias();
        let dst = sqlf!(sql, "lk{n}.dst_record");
        let child = Lowerer {
            q: sc.q,
            links: self.links,
            scopes: self.scopes,
        };
        let (cond, guard) = sql.within(&cph, |sql| {
            let cond = child.expr_sql(&l.filter, &dst, sql);
            // 行述語の降下の誤り（入口で検査済み）は不成立へ倒す。
            let guard = sc
                .guard
                .lower(&dst, sql)
                .map_or_else(|_| "FALSE".to_string(), |g| g.as_str().to_string());
            (cond, guard)
        });
        sqlf!(sql,
            "EXISTS (SELECT 1 FROM data_link lk{n} WHERE lk{n}.tenant_int = $1 AND lk{n}.table_int = {parent} \
             AND lk{n}.field_id = {fph} AND lk{n}.src_record = {rec} AND lk{n}.dst_table_int = {cph} \
             AND EXISTS (SELECT 1 FROM data_record ld{n} WHERE ld{n}.tenant_int = $1 \
             AND ld{n}.table_int = {cph} AND ld{n}.record_int = {dst}) \
             AND {cond} AND ({guard}) OFFSET 0)"
        )
    }

    /// `CASE WHEN c₁ THEN (CASE WHEN c₂ THEN … <guard> …) ELSE false END`（最後に行述語）。
    pub(crate) fn case_chain(
        &self,
        checks: &[&Expr],
        guard: Option<&GuardSql>,
        rec: &str,
        sql: &mut SqlBuf,
    ) -> String {
        let tail = guard.map_or_else(|| "TRUE".to_string(), |g| sqlf!(sql, "({})", g.as_str()));
        let mut parts: Vec<String> = checks.iter().map(|c| self.expr_sql(c, rec, sql)).collect();
        let mut out = tail;
        while let Some(p) = parts.pop() {
            out = sqlf!(sql, "CASE WHEN {p} THEN {out} ELSE false END");
        }
        out
    }

    /// 候補集合の SELECT（列 `record_int`）。
    pub(crate) fn source_sql(&self, src: &Source<'_>, sql: &mut SqlBuf) -> String {
        match src {
            Source::Atom(a) => self.atom_source(a, sql),
            Source::Union(v) => {
                let parts: Vec<String> = v.iter().map(|a| self.atom_source(a, sql)).collect();
                parts.join(" UNION ")
            }
            Source::Policy(parts) => parts.join(" UNION "),
            Source::Intersect(a, b) => {
                let x = self.atom_source(a, sql);
                let y = self.atom_source(b, sql);
                sqlf!(sql, "({x}) INTERSECT ({y})")
            }
            Source::Ids(ids) => {
                let ph = sql.push(Bind::I64Array(ids.clone()));
                sqlf!(sql, "SELECT unnest({ph}::int8[]) AS record_int")
            }
            Source::All => {
                let ph = sql.typed(Bind::I16(field_i16(SYSTEM_FIELD_CREATED_AT)));
                sqlf!(
                    sql,
                    "SELECT i.record_int FROM data_index_text i \
                     WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = {ph}"
                )
            }
            Source::Window { key, lo, hi } => {
                let f = self.field(key.field);
                let rel = f.value_table().relation();
                let fph = sql.typed(Bind::I16(field_i16(f.id)));
                let mut s = sqlf!(
                    sql,
                    "SELECT i.record_int FROM {rel} i \
                     WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = {fph}"
                );
                for (b, lower) in [(lo, true), (hi, false)] {
                    if let Some((v, inc)) = b {
                        let op = match (lower, inc) {
                            (true, true) => ">=",
                            (true, false) => ">",
                            (false, true) => "<=",
                            (false, false) => "<",
                        };
                        let ph = sort_val_ph(f.kind, v, sql);
                        if !sql.skip_text {
                            let _ = write!(s, " AND i.val {op} {ph}");
                        }
                    }
                }
                s
            }
        }
    }

    pub(crate) fn atom_source(&self, a: &Atom, sql: &mut SqlBuf) -> String {
        let table = self.atom_table(a);
        let rel = table.relation();
        let col = table.record_column();
        let fph = sql.typed(Bind::I16(field_i16(a.field)));
        let cond = self.leaf_cond(a, "x", sql);
        let distinct = if self.field(a.field).multi() || table == IndexTable::Search {
            "DISTINCT "
        } else {
            ""
        };
        sqlf!(
            sql,
            "SELECT {distinct}x.{col} AS record_int FROM {rel} x \
             WHERE x.tenant_int = $1 AND x.table_int = $2 AND x.field_id = {fph} AND {cond}"
        )
    }

    /// ソート値を引くスカラー部分問い合わせ（型はそのまま・text は `COLLATE "C"`）。
    pub(crate) fn sort_value_sql(&self, k: BSort, rec: &str, sql: &mut SqlBuf) -> String {
        let f = self.field(k.field);
        let rel = f.value_table().relation();
        let fph = sql.typed(Bind::I16(field_i16(f.id)));
        sqlf!(
            sql,
            "(SELECT s.val FROM {rel} s WHERE s.tenant_int = $1 AND s.table_int = $2 \
             AND s.field_id = {fph} AND s.record_int = {rec} AND s.ord = 0 OFFSET 0)"
        )
    }

    /// 候補集合を検査し、全ソートキーで並べて位置より後ろの先頭 `limit` 件を返す文。
    ///
    /// 列: `n_cand`（候補数・上限超過の検出）, `record_int`, `k0..`（text 表現）。0 件でも 1 行返す。
    #[allow(clippy::too_many_arguments)] // 1 文の部品（入口・検査・Guard・位置・予算・件数）を明示で渡す。
    pub(crate) fn candidates_sql(
        &self,
        src: &Source<'_>,
        checks: &[&Expr],
        guard: &GuardSql,
        after: Option<&Pos>,
        cap: Option<i64>,
        limit: i64,
        sql: &mut SqlBuf,
    ) -> String {
        let source = self.source_sql(src, sql);
        let cap_sql = cap.map_or_else(String::new, |c| {
            let ph = sql.typed(Bind::I64(c + 1));
            sqlf!(sql, " LIMIT {ph}")
        });
        let check = self.case_chain(checks, Some(guard), "c.record_int", sql);
        let sort = &self.q.sort;
        let mut cols = String::new();
        let mut outer = String::new();
        let mut order = Vec::new();
        for (i, k) in sort.iter().enumerate() {
            let v = self.sort_value_sql(*k, "p.record_int", sql);
            if !sql.skip_text {
                let _ = write!(cols, ", {v} AS k{i}");
                let _ = write!(outer, ", z.k{i}::text AS k{i}");
            }
            let dir = if k.desc { "DESC" } else { "ASC" };
            order.push(sqlf!(sql, "(z.k{i} IS NULL), z.k{i} {dir}"));
        }
        let rdir = if sort.first().is_some_and(|k| k.desc) {
            "DESC"
        } else {
            "ASC"
        };
        order.push(sqlf!(sql, "z.record_int {rdir}"));
        let keyset = after.map_or_else(|| "TRUE".to_string(), |p| self.keyset_sql(p, sql));
        let lim = sql.typed(Bind::I64(limit));
        sqlf!(sql,
            "WITH cand AS MATERIALIZED ({source}{cap_sql}), \
             pass AS MATERIALIZED (SELECT c.record_int FROM cand c WHERE {check}) \
             SELECT nc.n AS n_cand, np.n AS n_pass, z.record_int{outer} \
             FROM (SELECT count(*) AS n FROM cand) nc CROSS JOIN (SELECT count(*) AS n FROM pass) np \
             LEFT JOIN (SELECT p.record_int{cols} FROM pass p) z ON {keyset} \
             ORDER BY {} LIMIT {lim}",
            order.join(", ")
        )
    }

    /// 全順序で `p` より後ろ（別名 `z`・列 `k0..`・`record_int`）。値なしは各キーの末尾。
    ///
    /// 値なしの位置も `NULL` のバインドで表し、位置の値の有無で SQL テキストを変えない
    /// （`IS NOT DISTINCT FROM`・`$v IS NOT NULL` は計画時に畳み込まれる・data-platform §16.3）。
    pub(crate) fn keyset_sql(&self, p: &Pos, sql: &mut SqlBuf) -> String {
        let sort = &self.q.sort;
        let mut ors = Vec::new();
        let mut eqs: Vec<String> = Vec::new();
        for (i, k) in sort.iter().enumerate() {
            let f = self.field(k.field);
            let col = sqlf!(sql, "z.k{i}");
            let v = p.vals.get(i).unwrap_or(&SortVal::Missing);
            let ph = sort_val_ph(f.kind, v, sql);
            let op = if k.desc { "<" } else { ">" };
            let gt = sqlf!(
                sql,
                "({ph} IS NOT NULL AND ({col} IS NULL OR {col} {op} {ph}))"
            );
            let mut conj = eqs.clone();
            conj.push(gt);
            ors.push(sqlf!(sql, "({})", conj.join(" AND ")));
            eqs.push(sqlf!(sql, "{col} IS NOT DISTINCT FROM {ph}"));
        }
        let rop = if sort.first().is_some_and(|k| k.desc) {
            "<"
        } else {
            ">"
        };
        let rph = sql.typed(Bind::I64(p.rint));
        let mut conj = eqs;
        conj.push(sqlf!(sql, "z.record_int {rop} {rph}"));
        ors.push(sqlf!(sql, "({})", conj.join(" AND ")));
        sqlf!(sql, "({})", ors.join(" OR "))
    }

    /// 予算付きソート駆動（区間 1・単一キーまたは複数ソートの窓探し）。§6.1 の形。
    ///
    /// 列: `record_int`, `v`（第 1 キーの text 表現）, `pos`（何件目を読んだか）, `pass`, `is_last`。
    /// `start` は `(値, record_int)` の厳密に後ろから、`group_start` は値の同値群の先頭から歩く。
    /// `stop` は走査の終点（値, 含むか・第 1 キーの列の範囲条件の反対の端・[`super::seek`]）。
    #[allow(clippy::too_many_arguments)] // 1 文の部品（入口・検査・Guard・位置・予算・件数）を明示で渡す。
    pub(crate) fn sort_drive_sql(
        &self,
        key: BSort,
        start: Option<(&SortVal, Option<i64>)>,
        stop: Option<&(SortVal, bool)>,
        checks: &[&Expr],
        guard: &GuardSql,
        budget: i64,
        limit: i64,
        sql: &mut SqlBuf,
    ) -> String {
        let f = self.field(key.field);
        let rel = f.value_table().relation();
        let dir = if key.desc { "DESC" } else { "ASC" };
        let fph = sql.typed(Bind::I16(field_i16(f.id)));
        let keyset = match start {
            None => String::new(),
            Some((v, Some(r))) => {
                let op = if key.desc { "<" } else { ">" };
                let vph = sort_val_ph(f.kind, v, sql);
                let rph = sql.typed(Bind::I64(r));
                sqlf!(sql, " AND (i.val, i.record_int) {op} ({vph}, {rph})")
            }
            Some((v, None)) => {
                let op = if key.desc { "<=" } else { ">=" };
                let vph = sort_val_ph(f.kind, v, sql);
                sqlf!(sql, " AND i.val {op} {vph}")
            }
        };
        let stop = match stop {
            None => String::new(),
            Some((v, inc)) => {
                let op = match (key.desc, *inc) {
                    (false, true) => "<=",
                    (false, false) => "<",
                    (true, true) => ">=",
                    (true, false) => ">",
                };
                let vph = sort_val_ph(f.kind, v, sql);
                sqlf!(sql, " AND i.val {op} {vph}")
            }
        };
        let bph = sql.typed(Bind::I64(budget.max(1)));
        let check = self.case_chain(checks, Some(guard), "d.record_int", sql);
        let lph = sql.typed(Bind::I64(limit));
        sqlf!(
            sql,
            "SELECT s.record_int, s.val::text AS v, s.pos, s.pass, s.is_last FROM ( \
               SELECT d.record_int, d.val, row_number() OVER w AS pos, \
                      lead(d.record_int) OVER w IS NULL AS is_last, {check} AS pass \
               FROM (SELECT i.record_int, i.val FROM {rel} i \
                     WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = {fph}{keyset}{stop} \
                     ORDER BY i.val {dir}, i.record_int {dir} LIMIT {bph}) d \
               WINDOW w AS (ORDER BY d.val {dir}, d.record_int {dir}) \
             ) s WHERE s.pass OR s.is_last LIMIT {lph}"
        )
    }

    /// 区間 2（第 1 ソートキーの値が無い行）: 擬似列 created_at を `record_int` 順に歩き、
    /// 「値が無い」を点検索で確かめる。列は [`Self::sort_drive_sql`] と同じ（`v` は常に NULL）。
    #[allow(clippy::too_many_arguments)] // 1 文の部品（入口・検査・Guard・位置・予算・件数）を明示で渡す。
    pub(crate) fn missing_drive_sql(
        &self,
        key: BSort,
        after_rint: Option<i64>,
        checks: &[&Expr],
        guard: &GuardSql,
        budget: i64,
        limit: i64,
        sql: &mut SqlBuf,
    ) -> String {
        let f = self.field(key.field);
        let dir = if key.desc { "DESC" } else { "ASC" };
        let cph = sql.typed(Bind::I16(field_i16(SYSTEM_FIELD_CREATED_AT)));
        let keyset = after_rint.map_or_else(String::new, |r| {
            let op = if key.desc { "<" } else { ">" };
            let ph = sql.typed(Bind::I64(r));
            sqlf!(sql, " AND i.record_int {op} {ph}")
        });
        let bph = sql.typed(Bind::I64(budget.max(1)));
        let missing = Expr::Not(Box::new(Expr::atom(f.id, Leaf::Exists)));
        let mut all: Vec<&Expr> = vec![&missing];
        all.extend_from_slice(checks);
        let check = self.case_chain(&all, Some(guard), "d.record_int", sql);
        let lph = sql.typed(Bind::I64(limit));
        sqlf!(
            sql,
            "SELECT s.record_int, NULL::text AS v, s.pos, s.pass, s.is_last FROM ( \
               SELECT d.record_int, row_number() OVER w AS pos, \
                      lead(d.record_int) OVER w IS NULL AS is_last, {check} AS pass \
               FROM (SELECT i.record_int FROM data_index_text i \
                     WHERE i.tenant_int = $1 AND i.table_int = $2 AND i.field_id = {cph}{keyset} \
                     ORDER BY i.record_int {dir} LIMIT {bph}) d \
               WINDOW w AS (ORDER BY d.record_int {dir}) \
             ) s WHERE s.pass OR s.is_last LIMIT {lph}"
        )
    }

    /// 上限付き件数（`cap + 1` で打ち切る・data-platform §5.4）。
    pub(crate) fn count_sql(
        &self,
        src: &Source<'_>,
        checks: &[&Expr],
        guard: &GuardSql,
        cap: i64,
        sql: &mut SqlBuf,
    ) -> String {
        let source = self.source_sql(src, sql);
        let check = self.case_chain(checks, Some(guard), "c.record_int", sql);
        let ph = sql.typed(Bind::I64(cap + 1));
        sqlf!(
            sql,
            "WITH cand AS MATERIALIZED ({source}) \
             SELECT count(*) FROM (SELECT 1 FROM cand c WHERE {check} LIMIT {ph}) s"
        )
    }

    /// 上限付き件数探索（§4.3）。
    ///
    /// 値索引の並び（`val, record_int`）で `ORDER BY … LIMIT` にして、上限に達した所で必ず止まる
    /// 索引走査に固定する（ビットマップ走査は一致する全エントリを先に読むので、件数が多い値ほど遅くなる・
    /// プール統計の推定に左右されない・PIT-69）。多値の重複は数える（上限として使うので足りる・§4.3）。
    pub(crate) fn probe_sql(&self, a: &Atom, cap: i64, sql: &mut SqlBuf) -> String {
        let table = self.atom_table(a);
        // `= ANY(配列)` は値索引の並びと両立しない（索引条件にならず全件を読む）ので、値ごとの
        // 等値探索を `unnest` の `LATERAL` で並べる（要素数で SQL テキストを変えない・§16.3）。
        match (&a.leaf, table) {
            (Leaf::In(vals), IndexTable::Text) => {
                let texts: Vec<String> = vals
                    .iter()
                    .filter_map(|v| v.as_text().map(str::to_string))
                    .collect();
                probe_each(
                    table,
                    a.field,
                    Bind::TextArray(index_prefixes(&texts)),
                    cap,
                    sql,
                )
            }
            (Leaf::In(vals), IndexTable::Num) => {
                let nums: Vec<String> = vals
                    .iter()
                    .filter_map(|v| match v {
                        Val::Num(d) => Some(d.canonical()),
                        _ => None,
                    })
                    .collect();
                probe_each(table, a.field, Bind::NumericArray(nums), cap, sql)
            }
            _ => {
                let c = self.leaf_cond(a, "x", sql);
                probe_union(table, a.field, &[c], cap, sql)
            }
        }
    }

    /// 集計（候補 → 検査 → 索引値でグループ化・数値は numeric で正確に）。
    pub(crate) fn aggregate_sql(
        &self,
        src: &Source<'_>,
        checks: &[&Expr],
        guard: &GuardSql,
        sql: &mut SqlBuf,
    ) -> String {
        let source = self.source_sql(src, sql);
        let check = self.case_chain(checks, Some(guard), "c.record_int", sql);
        let t = sql.t();
        let (inner, select, group) =
            self.agg_columns(&|_| (t.clone(), "p.record_int".to_string()), sql);
        sqlf!(
            sql,
            "WITH cand AS MATERIALIZED ({source}), \
             pass AS MATERIALIZED (SELECT c.record_int FROM cand c WHERE {check}) \
             SELECT {} FROM (SELECT {} FROM pass p) z{group}",
            select.join(", "),
            inner.join(", ")
        )
    }

    /// 集計の列（内側の値の点検索・外側の集計式・GROUP BY）。`at(列)` はその列の値を引く表の
    /// `table_int` のプレースホルダと行の `record_int` の式（結合では結合先の表・行）。
    ///
    /// 値は行ごとの点検索（スカラー部分問い合わせ）で引く。索引表どうしの JOIN は書かない（S4）。
    pub(crate) fn agg_columns(
        &self,
        at: &dyn Fn(ColRef) -> (String, String),
        sql: &mut SqlBuf,
    ) -> (Vec<String>, Vec<String>, String) {
        let mut inner = Vec::new();
        let mut select = vec!["count(*)::int8 AS grp_count".to_string()];
        let mut group = Vec::new();
        for (i, g) in self.q.group_by.iter().enumerate() {
            let f = self.q.col_field(*g);
            let (t, rec) = at(*g);
            let rel = f.value_table().relation();
            let fph = sql.typed(Bind::I16(field_i16(f.id)));
            let text = f.value_table() == IndexTable::Text;
            // 切り詰め行（data-platform §3.3）はプレフィクスでは値を区別できない。実フィールドは本体の
            // 完全値でグループ化する。材料化した派生列は本体に値が無いので、切り詰め行がグループに入ったか
            // （gt{i}）を返し、組み立てで 422 にする（可視・条件を満たした行だけで判定する）。
            let val = if text && !f.is_pseudo() && !f.derived {
                let kph = sql.push(Bind::Text(crate::field_key::json_key(f.id)));
                sqlf!(
                    sql,
                    "CASE WHEN g.truncated THEN (SELECT b.data ->> {kph} FROM data_record b \
                     WHERE b.tenant_int = $1 AND b.table_int = {t} AND b.record_int = {rec}) \
                     ELSE g.val::text END"
                )
            } else {
                "g.val::text".to_string()
            };
            let probe = |col: &str| {
                sqlf!(
                    sql,
                    "(SELECT {col} FROM {rel} g WHERE g.tenant_int = $1 AND g.table_int = {t} \
                     AND g.field_id = {fph} AND g.record_int = {rec} AND g.ord = 0 OFFSET 0)"
                )
            };
            inner.push(sqlf!(sql, "{} AS g{i}", probe(&val)));
            select.push(sqlf!(sql, "z.g{i}"));
            group.push(sqlf!(sql, "z.g{i}"));
            if text && f.derived {
                inner.push(sqlf!(sql, "{} AS gt{i}", probe("g.truncated")));
                select.push(sqlf!(sql, "bool_or(z.gt{i}) AS gt{i}"));
            }
        }
        for (i, (m, field)) in self.q.metrics.iter().enumerate() {
            use crate::query::ir::Metric;
            let Some(c) = field else {
                select.push(sqlf!(sql, "count(*)::float8 AS m{i}"));
                continue;
            };
            let (t, rec) = at(*c);
            let fph = sql.typed(Bind::I16(field_i16(c.field)));
            inner.push(sqlf!(sql,
                "(SELECT n.val FROM data_index_num n WHERE n.tenant_int = $1 AND n.table_int = {t} \
                 AND n.field_id = {fph} AND n.record_int = {rec} AND n.ord = 0 OFFSET 0) AS m{i}"
            ));
            let func = match m {
                Metric::Sum => "sum",
                Metric::Avg => "avg",
                Metric::Min => "min",
                Metric::Max => "max",
                Metric::Count => "count",
            };
            select.push(sqlf!(sql, "({func}(z.m{i}))::float8 AS m{i}"));
        }
        if inner.is_empty() {
            inner.push("1 AS one".to_string());
        }
        let group_sql = if group.is_empty() {
            String::new()
        } else {
            sqlf!(sql, " GROUP BY {}", group.join(", "))
        };
        (inner, select, group_sql)
    }
}

/// 値の集合ごとの上限付き件数探索（`unnest` の各値について値索引の並びで `LIMIT` し、和をさらに上限で切る）。
///
/// `vals` は `TextArray`（索引上のプレフィクス）か `NumericArray`。件数探索は計画の目安なので、切り詰め行は
/// プレフィクスで数える（多めに数えるだけで結果には影響しない）。
pub(crate) fn probe_each(
    table: IndexTable,
    field: FieldId,
    vals: Bind,
    cap: i64,
    sql: &mut SqlBuf,
) -> String {
    let rel = table.relation();
    let ty = if matches!(vals, Bind::NumericArray(_)) {
        "numeric[]"
    } else {
        "text[]"
    };
    let vph = sql.push(vals);
    let fph = sql.typed(Bind::I16(field_i16(field)));
    let ph = sql.typed(Bind::I64(cap));
    let t = sql.t();
    let order = index_order(table);
    sqlf!(sql,
        "SELECT count(*) FROM (SELECT 1 FROM unnest({vph}::{ty}) v(val) CROSS JOIN LATERAL \
         (SELECT 1 FROM {rel} x WHERE x.tenant_int = $1 AND x.table_int = {t} AND x.field_id = {fph} \
         AND x.val = v.val ORDER BY {order} LIMIT {ph}) y LIMIT {ph}) s"
    )
}

/// 索引上の値（`MAX_INDEX_KEY_BYTES` で文字境界に切り詰めたプレフィクス）。
pub(crate) fn index_prefixes(vals: &[String]) -> Vec<String> {
    vals.iter()
        .map(|v| {
            crate::index::normalize::prefix_at_char_boundary(v, MAX_INDEX_KEY_BYTES)
                .0
                .to_string()
        })
        .collect()
}

/// 上限付き件数探索の文（条件ごとに値索引の並びで `LIMIT` した和を、さらに上限で切る）。
pub(crate) fn probe_union(
    table: IndexTable,
    field: FieldId,
    conds: &[String],
    cap: i64,
    sql: &mut SqlBuf,
) -> String {
    let rel = table.relation();
    let fph = sql.typed(Bind::I16(field_i16(field)));
    let ph = sql.typed(Bind::I64(cap));
    let order = index_order(table);
    let parts: Vec<String> = conds
        .iter()
        .map(|c| {
            sqlf!(
                sql,
                "(SELECT 1 FROM {rel} x WHERE x.tenant_int = $1 AND x.table_int = $2 \
                 AND x.field_id = {fph} AND {c} ORDER BY {order} LIMIT {ph})"
            )
        })
        .collect();
    sqlf!(
        sql,
        "SELECT count(*) FROM (SELECT 1 FROM ({}) u LIMIT {ph}) s",
        parts.join(" UNION ALL ")
    )
}

/// 索引表の値索引の並び（件数探索を早期終了する索引走査に固定するため）。
pub(crate) fn index_order(table: IndexTable) -> &'static str {
    match table {
        IndexTable::Text | IndexTable::Num => "x.val, x.record_int",
        IndexTable::Link => "x.src_record, x.ord",
        // 部分一致（GIN）は並びを持たない（一致の全件を読む）。
        IndexTable::Unique | IndexTable::Search => "x.record_int",
    }
}

/// ソート値のプレースホルダ（num は numeric・text は `COLLATE "C"`）。値なしは型つきの `NULL` の
/// バインド（値の有無で SQL テキストを変えない・data-platform §16.3）。
pub(crate) fn sort_val_ph(kind: FieldKind, v: &SortVal, sql: &mut SqlBuf) -> String {
    match (kind, v) {
        (_, SortVal::Num(d)) => sql.typed(Bind::OptNumeric(Some(d.canonical()))),
        (_, SortVal::Text(s)) => {
            let ph = sql.push(Bind::OptText(Some(s.clone())));
            sqlf!(sql, "{ph}::text COLLATE \"C\"")
        }
        (FieldKind::Num, SortVal::Missing) => sql.typed(Bind::OptNumeric(None)),
        (_, SortVal::Missing) => {
            let ph = sql.push(Bind::OptText(None));
            sqlf!(sql, "{ph}::text COLLATE \"C\"")
        }
    }
}

/// 本体の取得（ページの行だけ・`(tenant_int, table_int, record_int)` の一意索引で点検索）。
///
/// **射影刈り込み**（data-platform §16.2）: `keys` があれば本体の JSONB から**そのキーだけ**を DB 側で取り出す
/// （`jsonb_each` を `= ANY($keys)` で絞って組み直す）。キーの集合はバインドで渡すので、テキストは刈り込みの
/// 有無（＝射影の有無・形状に入っている）だけで決まる。値の無いキーは結果にも無い（本体と同じ意味）。
/// JSONB は TOAST の展開を避けられない（§16.5）ので、効くのは転送量と Rust 側の直列化の量。
pub(crate) fn fetch_sql(
    tenant_id: &str,
    ints: Vec<i64>,
    keys: Option<Vec<String>>,
    sql: &mut SqlBuf,
) -> String {
    let tph = sql.push(Bind::Text(tenant_id.to_string()));
    let iph = sql.push(Bind::I64Array(ints));
    let data = match keys {
        None => "r.data".to_string(),
        Some(k) => {
            let kph = sql.push(Bind::TextArray(k));
            sqlf!(
                sql,
                "(SELECT COALESCE(jsonb_object_agg(e.key, e.value), '{{}}'::jsonb) \
                 FROM jsonb_each(r.data) e WHERE e.key = ANY({kph}::text[])) AS data"
            )
        }
    };
    sqlf!(sql,
        "SELECT r.record_int, r.id, r.table_id, {data}, r.rev, r.owner, r.created_at, r.updated_at \
         FROM data_record r WHERE r.tenant_int = $1 AND r.table_int = $2 AND r.tenant_id = {tph} \
         AND r.record_int = ANY({iph}::int8[])"
    )
}

#[cfg(test)]
#[path = "lower_tests.rs"]
mod tests;
