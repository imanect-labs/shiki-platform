//! 束縛と IR への変換（data-platform §16.1 ③④）: [`super::ast::Select`] → [`DataQuery`]。
//!
//! - **名前の解決は閲覧者に見えるものだけで行う**。存在しない列とマスクされた列、存在しないテーブルと
//!   viewer でないテーブルは、同じコード・同じ文言・同じ候補（見える名前だけ）で拒否する
//!   （[`QueryRejection::unknown_name`]・列/テーブルの存在オラクルを作らない）。
//! - **JOIN は宣言済みリンク列の等値 1 個だけ**。`ON 既出.リンク列 = 新.\_id`（順方向）か
//!   `ON 新.リンク列 = 既出.\_id`（逆方向・リンク列の対称列を辿る）で、リンク列の参照先テーブルが
//!   結合先と一致しなければ拒否する（`(src_table, field_id, dst_table)` が宣言と一致するものだけ）。
//!   カーディナリティは `LinkDef` から取り、SQL 側では指定できない。
//! - WHERE は最上位の AND で表ごとに振り分ける（FROM の表 → `filter`・結合先 → 結合の `filter`）。
//!   1 つの項に複数の表の列を混ぜた OR / NOT は拒否する（参照先の行述語の適用位置をずらさない）。

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::ast::{Agg, BExpr, Col, Item, Loc, Operand, OrdTarget, Select};
use super::translate::{cols, value_for, RCol, Translator};
use crate::model::{FieldType, TableSchema};
use crate::query::ir::{
    CondOp, Condition, DataQuery, GroupSortKey, HavingCmp, HavingCond, JoinSpec, Metric,
    MetricSpec, SortKey, MAX_GROUP_LIMIT, MAX_LIMIT,
};
use crate::query::limits::charge;
use crate::query::reject::{NameKind, QueryRejection, RejectCode};

type R<T> = Result<T, QueryRejection>;

/// システム擬似列（`_id` はレコードの外部 ID）。
const PSEUDO: &[&str] = &["_id", "_owner", "_created_at", "_updated_at"];

/// 名前で開いたテーブル（閲覧者に見えるもの）。
pub(crate) struct TableInfo<'a> {
    pub id: Uuid,
    pub schema: &'a TableSchema,
    pub masked: &'a HashSet<String>,
}

/// 出力の列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutKind {
    /// 表 `src`（0 は FROM）の列。`key` は参照名（擬似列を含む）。
    Field { src: usize, key: String },
    /// 集計のグループ化の列（`group_by` の添字）。
    Group(usize),
    /// 集計指標（`aggregate` の添字）。
    Metric(usize),
}

/// 出力の列（名前と取り出し方）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutCol {
    pub name: String,
    pub kind: OutKind,
}

/// IR へ変換した結果。
#[derive(Debug, Clone)]
pub(crate) struct Compiled {
    pub table_id: Uuid,
    pub query: DataQuery,
    pub columns: Vec<OutCol>,
    /// 結合の IR 上の別名（`joins[i]` → `j{i+1}`）。
    pub join_aliases: Vec<String>,
}

fn fold_case(s: &str, quoted: bool) -> String {
    if quoted {
        s.to_string()
    } else {
        s.to_lowercase()
    }
}

struct Resolver<'a> {
    tables: &'a [TableInfo<'a>],
    /// 修飾名（別名かテーブル名）→ 表の添字。
    quals: Vec<String>,
}

