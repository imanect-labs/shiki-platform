//! クエリ IR v2（data-platform §5・Task 15.3）。外部契約（DTO）はここだけに置く。
//!
//! - 条件木（`all` / `any` / `not` / `cmp`）。演算子語彙は workflow IR の `Condition` に揃える
//!   （`matches` は語彙として受けるが data では 400・索引が効かず DoS 面になるため）。
//! - 意味論は **2 値論理**（query-optimizer.md §3.1）: 値なしの `neq` は真、`not` は補集合、
//!   多値列の `eq` は「含む」、数値は数値として比較する。**行述語（row_policy）はこの表の対象外**で、
//!   v1 の意味論（値なしの `ne` は偽）のまま別経路で評価する（§3.3）。
//! - ページングは keyset カーソル（OFFSET は廃止）。カーソルはサーバ鍵の AEAD で暗号化した不透明
//!   トークンで、`record_int` を外へ出さない（PIT-75）。
//! - 推定値・統計値・戦略名・予測コストのフィールドは**持たない**（PIT-75）。
//!
//! v1 の形（`filter: {field, value}`・`sort: {field, descending}`・`aggregate: {group_by, metric, field}`・
//! `offset`）も受理して v2 の形へ写す（保存済みビューと SDK の後方互換）。`offset` は 0 以外を 400 にする。

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
/// 集計結果の件数の上限（`group_limit`）。
pub(crate) const MAX_GROUP_LIMIT: u32 = 1_000;

/// 比較演算子（workflow IR の `CmpOp` と同じ語彙・snake_case。語彙の一致は
/// `crates/workflow-engine/tests/cmp_op_vocab.rs` が生成型の列挙値で検査する）。
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
    /// 正規表現。語彙の互換のためだけに受け、data では常に 400（索引が効かず DoS 面）。
    Matches,
}

/// 比較の葉（`field` は参照名 key・`value` は演算子ごとの引数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CondCmp {
    pub field: String,
    pub op: CondOp,
    /// `exists` / `is_null` では省略。`in` は配列。値は文字列・数値・真偽・配列（任意の JSON）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

/// 条件木（AND / OR / NOT / 比較 / リンクの半結合）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(no_recursion)]
pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    Cmp(CondCmp),
    /// リンク列で辿った参照先に「閲覧者に見えて `filter` を満たす行」が 1 件以上ある（半結合・
    /// data-platform §16.2）。参照先の行述語・テーブル viewer は閲覧者本人の権限で必ず適用する（PIT-20）。
    Link(LinkCond),
}

/// リンクの半結合の条件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(no_recursion)]
pub struct LinkCond {
    /// この表のリンク列（`record_ref`・対称列も可）の参照名。
    pub field: String,
    /// 参照先の条件（参照先の列の参照名で書く）。省略時は「見える参照先が 1 件以上ある」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Box<Condition>>,
}

/// 結合（宣言済みリンク列経由の INNER JOIN・data-platform §16.1 の JOIN の束縛規則）。
///
/// 結果の行は「FROM の行 × 結合先の行（閲覧者に見えて `filter` を満たし、リンク列で結ばれたもの）」の
/// 組み合わせ。結合先が 1 件も無い行は結果から除かれ、多対一以外では行が重複する（SQL の `INNER JOIN` と同じ）。
/// カーディナリティは `LinkDef` から取る（ここでは指定させない）。結合先の列は `別名.参照名` で
/// `select` / `group_by` / `aggregate` に書く。結合先の列を使わず、件数・集計が変わらないと証明できる
/// 結合は、実行前に参照先の行述語つきの半結合へ書き換える（§16.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinSpec {
    /// 結合先の別名（`[a-z][a-z0-9_]*`）。
    pub alias: String,
    /// 結合元の別名（省略時は FROM のテーブル）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// 結合元のリンク列（`record_ref`・対称列も可）の参照名。
    pub field: String,
    /// 結合先に掛ける条件（結合先の列の参照名で書く）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<Condition>,
}

/// 集計後の条件（SQL の `HAVING`）。抑制された（K 未満の）グループの値は `NULL` として扱い、
/// どの比較も成立しない（伏せた値で絞り込ませない・PIT-17）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(no_recursion)]
pub enum HavingCond {
    All(Vec<HavingCond>),
    Any(Vec<HavingCond>),
    Not(Box<HavingCond>),
    Cmp(HavingCmp),
}

