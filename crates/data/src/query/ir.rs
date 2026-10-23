//! クエリ IR v2（data-platform §5・Task 15.3）。外部契約（DTO）はここだけに置く。
//!
//! - 条件木（`all` / `any` / `not` / `cmp`）。演算子語彙は workflow IR の `Condition` に揃える
//!   （`matches` は語彙として受けるが data では 422・索引が効かず DoS 面になるため）。
//! - 意味論は **2 値論理**（query-optimizer.md §3.1）: 値なしの `neq` は真、`not` は補集合、
//!   多値列の `eq` は「含む」、数値は数値として比較する。**行述語（row_policy）はこの表の対象外**で、
//!   v1 の意味論（値なしの `ne` は偽）のまま別経路で評価する（§3.3）。
//! - ページングは keyset カーソル（OFFSET は廃止）。カーソルはサーバ鍵の AEAD で暗号化した不透明
//!   トークンで、`record_int` を外へ出さない（PIT-75）。
//! - 推定値・統計値・戦略名・予測コストのフィールドは**持たない**（PIT-75）。
//!
//! v1 の形（`filter: {field, value}`・`sort: {field, descending}`・`aggregate: {group_by, metric, field}`・
//! `offset`）も受理して v2 の形へ写す（保存済みビューと SDK の後方互換）。`offset` は 0 以外を 422 にする。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::index::SortPrecision;
use crate::model::DataRecord;

/// 条件木の最大深さ。
pub(crate) const MAX_CONDITION_DEPTH: usize = 8;
/// 条件木の葉の最大数。
pub(crate) const MAX_CONDITION_LEAVES: usize = 64;
/// `in` の要素数上限。
pub(crate) const MAX_IN_VALUES: usize = 1_000;
/// ソートキー数の上限。
pub(crate) const MAX_SORT_KEYS: usize = 4;
/// 1 ページの既定件数と上限。
pub(crate) const DEFAULT_LIMIT: u32 = 50;
pub(crate) const MAX_LIMIT: u32 = 200;

/// 比較演算子（workflow IR の `CmpOp` と同じ語彙・snake_case）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CondOp {
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    In,
    Contains,
    StartsWith,
    EndsWith,
    Exists,
    IsNull,
    /// 正規表現。語彙の互換のためだけに受け、data では常に 422（索引が効かず DoS 面）。
    Matches,
}

/// 比較の葉（`field` は参照名 key・`value` は演算子ごとの引数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CondCmp {
    pub field: String,
    pub op: CondOp,
    /// `exists` / `is_null` では省略。`in` は配列。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub value: Option<serde_json::Value>,
}

/// 条件木（AND / OR / NOT / 比較）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(no_recursion)]
pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    Cmp(CondCmp),
}

impl Condition {
    /// 便利関数: 比較の葉。
    pub fn cmp(field: impl Into<String>, op: CondOp, value: Option<serde_json::Value>) -> Self {
        Condition::Cmp(CondCmp {
            field: field.into(),
            op,
            value,
        })
    }
}

/// ソートキー（値なしは昇順・降順とも末尾に固定。`NULLS FIRST` は受け付けない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SortKey {
    pub field: String,
    #[serde(default)]
    pub descending: bool,
}

/// 集計メトリクス。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

/// 集計の 1 指標（`count` 以外は number フィールドを指定する）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MetricSpec {
    pub metric: Metric,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

