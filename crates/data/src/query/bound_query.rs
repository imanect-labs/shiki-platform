//! クエリ全体の束縛（[`super::bound`] の入口）と、結合・集計後の条件の束縛、結合の半結合化。
//!
//! - **結合（JOIN）**: 宣言済みリンク列だけを辿る。結合元のリンク列は `(結合元の表, field_id, 参照先の表)` が
//!   スキーマの宣言（`ref_table`・`LinkDef`）と一致するものに限られ（ここでは列の参照名で受けるので、宣言外の
//!   組は作れない）、カーディナリティも `LinkDef` から取る。参照先の行述語は降下で必ず合成する（PIT-20）。
//! - **半結合化（data-platform §16.2）**: 結合先の列をどこにも使っておらず、結合元 1 行から辿れる結合先が
//!   高々 1 件（`LinkDef` の src 側が単値）と証明できる結合だけを、参照先の行述語つきの `EXISTS`
//!   （[`Expr::Link`]）へ書き換える。**除去ではない**（参照先が無い行は除外されるまま）。多対一以外の結合は
//!   行を重複させ件数と集計値を変えるので、書き換えずに残す。
//! - **HAVING / 集計結果の並べ替え**: 指標を参照する。`aggregate` に無い指標は末尾に足して計算し、
//!   出力からは外す（`hidden_metrics`）。

use std::cell::RefCell;
use std::collections::HashSet;

use super::bound::{
    pseudo, pseudo_updated_at, BField, BHaving, BSort, Binder, BoundJoin, BoundQuery, ColRef, Expr,
    FieldKind, GroupKey, LinkCtx, LinkExpr,
};
use super::ir::{
    CondOp, DataQuery, HavingCond, Metric, MetricSpec, DEFAULT_LIMIT, MAX_GROUP_LIMIT, MAX_LIMIT,
    MAX_SORT_KEYS,
};
use super::limits::MAX_JOIN_DEPTH;
use super::planner::normalize::{normalize, Rules};
use super::scopes::ScopeTables;
use crate::field_key::is_valid_field_key;
use crate::index::SortPrecision;
use crate::model::{FieldType, TableSchema};
use crate::DataError;

/// 集計指標の上限（`aggregate` ＋ HAVING・並べ替えのために足したもの）。
const MAX_METRICS: usize = 8;
const MAX_METRICS_WITH_HIDDEN: usize = 12;

/// 束縛の選択。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BindOpts {
    /// 結合・半結合の段数の上限。
    pub max_join_depth: usize,
    /// 証明できる結合を半結合へ書き換える（検証用に止められる・既定 true）。
    pub semijoin: bool,
}

impl Default for BindOpts {
    fn default() -> Self {
        BindOpts {
            max_join_depth: MAX_JOIN_DEPTH,
            semijoin: true,
        }
    }
}

/// クエリ全体を束縛する（リンクの半結合・結合を使わない経路・検証用）。
#[cfg(test)]
pub(crate) fn bind_query(
    schema: &TableSchema,
    masked: &HashSet<String>,
    q: &DataQuery,
) -> Result<BoundQuery, DataError> {
    bind_query_scoped(
        schema,
        masked,
        q,
        &ScopeTables::default(),
        BindOpts::default(),
    )
}

/// 結合 1 つの束縛途中の状態（結合先の束縛器を最後まで持つ）。
struct JoinB<'a> {
    alias: String,
    from: usize,
    field: crate::model::FieldId,
    scope: usize,
    binder: Binder<'a>,
    filter: Expr,
    at_most_one: bool,
    select: Vec<String>,
}