impl Resolver<'_> {
    /// 表 `i` の見える列（擬似列を含む）。
    fn visible(&self, i: usize) -> Vec<String> {
        let t = &self.tables[i];
        charge(PSEUDO.len() + t.schema.fields.len());
        let mut v: Vec<String> = PSEUDO.iter().map(ToString::to_string).collect();
        v.extend(
            t.schema
                .fields
                .iter()
                .filter(|f| !t.masked.contains(&f.key))
                .map(|f| f.key.clone()),
        );
        v
    }

    /// 表 `i` の見える列から名前を探す（引用符なしは大文字小文字を区別しない）。
    fn find(&self, i: usize, name: &str, quoted: bool) -> Option<String> {
        let want = fold_case(name, quoted);
        self.visible(i)
            .into_iter()
            .find(|k| fold_case(k, quoted) == want || k == name)
    }

    fn unknown_col(&self, c: &Col, scope: &[usize]) -> QueryRejection {
        let mut expected = Vec::new();
        for &i in scope {
            for k in self.visible(i) {
                let shown = if self.tables.len() > 1 {
                    format!("{}.{k}", self.quals[i])
                } else {
                    k
                };
                expected.push(shown);
            }
        }
        let shown = match &c.qual {
            Some(q) => format!("{q}.{}", c.name),
            None => c.name.clone(),
        };
        QueryRejection::unknown_name(NameKind::Column, &shown, expected).at(c.loc)
    }

    fn qual(&self, q: &str, loc: Loc) -> R<usize> {
        charge(self.quals.len());
        self.quals
            .iter()
            .position(|x| x == q || x.to_lowercase() == q.to_lowercase())
            .ok_or_else(|| {
                QueryRejection::unknown_name(NameKind::Table, q, self.quals.clone()).at(loc)
            })
    }

    /// 列を（表, 参照名）へ解決する（見えない列は存在しない列と同じ応答）。
    fn col(&self, c: &Col) -> R<(usize, String)> {
        if let Some(q) = &c.qual {
            let i = self.qual(q, c.loc)?;
            return self
                .find(i, &c.name, c.quoted)
                .map(|k| (i, k))
                .ok_or_else(|| self.unknown_col(c, &[i]));
        }
        let hits: Vec<(usize, String)> = (0..self.tables.len())
            .filter_map(|i| self.find(i, &c.name, c.quoted).map(|k| (i, k)))
            .collect();
        match hits.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(self.unknown_col(c, &(0..self.tables.len()).collect::<Vec<_>>())),
            _ => Err(QueryRejection::new(
                RejectCode::InvalidQuery,
                format!(
                    "列 '{}' が複数のテーブルにあります（別名.列 で書いてください）",
                    c.name
                ),
            )
            .at(c.loc)),
        }
    }

    /// 数値として扱う列か。
    fn numeric(&self, i: usize, key: &str) -> bool {
        self.tables[i].schema.field(key).is_some_and(|f| {
            f.field_type == FieldType::Number
                || f.result_type == Some(crate::derived::types::ResultType::Number)
        })
    }
}

