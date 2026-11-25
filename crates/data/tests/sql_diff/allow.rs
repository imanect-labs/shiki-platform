//! 許容リスト: Shiki がわざと PostgreSQL と違えている箇所（#1004）。
//!
//! ここに書いた差は、探索が毎回そこで止まらないよう、参照側の SQL を Shiki の決まりに合わせて書き換えるか、
//! 比べる対象から外す。**根拠（正本ドキュメントの箇所）の無い差は書かない**。根拠の無い食い違いは誤りとして
//! 報告する。拒否を飛ばすのは、拒否の文言が一致し、**かつその 1 件の形から拒否が起こり得る**ときだけ
//! （たとえば索引のある列だけを使う問い合わせが `field_not_indexed` になったら、飛ばさず食い違いにする）。
//!
//! | id | 差 | 扱い | 根拠 |
//! |---|---|---|---|
//! | `nulls-last` | 値なしは昇順・降順とも末尾（PostgreSQL は降順で先頭） | 参照側の `ORDER BY` に `NULLS LAST` を付ける | `crates/data/src/query/ir.rs` の `SortKey`・data-platform §5.3 |
//! | `limit-is-page` | `LIMIT n` は 1 ページの件数（続きは `next_cursor`・上限 200） | `LIMIT` のある文は最初のページだけを比べ、`LIMIT` は 200 以下で作る | data-platform §5.3（keyset）・`query/ir.rs` の `MAX_LIMIT` |
//! | `small-cell` | 行数が K 未満の集計は値を伏せる（`suppressed`） | 生成する表は K = 1（`aggregate_min_rows`）。伏せたのが 0 行の全体集計だけなら比べない | data-platform §5.5・PIT-17 |
//! | `field-not-indexed` | 索引の無い列での絞り込み・並べ替え・集計、`searchable` でない列の部分一致・後方一致は 400 `field_not_indexed` | [`REFUSALS`] | data-platform §5（「Phase 15 実装で変更」）・エラー表の `InvalidInput` |
//! | `like-subset` | `LIKE` は `'p%'` / `'%p%'` / `'%p'` / `'p'` だけ（`_`・途中の `%`・空のパターンは 422） | [`REFUSALS`] | `crates/data/src/sql/translate.rs` の翻訳表と `like` |
//! | `no-ilike` | `ILIKE` は 422（大文字小文字を区別して比べる） | [`REFUSALS`] | `crates/data/src/sql/parse.rs` の `ILike` |
//! | `count-star-only` | `COUNT(列)` は 422（`COUNT(*)` と `WHERE 列 IS NOT NULL` へ誘導） | [`REFUSALS`] | `crates/data/src/sql/parse.rs` の集計の写し（拒否文に書き換え方がある） |
//! | `agg-number-only` | `SUM` / `AVG` / `MIN` / `MAX` は number の列だけ（400 `value_type_mismatch`） | [`REFUSALS`] | data-platform §5.5（数値集計は `data_index_num` で計算）・`query/bound_query.rs` の `bind_metric` |
//! | `limit-zero` | `LIMIT 0` は 400（`limit は 1〜200`・件数だけなら `with_count`） | [`REFUSALS`] | data-platform エラー表の `invalid_input`（「`limit` が 0 で件数だけでない」） |
//! | `date-literal` | 日付の列に日付（`YYYY-MM-DD`）でない比較値を渡すと 400 `value_type_mismatch` | [`REFUSALS`] | data-platform エラー表の `value_type_mismatch` |

use super::model::{Agg, Case, Func, Pred, Query, Side, Ty, Val};

/// 並べ替えのキー（参照側は値なしを末尾に固定する・`nulls-last`）。
pub fn order_key(col: &str, desc: bool, side: Side) -> String {
    let dir = if desc { " DESC" } else { "" };
    match side {
        Side::Shiki => format!("{col}{dir}"),
        Side::Pg => format!("{col}{dir} NULLS LAST"),
    }
}

/// 生成する `LIMIT` の上限（`limit-is-page`）。
pub const MAX_LIMIT: u32 = 200;

/// わざと拒否する形。`needle` は拒否・エラーの文言に含まれる語、`applies` はその 1 件の形から
/// 拒否が起こり得るか。
pub struct Refusal {
    pub id: &'static str,
    pub needle: &'static str,
    pub applies: fn(&Case) -> bool,
}

pub const REFUSALS: &[Refusal] = &[
    Refusal {
        id: "field-not-indexed",
        needle: "code: FieldNotIndexed",
        applies: uses_unindexed,
    },
    Refusal {
        id: "like-subset",
        needle: "LIKE のパターン '",
        applies: has_bad_like,
    },
    Refusal {
        id: "no-ilike",
        needle: "ILIKE は使えません",
        applies: has_ilike,
    },
    Refusal {
        id: "count-star-only",
        needle: "COUNT(列) は使えません",
        applies: has_count_col,
    },
    Refusal {
        id: "agg-number-only",
        needle: "は number である必要があります",
        applies: has_non_number_agg,
    },
    Refusal {
        id: "limit-zero",
        needle: "limit は 1〜200 で指定してください",
        applies: has_limit_zero,
    },
    Refusal {
        id: "date-literal",
        needle: "は日付（YYYY-MM-DD）で指定してください",
        applies: has_bad_date_literal,
    },
];

