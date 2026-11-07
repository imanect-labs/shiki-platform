//! 索引（Task 15.2・data-platform §3・§10）。
//!
//! - pivot 索引テーブル（`data_index_text` / `data_index_num` / `data_link` / `data_unique` /
//!   `data_index_search`）への書込（[`write`]）・値の正規化（[`normalize`]）・
//!   索引状態の台帳（[`state`]）・バックフィル（[`backfill`]）・parity 検証（[`parity`]）
//!
//! # 不変条件
//!
//! - **索引の物理本数はテナント数・テーブル数・列数に依存しない**。索引の付け外しは行の追記/削除で、
//!   DDL を打たない（旧方式の式インデックスは Task 15.3 で撤去済み＝**ランタイム DDL はゼロ**）。
//! - 書込経路（create / update / delete / FSM 遷移）は**同一 Tx で、変更のあったフィールドの索引行だけ**を
//!   書き換える（[`write`]）。索引状態が `building` / `active` の (フィールド, 索引テーブル) が対象。
//! - `record_int` は索引の内部キーで、**外へ出さない**（PIT-75）。

pub(crate) mod backfill;
pub(crate) mod composite;
pub(crate) mod link;
pub(crate) mod normalize;
pub(crate) mod parity;
pub(crate) mod partition;
pub(crate) mod promote;
pub(crate) mod scan;
pub(crate) mod state;
pub(crate) mod write;

/// 索引テーブルに格納するキーの最大バイト数（btree キー上限 ≒ 2,704B の内側・data-platform §3.3）。
///
/// これを超える値はプレフィクスだけを格納して `truncated = true` にし、等値・前方一致は候補を本体で
/// 再確認する。マイグレーションの `check (octet_length(val) <= 2000)` と一致させること。
pub const MAX_INDEX_KEY_BYTES: usize = 2_000;

/// 多値（multi_select / リンク）の 1 レコードあたり要素数の既定上限（PIT-72）。
/// [`crate::DataStore::with_max_multi_values`] で変更できる。超過は 422。
pub const DEFAULT_MAX_MULTI_VALUES: usize = 50;

/// 多値要素数の上限として設定できる最大値（`ord` が `int2` のため）。
pub(crate) const MAX_MULTI_VALUES_CEILING: usize = 32_767;

/// 索引テーブルの種別（台帳 `data_index_state.index_table` の値）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum IndexTable {
    /// `data_index_text`（text 系・multi_select・擬似列）。
    Text,
    /// `data_index_num`（number）。
    Num,
    /// `data_link`（record_ref）。
    Link,
    /// `data_unique`（unique 宣言）。
    Unique,
    /// `data_index_search`（searchable 宣言の部分一致）。
    Search,
}

impl IndexTable {
    pub(crate) const ALL: [IndexTable; 5] = [
        IndexTable::Text,
        IndexTable::Num,
        IndexTable::Link,
        IndexTable::Unique,
        IndexTable::Search,
    ];

    pub(crate) fn code(self) -> i16 {
        match self {
            IndexTable::Text => 1,
            IndexTable::Num => 2,
            IndexTable::Link => 3,
            IndexTable::Unique => 4,
            IndexTable::Search => 5,
        }
    }

    pub(crate) fn from_code(c: i16) -> Option<IndexTable> {
        IndexTable::ALL.into_iter().find(|t| t.code() == c)
    }

    /// 物理表名（SQL へ埋め込むのはこの固定集合だけ・PIT-21）。
    pub(crate) fn relation(self) -> &'static str {
        match self {
            IndexTable::Text => "data_index_text",
            IndexTable::Num => "data_index_num",
            IndexTable::Link => "data_link",
            IndexTable::Unique => "data_unique",
            IndexTable::Search => "data_index_search",
        }
    }

    /// その表で「このレコードの行」を指す列名（`data_link` だけ `src_record`）。
    pub(crate) fn record_column(self) -> &'static str {
        match self {
            IndexTable::Link => "src_record",
            _ => "record_int",
        }
    }
}

/// 索引状態（data-platform §10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// 索引行を持たない（書込もしない）。
    Absent,
    /// バックフィル中。**書込は write-through する**がクエリには使わせない（PIT-72）。
    Building,
    /// 完成。クエリの駆動・フィルタに使える。
    Active,
}

impl IndexState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            IndexState::Absent => "absent",
            IndexState::Building => "building",
            IndexState::Active => "active",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<IndexState> {
        match s {
            "absent" => Some(IndexState::Absent),
            "building" => Some(IndexState::Building),
            "active" => Some(IndexState::Active),
            _ => None,
        }
    }

    /// 書込経路が索引行を保守すべき状態か（building も含む＝write-through）。
    pub(crate) fn is_maintained(self) -> bool {
        matches!(self, IndexState::Building | IndexState::Active)
    }
}

pub use backfill::{
    IndexBackfillStatus, IndexBackfillStep, IndexBackfillWorker, BACKFILL_CHUNK,
    INDEX_BACKFILL_QUEUE,
};
pub use parity::{IndexParityMismatch, IndexParityReport};
#[doc(hidden)]
pub use partition::analyze_index_partitions_without_stats;
pub use promote::{
    FieldIndexStatus, IndexPromotion, IndexStatus, TableIndexOverview, DEFAULT_MAX_INDEXED_FIELDS,
    SYNC_INDEX_MAX_ROWS,
};
pub use scan::{sort_precision, SortPrecision};
#[doc(hidden)]
pub use write::text_index_delete_sql;