/// `SELECT` を IR へ変換する（`tables[0]` は FROM・`tables[i]` は `joins[i-1]`）。
#[allow(clippy::too_many_lines)] // 句ごとの変換を 1 か所で読めるようにする。
pub(crate) fn compile(
    sel: &Select,
    tables: &[TableInfo<'_>],
    max_join_depth: usize,
) -> R<Compiled> {
    if sel.joins.len() > max_join_depth {
        return Err(QueryRejection::limit(
            "max_join_depth",
            max_join_depth as u64,
        ));
    }
    let mut quals: Vec<String> = Vec::with_capacity(tables.len());
    quals.push(sel.from.qualifier().to_string());
    for j in &sel.joins {
        let q = j.table.qualifier().to_string();
        if quals.iter().any(|x| x.to_lowercase() == q.to_lowercase()) {
            return Err(QueryRejection::new(
                RejectCode::InvalidJoin,
                format!("別名 '{q}' が重複しています（自己結合では別名を付けてください）"),
            )
            .at(j.loc));
        }
        quals.push(q);
    }
    let rs = Resolver {
        tables,
        quals: quals.clone(),
    };
    let join_aliases: Vec<String> = (1..=sel.joins.len()).map(|i| format!("j{i}")).collect();
    let ir_alias =
        |src: usize| -> Option<String> { (src > 0).then(|| join_aliases[src - 1].clone()) };

    // JOIN の束縛（宣言済みリンク列の等値 1 個だけ）。
    let mut joins = Vec::with_capacity(sel.joins.len());
    for (n, j) in sel.joins.iter().enumerate() {
        let new = n + 1;
        let bad = |msg: &str| {
            QueryRejection::new(RejectCode::InvalidJoin, msg.to_string())
                .at(j.loc)
                .hint("例: FROM orders o JOIN customers c ON o.customer = c._id（o.customer は customers を参照するリンク列）")
        };
        let side = |c: &Col| -> R<(usize, String)> {
            let (i, k) = rs.col(c)?;
            Ok((i, k))
        };
        let (li, lk) = side(&j.left)?;
        let (ri, rk) = side(&j.right)?;
        if li == ri || (li != new && ri != new) || li > new || ri > new {
            return Err(bad(
                "ON は結合するテーブルと、それより前のテーブルを 1 本のリンク列で結んでください",
            ));
        }
        let (old, link_side, link_key) = match (lk == "_id", rk == "_id") {
            (false, true) => (if li == new { ri } else { li }, li, lk.clone()),
            (true, false) => (if ri == new { li } else { ri }, ri, rk.clone()),
            _ => {
                return Err(bad(
                    "ON は「リンク列 = 参照先の _id」の等値 1 個で書いてください",
                ))
            }
        };
        let link_table = &tables[link_side];
        let Some(f) = link_table.schema.field(&link_key) else {
            return Err(bad("リンク列が見つかりません"));
        };
        let other = if link_side == new { old } else { new };
        if f.field_type != FieldType::RecordRef || f.ref_table != Some(tables[other].id) {
            return Err(bad(&format!(
                "'{link_key}' はこの結合先を参照する宣言済みのリンク列ではありません"
            )));
        }
        // 結合元（old）から辿る列: 順方向はリンク列そのもの・逆方向は参照先に置いた対称列。
        let field = if link_side == old {
            link_key.clone()
        } else {
            let sym = f
                .link
                .as_ref()
                .and_then(|l| l.symmetric_field)
                .and_then(|id| tables[old].schema.field_by_id(id))
                .filter(|s| !tables[old].masked.contains(&s.key));
            match sym {
                Some(s) => s.key.clone(),
                None => {
                    return Err(bad(&format!(
                        "逆向きの結合には対称列が要ります（'{link_key}' には参照先に対称列がありません。{}.{link_key} = … の向きで書いてください）",
                        quals[new]
                    )))
                }
            }
        };
        joins.push(JoinSpec {
            alias: join_aliases[n].clone(),
            from: ir_alias(old),
            field,
            filter: None,
        });
    }

    // WHERE: 最上位の AND を表ごとに振り分け、3 値論理 → 2 値論理へ翻訳する。
    let mut per_table: Vec<Vec<Condition>> = vec![Vec::new(); tables.len()];
    if let Some(f) = &sel.filter {
        let conj: Vec<&BExpr> = match f {
            BExpr::And(v) => v.iter().collect(),
            other => vec![other],
        };
        for c in conj {
            let mut cs = Vec::new();
            cols(c, &mut cs);
            let mut srcs = Vec::new();
            for col in &cs {
                let (i, k) = rs.col(col)?;
                let _ = i;
                if k == "_id" {
                    return Err(QueryRejection::new(
                        RejectCode::Unsupported,
                        "_id では絞り込めません（_id は射影と JOIN の ON にだけ使えます）",
                    )
                    .at(col.loc));
                }
                if !srcs.contains(&i) {
                    srcs.push(i);
                }
            }
            if srcs.len() > 1 {
                return Err(QueryRejection::new(
                    RejectCode::InvalidQuery,
                    "1 つの条件（OR / NOT の中）に複数のテーブルの列は混ぜられません（AND で分けてください）",
                ));
            }
            let src = srcs.first().copied().unwrap_or(0);
            let mut resolve = |col: &Col| -> R<RCol> {
                let (i, key) = rs.col(col)?;
                Ok(RCol {
                    numeric: rs.numeric(i, &key),
                    key,
                })
            };
            let translated = Translator {
                resolve: &mut resolve,
            }
            .tr(c, true)?;
            per_table[src].push(translated);
        }
    }
    let merge = |v: Vec<Condition>| -> Option<Condition> {
        match v.len() {
            0 => None,
            1 => v.into_iter().next(),
            _ => Some(Condition::All(v)),
        }
    };
    let mut per_table = per_table.into_iter();
    let base_filter = per_table.next().and_then(merge);
    for (j, conds) in joins.iter_mut().zip(per_table) {
        j.filter = merge(conds);
    }

    // 射影の別名（ORDER BY から参照できる）。
    let alias_of: HashMap<String, usize> = sel
        .items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| match it {
            Item::Col { alias: Some(a), .. } | Item::Agg { alias: Some(a), .. } => {
                Some((a.to_lowercase(), i))
            }
            _ => None,
        })
        .collect();
    let ir_key = |src: usize, key: &str| -> String {
        match ir_alias(src) {
            Some(a) => format!("{a}.{key}"),
            None => key.to_string(),
        }
    };
    let is_aggregate = !sel.group_by.is_empty()
        || sel.having.is_some()
        || sel.items.iter().any(|i| matches!(i, Item::Agg { .. }));

    let mut q = DataQuery {
        filter: base_filter,
        joins,
        ..DataQuery::default()
    };
    let mut columns = Vec::with_capacity(sel.items.len());

    if is_aggregate {
        let mut group_keys: Vec<String> = Vec::new();
        for g in &sel.group_by {
            let (i, k) = rs.col(g)?;
            if PSEUDO.contains(&k.as_str()) {
                // SQL としては妥当（部分集合の制限）なので unsupported。
                return Err(QueryRejection::new(
                    RejectCode::Unsupported,
                    format!("'{k}' ではグループ化できません"),
                )
                .hint("グループ化には利用者の列（名前など）を使ってください")
                .at(g.loc));
            }
            let key = ir_key(i, &k);
            if !group_keys.contains(&key) {
                group_keys.push(key);
            }
        }
        let mut metrics: Vec<MetricSpec> = Vec::new();
        let spec_of = |a: &Agg| -> R<MetricSpec> {
            let field = match &a.arg {
                None => None,
                Some(c) => {
                    let (i, k) = rs.col(c)?;
                    if k == "_id" && a.func == Metric::Count {
                        None
                    } else if a.func == Metric::Count {
                        return Err(QueryRejection::new(
                            RejectCode::Unsupported,
                            "COUNT(列) は使えません（COUNT(*) を使ってください。値のある行だけ数えるなら WHERE 列 IS NOT NULL）",
                        )
                        .at(a.loc));
                    } else {
                        Some(ir_key(i, &k))
                    }
                }
            };
            Ok(MetricSpec {
                metric: a.func,
                field,
            })
        };
        let metric_of = |a: &Agg, metrics: &mut Vec<MetricSpec>| -> R<usize> {
            let m = spec_of(a)?;
            if let Some(i) = metrics.iter().position(|x| *x == m) {
                return Ok(i);
            }
            metrics.push(m);
            Ok(metrics.len() - 1)
        };
        for it in &sel.items {
            match it {
                Item::Col { col, alias } => {
                    let (i, k) = rs.col(col)?;
                    let key = ir_key(i, &k);
                    let g = group_keys.iter().position(|x| *x == key).ok_or_else(|| {
                        QueryRejection::new(
                            RejectCode::InvalidQuery,
                            format!("列 '{}' は GROUP BY に無いので射影できません（GROUP BY に足すか集計関数で包んでください）", col.name),
                        )
                        .at(col.loc)
                    })?;
                    columns.push(OutCol {
                        name: alias.clone().unwrap_or_else(|| col.name.clone()),
                        kind: OutKind::Group(g),
                    });
                }
                Item::Agg { agg, alias } => {
                    let m = metric_of(agg, &mut metrics)?;
                    columns.push(OutCol {
                        name: alias.clone().unwrap_or_else(|| agg_name(agg)),
                        kind: OutKind::Metric(m),
                    });
                }
            }
        }
        // HAVING（集計値 × リテラル・3 値論理の翻訳は集計値が値なし＝抑制のときも同じ）。
        let having = match &sel.having {
            None => None,
            Some(h) => Some(having(h, true, &mut |a| spec_of(a))?),
        };
        let mut group_sort = Vec::new();
        for (t, desc, loc) in &sel.order_by {
            let target = match t {
                OrdTarget::Col(c)
                    if c.qual.is_none() && alias_of.contains_key(&c.name.to_lowercase()) =>
                {
                    item_target(&sel.items, alias_of[&c.name.to_lowercase()])
                }
                OrdTarget::Pos(p) => {
                    if *p == 0 || *p > sel.items.len() {
                        return Err(QueryRejection::new(
                            RejectCode::InvalidQuery,
                            format!("ORDER BY {p} は射影の範囲外です"),
                        )
                        .at(*loc));
                    }
                    item_target(&sel.items, p - 1)
                }
                other => other.clone(),
            };
            let key = match target {
                OrdTarget::Col(c) => {
                    let (i, k) = rs.col(&c)?;
                    let key = ir_key(i, &k);
                    if !group_keys.contains(&key) {
                        return Err(QueryRejection::new(
                            RejectCode::InvalidQuery,
                            format!("ORDER BY の '{}' は GROUP BY にありません", c.name),
                        )
                        .at(*loc));
                    }
                    GroupSortKey {
                        field: Some(key),
                        metric: None,
                        descending: *desc,
                    }
                }
                OrdTarget::Agg(a) => GroupSortKey {
                    field: None,
                    metric: Some(spec_of(&a)?),
                    descending: *desc,
                },
                OrdTarget::Pos(_) => unreachable_pos(*loc)?,
            };
            group_sort.push(key);
        }
        q.group_by = group_keys;
        q.aggregate = metrics;
        if q.aggregate.is_empty() {
            // GROUP BY だけ（射影が列だけ）でもグループを返すために件数を数える（出力しない）。
            q.aggregate.push(MetricSpec {
                metric: Metric::Count,
                field: None,
            });
        }
        q.having = having;
        q.group_sort = group_sort;
        q.group_limit = sel.limit.map(|n| {
            u32::try_from(n.min(u64::from(MAX_GROUP_LIMIT)))
                .unwrap_or(MAX_GROUP_LIMIT)
                .max(1)
        });
    } else {
        let mut select: Vec<String> = Vec::new();
        for it in &sel.items {
            let Item::Col { col, alias } = it else {
                continue;
            };
            let (i, k) = rs.col(col)?;
            if !PSEUDO.contains(&k.as_str()) || i > 0 {
                let key = ir_key(i, &k);
                if !select.contains(&key) {
                    select.push(key);
                }
            }
            columns.push(OutCol {
                name: alias.clone().unwrap_or_else(|| col.name.clone()),
                kind: OutKind::Field { src: i, key: k },
            });
        }
        q.select = Some(select);
        for (t, desc, loc) in &sel.order_by {
            let target = match t {
                OrdTarget::Col(c)
                    if c.qual.is_none() && alias_of.contains_key(&c.name.to_lowercase()) =>
                {
                    item_target(&sel.items, alias_of[&c.name.to_lowercase()])
                }
                OrdTarget::Pos(p) if *p >= 1 && *p <= sel.items.len() => {
                    item_target(&sel.items, p - 1)
                }
                other => other.clone(),
            };
            let OrdTarget::Col(c) = target else {
                return Err(QueryRejection::new(
                    RejectCode::InvalidQuery,
                    "集計しない問い合わせの ORDER BY には列を書いてください",
                )
                .at(*loc));
            };
            let (i, k) = rs.col(&c)?;
            if i != 0 {
                return Err(QueryRejection::new(
                    RejectCode::Unsupported,
                    "結合先の列では並べ替えられません（FROM のテーブルの列で並べてください）",
                )
                .at(*loc));
            }
            if k == "_id" {
                return Err(QueryRejection::new(
                    RejectCode::Unsupported,
                    "_id では並べ替えられません",
                )
                .at(*loc));
            }
            q.sort.push(SortKey {
                field: k,
                descending: *desc,
            });
        }
        q.limit = sel
            .limit
            .map(|n| u32::try_from(n.min(u64::from(MAX_LIMIT))).unwrap_or(MAX_LIMIT));
    }
    Ok(Compiled {
        table_id: tables[0].id,
        query: q,
        columns,
        join_aliases,
    })
}