/// 拒否・エラーが許容リストにあり、この 1 件で起こり得るか（あればその id）。
pub fn refusal_for(case: &Case, message: &str) -> Option<&'static str> {
    REFUSALS
        .iter()
        .find(|r| message.contains(r.needle) && (r.applies)(case))
        .map(|r| r.id)
}

/// 条件式の葉（論理演算を外したもの）。
fn leaves(p: &Pred) -> Vec<&Pred> {
    match p {
        Pred::Not(x) | Pred::Unknown(x, _) | Pred::Truth(x, _, _) => leaves(x),
        Pred::And(a, b) | Pred::Or(a, b) => {
            let mut v = leaves(a);
            v.extend(leaves(b));
            v
        }
        leaf => vec![leaf],
    }
}

fn filter_leaves(case: &Case) -> Vec<&Pred> {
    let filter = match &case.query {
        Query::Rows { filter, .. } | Query::Agg { filter, .. } => filter.as_ref(),
    };
    filter.map(leaves).unwrap_or_default()
}

fn leaf_col(p: &Pred) -> Option<&str> {
    match p {
        Pred::Cmp(c, ..)
        | Pred::IsNull(c, _)
        | Pred::In(c, ..)
        | Pred::Between(c, ..)
        | Pred::Like(c, ..) => Some(c),
        _ => None,
    }
}

/// LIKE のパターンが受け付ける形（`p%` / `%p%` / `%p` / `p`）か。
pub fn like_ok(pattern: &str) -> bool {
    if pattern.contains('_') || pattern.contains('\\') {
        return false;
    }
    let rest = pattern.strip_prefix('%').unwrap_or(pattern);
    let inner = rest.strip_suffix('%').unwrap_or(rest);
    !inner.is_empty() && !inner.contains('%')
}

fn uses_unindexed(case: &Case) -> bool {
    let unindexed = |name: &str| case.col(name).is_some_and(|c| !c.indexed);
    let mut cols: Vec<&str> = filter_leaves(case)
        .into_iter()
        .filter_map(leaf_col)
        .collect();
    match &case.query {
        Query::Rows { order, .. } => cols.extend(order.iter().map(|(c, _)| c.as_str())),
        Query::Agg { group, aggs, .. } => {
            cols.extend(group.as_deref());
            cols.extend(aggs.iter().filter_map(|a| a.arg.as_deref()));
        }
    }
    let partial_unsearchable = filter_leaves(case).into_iter().any(|p| {
        matches!(p, Pred::Like(c, pat, _, false)
            if pat.starts_with('%') && case.col(c).is_some_and(|c| !c.searchable))
    });
    cols.into_iter().any(unindexed) || partial_unsearchable
}

fn has_bad_like(case: &Case) -> bool {
    filter_leaves(case)
        .into_iter()
        .any(|p| matches!(p, Pred::Like(_, pat, _, false) if !like_ok(pat)))
}

fn has_ilike(case: &Case) -> bool {
    filter_leaves(case)
        .into_iter()
        .any(|p| matches!(p, Pred::Like(_, _, _, true)))
}

fn aggs(case: &Case) -> &[Agg] {
    match &case.query {
        Query::Agg { aggs, .. } => aggs,
        Query::Rows { .. } => &[],
    }
}

fn has_count_col(case: &Case) -> bool {
    aggs(case)
        .iter()
        .any(|a| a.func == Func::Count && a.arg.is_some())
}

fn has_non_number_agg(case: &Case) -> bool {
    aggs(case).iter().any(|a| {
        a.func != Func::Count
            && a.arg
                .as_deref()
                .and_then(|c| case.col(c))
                .is_some_and(|c| !c.ty.numeric())
    })
}

fn has_limit_zero(case: &Case) -> bool {
    matches!(case.query, Query::Rows { limit: Some(0), .. })
}

fn is_date(v: &Val) -> bool {
    matches!(v, Val::Str(s) if chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok())
}

fn has_bad_date_literal(case: &Case) -> bool {
    filter_leaves(case).into_iter().any(|p| {
        let date = leaf_col(p)
            .and_then(|c| case.col(c))
            .is_some_and(|c| c.ty == Ty::Date);
        date && match p {
            Pred::Like(..) => true,
            Pred::Cmp(_, _, v) => !is_date(v),
            Pred::In(_, vs, _) => vs.iter().any(|v| !is_date(v)),
            Pred::Between(_, a, b, _) => !is_date(a) || !is_date(b),
            _ => false,
        }
    })
}