/// クエリ IR（data-platform §5.1）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(from = "DataQueryWire")]
pub struct DataQuery {
    /// 条件木（2 値論理）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Condition>,
    /// 複数ソートキー（空ならシステム擬似列 `updated_at` の降順）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sort: Vec<SortKey>,
    /// 1 ページの件数（既定 50・最大 200）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// 前ページの `next_cursor`（不透明トークン）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// 投影（返すフィールドの参照名。省略時は全フィールド）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<Vec<String>>,
    /// グループ化フィールド（`aggregate` と組で使う）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<String>,
    /// 集計指標（空でなければ集計クエリ）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aggregate: Vec<MetricSpec>,
    /// 横断部分一致（`searchable` 宣言済みの列だけを対象にする）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// 部分一致の対象列を絞る（省略時は読める `searchable` 列すべて。マスク列の指定は 403）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_fields: Option<Vec<String>>,
    /// 件数を数えるか（既定 false・上限付き正確カウント）。
    #[serde(default)]
    pub with_count: bool,
    /// v1 の OFFSET（廃止）。0 以外は 422。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(deprecated)]
    pub offset: Option<i64>,
    /// スナップショット読取（data-platform §3.3）。最初のページの `snapshot_seq` にカーソルを束縛し、
    /// その時点の集合を不変の位置の順（おおむね作成順）に重複・欠落なく返す（エクスポート・一括処理用。`sort`・集計・件数は
    /// 指定できない・保持期間を過ぎたカーソルは 410）。既定はライブページング。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub snapshot: bool,
}

/// 上限付き件数（data-platform §5.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CountResult {
    /// 上限内の正確な件数。
    Exact(u64),
    /// 上限に達した（少なくともこの件数）。
    AtLeast(u64),
}

impl CountResult {
    /// 数値（`AtLeast` は下限値）。
    pub fn value(self) -> u64 {
        match self {
            CountResult::Exact(n) | CountResult::AtLeast(n) => n,
        }
    }
}

/// 集計結果の 1 グループ。
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AggregateGroup {
    /// group_by フィールド → 値（全体集計は空）。
    pub key: serde_json::Map<String, serde_json::Value>,
    /// 先頭の指標の値（v1 互換。K 未満で抑制された場合は null）。
    pub value: serde_json::Value,
    /// 全指標の値（`aggregate` と同じ順。K 未満で抑制された場合は null）。
    pub values: Vec<serde_json::Value>,
}

/// クエリ実行結果（`items` または `groups` のどちらか）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QueryResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<DataRecord>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<AggregateGroup>>,
    /// 次ページのカーソル（無ければ終端）。ライブページングは重複・欠落を保証しない。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// `with_count: true` のときだけ件数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<CountResult>,
    /// ソートの精度（切り詰めが起こり得る列で並べたら `prefix`・data-platform §3.3）。
    pub sort_precision: SortPrecision,
    /// このソートの組み合わせは複合索引の準備中で、深いページほど遅くなる（data-platform §5.3。
    /// 可視行で見て第 1 キーの同値群がページの 8 倍を超えたとき）。
    pub sort_degraded: bool,
    /// 個別共有集合の切り詰め（PIT-18）。
    pub shares_truncated: bool,
    /// K 未満で抑制されたグループ/全体集計があった（PIT-17）。
    pub suppressed: bool,
    /// 配信位置（SSE の `delivery_seq` と同じ時間軸・data-platform §3.3）。ライブページングでは
    /// 読取時点の位置（SSE を `since` にこれを渡して繋ぐと読取後の変更を取りこぼさない）、
    /// スナップショット読取ではカーソルが束縛された基準位置。集計では返さない。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_seq: Option<i64>,
}

// ---------------------------------------------------------------------------
// 受理形（v1 互換）
// ---------------------------------------------------------------------------