/// クエリ全体を束縛する（参照先テーブルは `tables` に読み込み済み）。
#[allow(clippy::too_many_lines)] // 束縛の順序（条件 → ソート → 結合 → 集計 → 射影）を 1 か所で読めるようにする。
pub(crate) fn bind_query_scoped(
    schema: &TableSchema,
    masked: &HashSet<String>,
    q: &DataQuery,
    tables: &ScopeTables,
    opts: BindOpts,
) -> Result<BoundQuery, DataError> {
    if q.offset.is_some_and(|o| o != 0) {
        return Err(DataError::Invalid(
            "offset は廃止されました。next_cursor を cursor に渡してください".into(),
        ));
    }
    let ctx = RefCell::new(LinkCtx {
        tables,
        scopes: Vec::new(),
        max_depth: opts.max_join_depth,
    });
    let mut b = Binder::scoped(schema, masked, &ctx, 0);
    let mut parts = Vec::new();
    if let Some(c) = &q.filter {
        parts.push(b.condition(c, 0)?);
    }
    if let Some(text) = q.search.as_deref().filter(|s| !s.is_empty()) {
        if text.chars().count() > 256 {
            return Err(DataError::Invalid(
                "search が長すぎます（最大 256 文字）".into(),
            ));
        }
        parts.push(b.search(text, q.search_fields.as_deref())?);
    }
    let mut filter = match parts.len() {
        0 => Expr::True,
        1 => parts.pop().unwrap_or(Expr::True),
        _ => Expr::And(parts),
    };

    if q.sort.len() > MAX_SORT_KEYS {
        return Err(DataError::Invalid(format!(
            "ソートキーが多すぎます（最大 {MAX_SORT_KEYS}）"
        )));
    }
    let mut sort = Vec::with_capacity(q.sort.len());
    let mut precision = SortPrecision::Exact;
    let mut seen = HashSet::new();
    for k in &q.sort {
        if k.field.contains('.') {
            return Err(DataError::Invalid(format!(
                "ソートキー '{}': 結合先の列では並べ替えられません（FROM の表の列を指定してください）",
                k.field
            )));
        }
        let f = b.queryable(&k.field)?;
        if f.multi() {
            return Err(DataError::Invalid(format!(
                "フィールド '{}' はソートに使えません（多値・リンク）",
                k.field
            )));
        }
        if !seen.insert(f.id) {
            return Err(DataError::Invalid(format!(
                "ソートキー '{}' が重複しています",
                k.field
            )));
        }
        if f.ty == Some(FieldType::Text) {
            precision = SortPrecision::Prefix;
        }
        // 材料化した派生列は複数ソートの複合索引を持たない（第 1 キーか単独のソートだけ）。
        if f.derived && q.sort.len() > 1 {
            return Err(DataError::Invalid(format!(
                "派生フィールド '{}' は複数キーのソートに使えません",
                k.field
            )));
        }
        sort.push(BSort {
            field: f.id,
            desc: k.descending,
        });
    }
    if sort.is_empty() && q.aggregate.is_empty() {
        let p = pseudo_updated_at();
        sort.push(BSort {
            field: p.id,
            desc: true,
        });
        b.register(p);
    }

    // 結合（宣言済みリンク列だけを辿る）。
    if q.joins.len() > opts.max_join_depth {
        return Err(DataError::Rejected(Box::new(
            super::reject::QueryRejection::limit("max_join_depth", opts.max_join_depth as u64),
        )));
    }
    let mut joins: Vec<JoinB<'_>> = Vec::with_capacity(q.joins.len());
    for j in &q.joins {
        if !is_valid_field_key(&j.alias) || joins.iter().any(|x| x.alias == j.alias) {
            return Err(DataError::Invalid(format!(
                "結合の別名 '{}' が不正か重複しています（英小文字・数字・_）",
                j.alias
            )));
        }
        let from = match &j.from {
            None => 0,
            Some(a) => {
                joins.iter().position(|x| &x.alias == a).ok_or_else(|| {
                    DataError::Invalid(format!("結合元の別名 '{a}' は先に結合されていません"))
                })? + 1
            }
        };
        let depth = depth_of(&joins, from) + 1;
        if depth > opts.max_join_depth {
            return Err(DataError::Rejected(Box::new(
                super::reject::QueryRejection::limit("max_join_depth", opts.max_join_depth as u64),
            )));
        }
        let parent: &Binder<'_> = if from == 0 {
            &b
        } else {
            &joins[from - 1].binder
        };
        let (f, st) = parent.link_target(&j.field)?;
        let at_most_one = f.link.as_ref().is_none_or(|l| l.cardinality.src_single());
        let mut child = Binder::scoped(&st.table.schema, &st.masked, &ctx, depth);
        let jf = match &j.filter {
            Some(c) => child.condition(c, 0)?,
            None => Expr::True,
        };
        // 結合先のスコープの席を先に取る（列は束縛の最後に確定する）。
        let scope = {
            let mut c = ctx.borrow_mut();
            c.scopes.push(super::bound::BoundScope {
                table: st.table.id,
                schema_version: st.table.schema_version,
                q: BoundQuery::fields_only(std::collections::BTreeMap::default()),
            });
            c.scopes.len() - 1
        };
        joins.push(JoinB {
            alias: j.alias.clone(),
            from,
            field: f.id,
            scope,
            binder: child,
            filter: jf,
            at_most_one,
            select: Vec::new(),
        });
    }

    // 集計の列（`別名.参照名` は結合先）。
    let mut group_by = Vec::new();
    for g in &q.group_by {
        let c = col_ref(&mut b, &mut joins, g)?;
        let f = col_field(&b, &joins, c);
        if f.multi() || f.is_pseudo() {
            return Err(DataError::Invalid(format!(
                "フィールド '{g}' は集計/グループ化に使えません"
            )));
        }
        group_by.push(c);
    }
    if !group_by.is_empty() && q.aggregate.is_empty() {
        return Err(DataError::Invalid(
            "group_by には aggregate の指定が必要です".into(),
        ));
    }
    let mut metrics = Vec::new();
    for m in &q.aggregate {
        metrics.push(bind_metric(&mut b, &mut joins, m)?);
    }
    if metrics.len() > MAX_METRICS {
        return Err(DataError::Invalid("集計指標が多すぎます（最大 8）".into()));
    }
    let visible = metrics.len();
    let aggregate = visible > 0;
    if !aggregate && (q.having.is_some() || !q.group_sort.is_empty() || q.group_limit.is_some()) {
        return Err(DataError::Invalid(
            "having / group_sort / group_limit には aggregate の指定が必要です".into(),
        ));
    }
    let having = match &q.having {
        None => None,
        Some(h) => Some(bind_having(&mut b, &mut joins, &mut metrics, h, 0)?),
    };
    let mut group_sort = Vec::with_capacity(q.group_sort.len());
    for k in &q.group_sort {
        let key = match (&k.field, &k.metric) {
            (Some(name), None) => {
                let c = col_ref(&mut b, &mut joins, name)?;
                let i = group_by.iter().position(|g| *g == c).ok_or_else(|| {
                    DataError::Invalid(format!("並べ替えの '{name}' は group_by にありません"))
                })?;
                GroupKey::Group(i)
            }
            (None, Some(m)) => GroupKey::Metric(metric_index(&mut b, &mut joins, &mut metrics, m)?),
            _ => {
                return Err(DataError::Invalid(
                    "group_sort の各キーには field か metric のどちらか一方を指定してください"
                        .into(),
                ))
            }
        };
        group_sort.push((key, k.descending));
    }
    if metrics.len() > MAX_METRICS_WITH_HIDDEN {
        return Err(DataError::Invalid(
            "HAVING・並べ替えに使う集計指標が多すぎます".into(),
        ));
    }

    // 射影（`別名.参照名` は結合先・擬似列も可）。
    let select = match &q.select {
        None => None,
        Some(keys) => {
            let mut base = Vec::new();
            for k in keys {
                if let Some((alias, key)) = k.split_once('.') {
                    let j = joins.iter_mut().find(|j| j.alias == alias).ok_or_else(|| {
                        DataError::Invalid(format!("別名 '{alias}' は結合にありません"))
                    })?;
                    if pseudo(key).is_none() && key != "_id" {
                        if j.binder.masked().contains(key) {
                            return Err(DataError::Forbidden);
                        }
                        if j.binder.schema().field(key).is_none() {
                            return Err(DataError::Invalid(format!(
                                "フィールド '{k}' はスキーマに存在しません"
                            )));
                        }
                    }
                    j.select.push(key.to_string());
                } else {
                    if schema.field(k).is_none() {
                        return Err(DataError::Invalid(format!(
                            "フィールド '{k}' はスキーマに存在しません"
                        )));
                    }
                    base.push(k.clone());
                }
            }
            Some(base.into_iter().collect())
        }
    };

    // 結合の半結合化（使っていない・高々 1 件の結合だけ）と、残す結合の確定。
    let used: Vec<bool> = (0..joins.len())
        .map(|i| {
            !joins[i].select.is_empty()
                || group_by.iter().any(|c| c.src == i + 1)
                || metrics
                    .iter()
                    .any(|(_, c)| c.is_some_and(|c| c.src == i + 1))
        })
        .collect();
    let n = joins.len();
    let mut keep = vec![true; n];
    let mut links: Vec<Vec<Expr>> = vec![Vec::new(); n + 1];
    for i in (0..n).rev() {
        let has_kept_child = (i + 1..n).any(|k| keep[k] && joins[k].from == i + 1);
        let mine = std::mem::take(&mut links[i + 1]);
        let jf = and_all(std::mem::replace(&mut joins[i].filter, Expr::True), mine);
        let jf = normalize(jf, joins[i].binder.fields(), Rules::ALL);
        if opts.semijoin && !used[i] && joins[i].at_most_one && !has_kept_child {
            keep[i] = false;
            let from = joins[i].from;
            links[from].push(Expr::Link(Box::new(LinkExpr {
                field: joins[i].field,
                scope: joins[i].scope,
                filter: jf,
            })));
        } else {
            joins[i].filter = jf;
        }
    }
    filter = and_all(filter, std::mem::take(&mut links[0]));

    // 残した結合の添字を詰め直す（使われている結合だけが列から参照される）。
    let mut remap = vec![0usize; n + 1];
    let mut next = 1;
    for i in 0..n {
        if keep[i] {
            remap[i + 1] = next;
            next += 1;
        }
    }
    let fix = |c: ColRef| ColRef {
        src: remap[c.src],
        field: c.field,
    };
    let group_by: Vec<ColRef> = group_by.into_iter().map(fix).collect();
    let metrics: Vec<(Metric, Option<ColRef>)> =
        metrics.into_iter().map(|(m, c)| (m, c.map(fix))).collect();
    let hidden_metrics = metrics.len() - visible;

    let mut bound_joins = Vec::new();
    let mut scope_fields = Vec::new();
    for (i, j) in joins.into_iter().enumerate() {
        scope_fields.push((j.scope, j.binder.into_fields()));
        if keep[i] {
            bound_joins.push(BoundJoin {
                alias: j.alias,
                from: remap[j.from],
                field: j.field,
                scope: j.scope,
                filter: j.filter,
                at_most_one: j.at_most_one,
                select: j.select,
            });
        }
    }
    let mut scopes = std::mem::take(&mut ctx.borrow_mut().scopes);
    for (s, fields) in scope_fields {
        if let Some(sc) = scopes.get_mut(s) {
            sc.q = BoundQuery::fields_only(fields);
        }
    }
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    Ok(BoundQuery {
        fields: b.into_fields(),
        filter,
        sort,
        group_by,
        metrics,
        hidden_metrics,
        having,
        group_sort,
        group_limit: q.group_limit.map(|n| n.clamp(1, MAX_GROUP_LIMIT)),
        joins: bound_joins,
        select,
        limit,
        with_count: q.with_count,
        sort_precision: precision,
        scopes,
    })
}