fn unreachable_pos<T>(loc: Loc) -> R<T> {
    Err(QueryRejection::new(RejectCode::InvalidQuery, "ORDER BY の位置が不正です").at(loc))
}

fn item_target(items: &[Item], i: usize) -> OrdTarget {
    match &items[i] {
        Item::Col { col, .. } => OrdTarget::Col(col.clone()),
        Item::Agg { agg, .. } => OrdTarget::Agg(agg.clone()),
    }
}

fn agg_name(a: &Agg) -> String {
    let f = match a.func {
        Metric::Count => "count",
        Metric::Sum => "sum",
        Metric::Avg => "avg",
        Metric::Min => "min",
        Metric::Max => "max",
    };
    match &a.arg {
        None => format!("{f}(*)"),
        Some(c) => match &c.qual {
            Some(q) => format!("{f}({q}.{})", c.name),
            None => format!("{f}({})", c.name),
        },
    }
}

/// HAVING の翻訳（3 値論理: 抑制・値なしの集計値は UNKNOWN で、`T` にも `F` にも入らない）。
fn having(e: &BExpr, want: bool, metric: &mut dyn FnMut(&Agg) -> R<MetricSpec>) -> R<HavingCond> {
    Ok(match e {
        BExpr::And(v) | BExpr::Or(v) => {
            let kids = v
                .iter()
                .map(|x| having(x, want, metric))
                .collect::<R<Vec<_>>>()?;
            if matches!(e, BExpr::And(_)) == want {
                HavingCond::All(kids)
            } else {
                HavingCond::Any(kids)
            }
        }
        BExpr::Not(x) => having(x, !want, metric)?,
        BExpr::Const(b) => {
            if *b == want {
                HavingCond::All(Vec::new())
            } else {
                HavingCond::Any(Vec::new())
            }
        }
        BExpr::Cmp { lhs, op, rhs, loc } => {
            let (agg, op, lit) = match (lhs, rhs) {
                (Operand::Agg(a), Operand::Lit(l)) => (a, *op, l),
                (Operand::Lit(l), Operand::Agg(a)) => (a, flip_cmp(*op), l),
                _ => {
                    return Err(QueryRejection::new(
                        RejectCode::Unsupported,
                        "HAVING に書けるのは「集計関数 比較 数値」だけです",
                    )
                    .at(*loc))
                }
            };
            let m = metric(agg)?;
            let rc = RCol {
                key: agg_name(agg),
                numeric: true,
            };
            let v = value_for(&rc, lit, *loc)?;
            let serde_json::Value::Number(n) = v else {
                return Err(QueryRejection::new(
                    RejectCode::InvalidQuery,
                    "HAVING の比較値は数値です",
                )
                .at(*loc));
            };
            use super::ast::Cmp as C;
            // T: そのまま・F: 値がある（抑制されていない）ときの否定（値なしはどちらにも入らない）。
            let op = match (op, want) {
                (C::Eq, true) | (C::Ne, false) => CondOp::Eq,
                (C::Ne, true) | (C::Eq, false) => CondOp::Neq,
                (C::Lt, true) | (C::Ge, false) => CondOp::Lt,
                (C::Le, true) | (C::Gt, false) => CondOp::Lte,
                (C::Gt, true) | (C::Le, false) => CondOp::Gt,
                (C::Ge, true) | (C::Lt, false) => CondOp::Gte,
            };
            HavingCond::Cmp(HavingCmp {
                metric: m,
                op,
                value: n,
            })
        }
        _ => {
            return Err(QueryRejection::new(
                RejectCode::Unsupported,
                "HAVING に書けるのは集計関数の比較と AND / OR / NOT だけです",
            ))
        }
    })
}

fn flip_cmp(op: super::ast::Cmp) -> super::ast::Cmp {
    use super::ast::Cmp as C;
    match op {
        C::Lt => C::Gt,
        C::Le => C::Ge,
        C::Gt => C::Lt,
        C::Ge => C::Le,
        o => o,
    }
}
