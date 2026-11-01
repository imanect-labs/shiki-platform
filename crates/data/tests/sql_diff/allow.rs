//! 許容リスト: Shiki がわざと PostgreSQL と違えている箇所（#1004）。
//!
//! ここに書いた差は、探索が毎回そこで止まらないよう、参照側の SQL を Shiki の決まりに合わせて書き換えるか、
//! 比べる対象から外す。**根拠（正本ドキュメントの箇所）の無い差は書かない**。根拠の無い食い違いは誤りとして
//! 報告する。
//!
//! | id | 差 | 扱い | 根拠 |
//! |---|---|---|---|
//! | `nulls-last` | 値なしは昇順・降順とも末尾（PostgreSQL は降順で先頭） | 参照側の `ORDER BY` に `NULLS LAST` を付ける | `crates/data/src/query/ir.rs` の `SortKey`・data-platform §5.3 |
//! | `limit-is-page` | `LIMIT n` は 1 ページの件数（続きは `next_cursor`・上限 200） | `LIMIT` のある文は最初のページだけを比べ、`LIMIT` は 200 以下で作る | data-platform §5.3（keyset）・`query/ir.rs` の `MAX_LIMIT` |
//! | `small-cell` | 行数が K 未満の集計は値を伏せる（`suppressed`） | 生成する表は K = 1（`aggregate_min_rows`）。それでも伏せたセル（0 行の集計）は比べない | data-platform §7（PIT-17） |
//! | `rejected` | 部分集合の外は 422 で拒否する | 下の [`REJECTIONS`] に一致する拒否は飛ばす（数だけ数える） | data-platform §16.1 |
//! | `like-subset` | `LIKE` は `'p%'` / `'%p%'` / `'%p'` / `'p'` だけ（`_`・途中の `%`・空のパターンは 422）。`ILIKE` は 422（大文字小文字を区別して比べる） | [`REJECTIONS`] で飛ばす | `crates/data/src/sql/translate.rs` の翻訳表と `like`・`parse.rs` の `ILike` |
//! | `agg-number-only` | `SUM` / `AVG` / `MIN` / `MAX` は number の列だけ（400 `value_type_mismatch`） | [`REJECTIONS`] で飛ばす | data-platform §5.5（数値集計は `data_index_num` で計算）・`query/bound_query.rs` の `bind_metric` |
//! | `count-star-only` | `COUNT(列)` は 422（`COUNT(*)` と `WHERE 列 IS NOT NULL` へ誘導） | [`REJECTIONS`] で飛ばす | `crates/data/src/sql/parse.rs` の集計の写し（拒否文に書き換え方がある） |
//! | `limit-zero` | `LIMIT 0` は 400（`limit は 1〜200`・件数だけなら `with_count`） | [`REJECTIONS`] で飛ばす | data-platform エラー表の `invalid_input`（「`limit` が 0 で件数だけでない」） |
//! | `date-literal` | 日付の列に日付（`YYYY-MM-DD`）でない比較値を渡すと 400 `value_type_mismatch` | [`REJECTIONS`] で飛ばす | data-platform エラー表の `value_type_mismatch` |
//! | `field-not-indexed` | 索引の無い列での絞り込み・並べ替えは 400 `field_not_indexed` | [`REJECTIONS`] で飛ばす。生成側は索引のある列を多めに作る | data-platform §5（「Phase 15 実装で変更」）・エラー表の `InvalidInput` |

use super::model::Side;

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

/// わざと拒否する形（拒否・エラーの文言に含まれる語と、その根拠）。一致したものは「飛ばした」と数える。
pub const REJECTIONS: &[(&str, &str)] = &[
    (
        "FieldNotIndexed",
        "field-not-indexed: data-platform §5・エラー表（索引の無い列の指定は 400）",
    ),
    ("LIKE のパターン", "like-subset: translate.rs の翻訳表"),
    ("COUNT(列) は使えません", "count-star-only: parse.rs"),
    (
        "limit は 1〜200",
        "limit-zero: data-platform エラー表の invalid_input",
    ),
    (
        "日付（YYYY-MM-DD）で指定",
        "date-literal: data-platform エラー表の value_type_mismatch",
    ),
    (
        "ILIKE は使えません",
        "like-subset: parse.rs（大文字小文字を区別する）",
    ),
    (
        "である必要があります",
        "agg-number-only: data-platform §5.5（数値集計）",
    ),
];

/// 拒否・エラーが許容リストにあるか（あればその語）。
pub fn rejection_allowed(message: &str) -> Option<&'static str> {
    REJECTIONS
        .iter()
        .find(|(needle, _)| message.contains(needle))
        .map(|(needle, _)| *needle)
}