/// 集計値の比較（`eq` / `neq` / `lt` / `lte` / `gt` / `gte`・値は数値）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HavingCmp {
    pub metric: MetricSpec,
    pub op: CondOp,
    #[schema(value_type = f64)]
    pub value: serde_json::Number,
}

/// 集計結果の並べ替えのキー（`field` か `metric` のどちらか一方）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupSortKey {
    /// グループ化フィールド（`group_by` にあるもの）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// 集計指標。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<MetricSpec>,
    #[serde(default)]
    pub descending: bool,
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
    /// 宣言済みリンク列経由の結合（INNER JOIN・最大 3 段）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joins: Vec<JoinSpec>,
    /// 集計後の条件（SQL の `HAVING`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub having: Option<HavingCond>,
    /// 集計結果の並べ替え（空なら順序は未定義）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_sort: Vec<GroupSortKey>,
    /// 集計結果の件数の上限（並べ替えの後に適用・最大 1,000）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_limit: Option<u32>,
    /// 横断部分一致（`searchable` 宣言済みの列だけを対象にする）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// 部分一致の対象列を絞る（省略時は読める `searchable` 列すべて。マスク列の指定は 403）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_fields: Option<Vec<String>>,
    /// 件数を数えるか（既定 false・上限付き正確カウント）。
    #[serde(default)]
    pub with_count: bool,
    /// v1 の OFFSET（廃止）。0 以外は 400。
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
    joins: Vec<JoinSpec>,
    #[serde(default)]
    having: Option<HavingCond>,
    #[serde(default)]
    group_sort: Vec<GroupSortKey>,
    #[serde(default)]
    group_limit: Option<u32>,
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

/// [`DataQuery`] の受理形の最上位のキー（[`DataQueryWire`] と同じ集合・未知のキーの検出用）。
pub const DATA_QUERY_KEYS: &[&str] = &[
    "filter",
    "sort",
    "limit",
    "cursor",
    "select",
    "group_by",
    "aggregate",
    "joins",
    "having",
    "group_sort",
    "group_limit",
    "search",
    "search_fields",
    "with_count",
    "offset",
    "snapshot",
];

impl DataQuery {
    /// 要求の入口で受けたクエリを読む。**最上位の未知のキー**（`limitt` 等の綴り誤り）は黙って無視せず
    /// 400（`invalid_input`・`field` にそのキー）にする（LLM・SDK が自己修正できる・検証 C P3）。
    /// 保存済みのビュー等の読み戻しには使わない（後方互換のため従来どおり寛容に読む）。
    pub fn from_request(v: serde_json::Value) -> Result<DataQuery, crate::DataError> {
        Self::parse_request(v).map_err(|e| match e {
            QueryParseError::Data(d) => d,
            QueryParseError::Shape(e) => {
                tracing::info!(error = %e, "クエリの形が不正です");
                crate::DataError::input(crate::InvalidCode::InvalidInput)
            }
        })
    }

    /// [`Self::from_request`] の、形の誤りの理由（serde の誤り）を返す版。serde の文言は内部の型名・英語を
    /// 含むので外へそのまま出さない。HTTP の入口は他の本文と同じ「本文（JSON）を読めません: …」へ訳して返す
    /// （3 回目の検証 3-B-6: `/query` だけ汎用の文言だった）。
    pub fn parse_request(v: serde_json::Value) -> Result<DataQuery, QueryParseError> {
        if let Some(obj) = v.as_object() {
            if let Some(k) = obj.keys().find(|k| !DATA_QUERY_KEYS.contains(&k.as_str())) {
                return Err(QueryParseError::Data(crate::DataError::unknown_query_key(
                    k,
                )));
            }
        }
        serde_json::from_value(v).map_err(QueryParseError::Shape)
    }
}

/// [`DataQuery::parse_request`] の誤り。
#[derive(Debug)]
pub enum QueryParseError {
    /// 構造化した入力の誤り（未知のキー）。
    Data(crate::DataError),
    /// 形の誤り（serde の誤り・文言は内部向け）。
    Shape(serde_json::Error),
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
            joins: w.joins,
            having: w.having,
            group_sort: w.group_sort,
            group_limit: w.group_limit,
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