fn and_all(e: Expr, more: Vec<Expr>) -> Expr {
    if more.is_empty() {
        return e;
    }
    let mut v = vec![e];
    v.extend(more);
    Expr::And(v)
}

/// 結合の段数（結合元を辿った数）。
fn depth_of(joins: &[JoinB<'_>], src: usize) -> usize {
    let mut d = 0;
    let mut s = src;
    while s > 0 {
        d += 1;
        s = joins[s - 1].from;
    }
    d
}

/// `参照名` または `別名.参照名` を集計に使える列として束縛する。
fn col_ref(b: &mut Binder<'_>, joins: &mut [JoinB<'_>], name: &str) -> Result<ColRef, DataError> {
    match name.split_once('.') {
        None => Ok(ColRef::base(b.queryable(name)?.id)),
        Some((alias, key)) => {
            let i = joins
                .iter()
                .position(|j| j.alias == alias)
                .ok_or_else(|| DataError::Invalid(format!("別名 '{alias}' は結合にありません")))?;
            let f = joins[i].binder.queryable(key)?;
            Ok(ColRef {
                src: i + 1,
                field: f.id,
            })
        }
    }
}

fn col_field<'b>(b: &'b Binder<'_>, joins: &'b [JoinB<'_>], c: ColRef) -> &'b BField {
    let fields = if c.src == 0 {
        b.fields()
    } else {
        joins[c.src - 1].binder.fields()
    };
    fields.get(&c.field).unwrap_or_else(|| unreachable_field())
}

fn unreachable_field() -> &'static BField {
    static F: std::sync::LazyLock<BField> = std::sync::LazyLock::new(pseudo_updated_at);
    &F
}