/// v1 の単一フィルタ（`{field, value}` = 型で決まる等値／multi_select は「含む」）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyFilter {
    field: String,
    value: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum FilterWire {
    Tree(Condition),
    Legacy(LegacyFilter),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum SortWire {
    Many(Vec<SortKey>),
    One(SortKey),
}

/// v1 の単一集計（`{group_by, metric, field}`）。
#[derive(Debug, Clone, Deserialize)]
struct LegacyAggregate {
    #[serde(default)]
    group_by: Vec<String>,
    metric: Metric,
    #[serde(default)]
    field: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum AggregateWire {
    Many(Vec<MetricSpec>),
    Legacy(LegacyAggregate),
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DataQueryWire {
    #[serde(default)]
    filter: Option<FilterWire>,
    #[serde(default)]
    sort: Option<SortWire>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    select: Option<Vec<String>>,
    #[serde(default)]
    group_by: Vec<String>,
    #[serde(default)]
    aggregate: Option<AggregateWire>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    search_fields: Option<Vec<String>>,
    #[serde(default)]
    with_count: bool,
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    snapshot: bool,
}

impl From<DataQueryWire> for DataQuery {
    fn from(w: DataQueryWire) -> Self {
        let filter = w.filter.map(|f| match f {
            FilterWire::Tree(c) => c,
            // v1 の等値フィルタは v2 の `eq`（multi_select の「含む」も `eq` と同義・§3.1）。
            FilterWire::Legacy(l) => Condition::cmp(l.field, CondOp::Eq, Some(l.value)),
        });
        let sort = match w.sort {
            None => Vec::new(),
            Some(SortWire::Many(v)) => v,
            Some(SortWire::One(k)) => vec![k],
        };
        let mut group_by = w.group_by;
        let aggregate = match w.aggregate {
            None => Vec::new(),
            Some(AggregateWire::Many(v)) => v,
            Some(AggregateWire::Legacy(l)) => {
                if group_by.is_empty() {
                    group_by = l.group_by;
                }
                vec![MetricSpec {
                    metric: l.metric,
                    field: l.field,
                }]
            }
        };
        DataQuery {
            filter,
            sort,
            // 範囲外は束縛時に丸める（負値・巨大値でデシリアライズを落とさない）。
            limit: w
                .limit
                .map(|n| u32::try_from(n.clamp(0, i64::from(u32::MAX))).unwrap_or(MAX_LIMIT)),
            cursor: w.cursor,
            select: w.select,
            group_by,
            aggregate,
            search: w.search,
            search_fields: w.search_fields,
            with_count: w.with_count,
            offset: w.offset,
            snapshot: w.snapshot,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn v2_shape_roundtrips() {
        let q: DataQuery = serde_json::from_value(json!({
            "filter": {"all": [
                {"cmp": {"field": "status", "op": "eq", "value": "open"}},
                {"not": {"cmp": {"field": "amount", "op": "lt", "value": 10}}},
                {"any": [{"cmp": {"field": "tags", "op": "exists"}}]}
            ]},
            "sort": [{"field": "amount", "descending": true}, {"field": "title"}],
            "limit": 20,
            "with_count": true
        }))
        .unwrap();
        assert_eq!(q.sort.len(), 2);
        assert!(q.with_count);
        assert_eq!(q.limit, Some(20));
        let back: DataQuery = serde_json::from_value(serde_json::to_value(&q).unwrap()).unwrap();
        assert_eq!(back, q);
    }

    #[test]
    fn v1_shape_is_accepted() {
        let q: DataQuery = serde_json::from_value(json!({
            "filter": {"field": "status", "value": "open"},
            "sort": {"field": "amount", "descending": true},
            "limit": 5,
            "offset": 0,
            "aggregate": {"group_by": ["status"], "metric": "sum", "field": "amount"}
        }))
        .unwrap();
        assert_eq!(
            q.filter,
            Some(Condition::cmp("status", CondOp::Eq, Some(json!("open"))))
        );
        assert_eq!(
            q.sort,
            vec![SortKey {
                field: "amount".into(),
                descending: true
            }]
        );
        assert_eq!(q.group_by, vec!["status".to_string()]);
        assert_eq!(q.aggregate[0].metric, Metric::Sum);
        assert_eq!(q.offset, Some(0));
    }

    #[test]
    fn unknown_operator_fails_closed() {
        let r: Result<DataQuery, _> = serde_json::from_value(json!({
            "filter": {"cmp": {"field": "a", "op": "raw_sql", "value": "1"}}
        }));
        assert!(r.is_err());
    }

    #[test]
    fn api_schema_has_no_estimates_or_strategy() {
        use utoipa::PartialSchema;
        let s = serde_json::to_string(&<QueryResult as PartialSchema>::schema()).unwrap();
        for banned in [
            "strategy",
            "estimate",
            "cost",
            "selectivity",
            "record_int",
            "scanned",
        ] {
            assert!(!s.contains(banned), "{banned} が API スキーマにある: {s}");
        }
        assert!(s.contains("sort_precision") && s.contains("next_cursor"));
    }
}
