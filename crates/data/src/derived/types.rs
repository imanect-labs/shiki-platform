//! リンク・ロールアップ・材料化規則の型（data-platform §7.1・§7.3・§7.4）。DTO 兼用（utoipa）。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::model::FieldId;

/// リンクのカーディナリティ（src = この列を持つテーブル側・dst = 参照先）。
///
/// 値の表現は常に配列（単一値の側は要素数 1 まで）。一意性は `data_link` の部分一意索引で DB が強制する
/// （`single_valued` は `data_link_field` への複合 FK で宣言値に固定・PIT-72）。
///
/// | カーディナリティ | 順方向行の `single_valued` | 逆方向行の `single_valued` |
/// |---|---|---|
/// | `one_to_one` | true | true |
/// | `one_to_many`（src 1 : dst 複数） | false | true |
/// | `many_to_one`（src 複数 : dst 1。v1 の `record_ref` と同じ向き） | true | false |
/// | `many_to_many` | false | false |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkCardinality {
    OneToOne,
    OneToMany,
    ManyToOne,
    ManyToMany,
}

impl LinkCardinality {
    /// この列（src 側）の値が 1 件までか（順方向行の `single_valued`）。
    pub fn src_single(self) -> bool {
        matches!(self, LinkCardinality::OneToOne | LinkCardinality::ManyToOne)
    }

    /// 各参照先（dst 側）が 1 つの src からしか指されないか（逆方向行の `single_valued`）。
    pub fn dst_single(self) -> bool {
        matches!(self, LinkCardinality::OneToOne | LinkCardinality::OneToMany)
    }

    /// 対称列（参照先から見た向き）のカーディナリティ。
    #[must_use]
    pub fn mirror(self) -> LinkCardinality {
        match self {
            LinkCardinality::OneToMany => LinkCardinality::ManyToOne,
            LinkCardinality::ManyToOne => LinkCardinality::OneToMany,
            other => other,
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde の skip_serializing_if は参照を渡す。
fn is_false(b: &bool) -> bool {
    !*b
}

/// リンク定義（`record_ref` 型のフィールドに付ける・data-platform §7.1）。
///
/// 参照先テーブルは `FieldDef.ref_table`（v1 の `record_ref` と同じ場所）。`link` を持たない
/// `record_ref` は v1 互換の `many_to_one`・片方向として扱う（値は要素 1 件までの配列）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LinkDef {
    pub cardinality: LinkCardinality,
    /// 参照先に置いた対称列（逆方向行の `field_id`）。サーバが採番する。`None` なら片方向リンク。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symmetric_field: Option<FieldId>,
    /// 対称列を参照先に作るときの参照名（作成時の入力。参照先テーブルの owner 権限が要る）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symmetric_key: Option<String>,
    /// 対称列を参照先に作るときの表示名（作成時の入力・省略時は参照名）。作成後の変更は参照先の
    /// スキーマの改訂で行う（ここを書き換えても対称列の表示名は変わらない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symmetric_display_name: Option<String>,
    /// この列が対称列（逆方向・読取専用）か。サーバだけが設定する（入力では作れない）。
    ///
    /// 対称列は**スキーマ上のメタデータ**で、値は本体に持たない。順方向の書込と同じ Tx で書いた
    /// 逆方向行（`data_link.is_reverse`）を、閲覧者本人の権限で読取時に引く。
    #[serde(default, skip_serializing_if = "is_false")]
    pub reverse: bool,
}

/// ロールアップの演算（data-platform §7.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RollupOp {
    /// 可視なリンク先の件数（`target` 不要）。
    Count,
    /// 合計（number・`numeric` 精度で正確）。
    Sum,
    /// 平均（number）。
    Avg,
    /// 最小（number / date / date_time / text）。
    Min,
    /// 最大（number / date / date_time / text）。
    Max,
    /// 連結（`, ` 区切り・リンクの並び順）。
    Concat,
}

/// ロールアップ定義: 自テーブルのリンク列 `link` を辿り、参照先の `target` を `op` で集計する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RollupDef {
    /// 自テーブルのリンク列（`record_ref`）の参照名。
    pub link: String,
    /// 参照先テーブルのフィールドの参照名（`count` では省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub op: RollupOp,
}

/// 派生値の材料化可否（data-platform §7.4・PIT-71）。**サーバがスキーマ保存時に決める**（入力は無視）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Materialization {
    /// 全閲覧者・全時刻で同値と証明できる。`indexed` なら書込時に材料化し、フィルタ・ソートに使える。
    Invariant,
    /// 閲覧者ごとに値が変わる。読取時計算のみ・キャッシュ禁止・索引/ソート/フィルタ不可。
    PerViewer,
    /// 時刻に依存する（`now` を含む）。読取時計算のみ・索引不可。
    Volatile,
}

impl Materialization {
    /// 依存の合成（閲覧者依存が最優先・次に時刻依存）。
    pub(crate) fn join(self, other: Materialization) -> Materialization {
        use Materialization::{Invariant, PerViewer, Volatile};
        match (self, other) {
            (PerViewer, _) | (_, PerViewer) => PerViewer,
            (Volatile, _) | (_, Volatile) => Volatile,
            _ => Invariant,
        }
    }
}

/// 派生フィールドの値の型（**サーバがスキーマ保存時に推論する**・入力は無視）。表示と索引の表の選択に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResultType {
    Number,
    Text,
    Date,
    DateTime,
    Bool,
    /// 多値（多値リンク越しの lookup 等・配列）。
    List,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinality_flags_match_the_design_table() {
        use LinkCardinality::*;
        let got: Vec<(bool, bool)> = [OneToOne, OneToMany, ManyToOne, ManyToMany]
            .iter()
            .map(|c| (c.src_single(), c.dst_single()))
            .collect();
        assert_eq!(
            got,
            vec![(true, true), (false, true), (true, false), (false, false)]
        );
        for c in [OneToOne, OneToMany, ManyToOne, ManyToMany] {
            assert_eq!(c.mirror().mirror(), c);
            assert_eq!(c.mirror().src_single(), c.dst_single());
        }
    }

    #[test]
    fn materialization_join_prefers_viewer_dependence() {
        use Materialization::*;
        assert_eq!(Invariant.join(Invariant), Invariant);
        assert_eq!(Invariant.join(Volatile), Volatile);
        assert_eq!(Volatile.join(PerViewer), PerViewer);
        assert_eq!(PerViewer.join(Invariant), PerViewer);
    }

    #[test]
    fn link_def_serde_is_snake_case_and_hides_defaults() {
        let d = LinkDef {
            cardinality: LinkCardinality::ManyToMany,
            symmetric_field: None,
            symmetric_key: Some("projects".into()),
            symmetric_display_name: None,
            reverse: false,
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["cardinality"], "many_to_many");
        assert!(v.get("reverse").is_none());
        assert!(v.get("symmetric_field").is_none());
    }
}