fn bind_metric(
    b: &mut Binder<'_>,
    joins: &mut [JoinB<'_>],
    m: &MetricSpec,
) -> Result<(Metric, Option<ColRef>), DataError> {
    if m.metric == Metric::Count {
        return Ok((Metric::Count, None));
    }
    let name = m
        .field
        .as_ref()
        .ok_or_else(|| DataError::Invalid("この集計には field（number）が必要です".into()))?;
    let c = col_ref(b, joins, name)?;
    if col_field(b, joins, c).kind != FieldKind::Num {
        return Err(DataError::Invalid(format!(
            "集計対象 '{name}' は number である必要があります"
        )));
    }
    Ok((m.metric, Some(c)))
}

/// 指標の添字（`aggregate` に無ければ出力しない指標として足す）。
fn metric_index(
    b: &mut Binder<'_>,
    joins: &mut [JoinB<'_>],
    metrics: &mut Vec<(Metric, Option<ColRef>)>,
    m: &MetricSpec,
) -> Result<usize, DataError> {
    let bm = bind_metric(b, joins, m)?;
    if let Some(i) = metrics.iter().position(|x| *x == bm) {
        return Ok(i);
    }
    metrics.push(bm);
    Ok(metrics.len() - 1)
}

fn bind_having(
    b: &mut Binder<'_>,
    joins: &mut [JoinB<'_>],
    metrics: &mut Vec<(Metric, Option<ColRef>)>,
    h: &HavingCond,
    depth: usize,
) -> Result<BHaving, DataError> {
    if depth > super::ir::MAX_CONDITION_DEPTH {
        return Err(DataError::Invalid("HAVING のネストが深すぎます".into()));
    }
    Ok(match h {
        HavingCond::All(v) => BHaving::All(
            v.iter()
                .map(|x| bind_having(b, joins, metrics, x, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        HavingCond::Any(v) => BHaving::Any(
            v.iter()
                .map(|x| bind_having(b, joins, metrics, x, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        HavingCond::Not(x) => BHaving::Not(Box::new(bind_having(b, joins, metrics, x, depth + 1)?)),
        HavingCond::Cmp(c) => {
            if !matches!(
                c.op,
                CondOp::Eq | CondOp::Neq | CondOp::Lt | CondOp::Lte | CondOp::Gt | CondOp::Gte
            ) {
                return Err(DataError::Invalid(
                    "HAVING の比較は eq / neq / lt / lte / gt / gte だけです".into(),
                ));
            }
            let value = c.value.as_f64().ok_or_else(|| {
                DataError::Invalid("HAVING の比較値は数値で指定してください".into())
            })?;
            BHaving::Cmp {
                metric: metric_index(b, joins, metrics, &c.metric)?,
                op: c.op,
                value,
            }
        }
    })
}
