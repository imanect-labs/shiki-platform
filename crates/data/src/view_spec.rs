//! 型付きの表示設定 `ViewSpec`（Task 15.5・data-platform §8.2）。
//!
//! v1 の `DataViewBody.display` はサーバ非解釈の JSON で、フロントが解釈規約を持てなかった。
//! ここで**Rust 型を単一ソース**として定義し、utoipa で OpenAPI → TypeScript へ生成する
//! （手書きの対応物を作らない）。
//!
//! - `kind` で分けたタグ付き enum（`grid` / `kanban` / `calendar` / `gallery`）。`form` は v1 スコープ外（§15）。
//! - 列は**参照名 `key`**で指す（外部契約は `key`・表示名はいつでも変わるので使わない）。擬似列
//!   `_owner` / `_created_at` / `_updated_at` も指せる。
//! - **サーバが保存時に検証する**: 存在しない列・マスク列の表示/並べ替え/グループ化/書式条件・
//!   閲覧者ごと（`per_viewer`）/時刻依存（`volatile`）の派生列での並べ替え・グループ化を拒否する。
//!   1 ビューで解決するルックアップ・ロールアップ列は既定 12 本まで（テナント設定で変更可）。
//!   不正は 422（[`DataError::Rejected`]・`invalid_query` / 上限超過は `limit_exceeded` と
//!   `limit.name = max_view_lookup_columns`）で、理由の文言を画面へ返す。マスク列の参照だけは
//!   クエリと同じ 403。
//! - 実行（行の取得）は表示設定と無関係に必ず `run_query` を通り、**閲覧者本人の権限**で毎回評価する。
//!   表示設定は「どの列をどう見せるか」だけで、権限を広げる経路を持たない。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::derived::types::Materialization;
use crate::model::{FieldDef, FieldType, TableSchema};
use crate::query::ir::{CondOp, Condition, MAX_CONDITION_DEPTH, MAX_CONDITION_LEAVES};
use crate::query::reject::{QueryRejection, RejectCode, RejectLimit};
use crate::DataError;

/// 1 ビューで解決するルックアップ・ロールアップ列の既定の上限（SharePoint のルックアップ 12 結合に倣う・§8.2）。
pub const DEFAULT_MAX_VIEW_LOOKUP_COLUMNS: u32 = 12;
/// ルックアップ・ロールアップ列の上限として設定できる最大値。
pub const MAX_VIEW_LOOKUP_COLUMNS_CEILING: u32 = 100;
/// 列設定の数の上限（テーブルの列数の上限より十分大きい）。
pub(crate) const MAX_VIEW_COLUMNS: usize = 512;
/// 行グループ化のキー数の上限。
pub(crate) const MAX_VIEW_GROUPS: usize = 3;
/// 条件付き書式の規則数の上限。
pub(crate) const MAX_CONDITIONAL_FORMATS: usize = 32;
/// カードに出す列の上限（カンバン・ギャラリー）。
pub(crate) const MAX_CARD_FIELDS: usize = 20;
/// 列の表示名（ビュー内の上書き）の最大長（文字数）。
pub(crate) const MAX_COLUMN_LABEL_CHARS: usize = 100;
/// 列幅の範囲（px）。
pub(crate) const MIN_COLUMN_WIDTH: u16 = 40;
pub(crate) const MAX_COLUMN_WIDTH: u16 = 1200;

/// 擬似列（全行に存在するシステム列）の参照名。
const PSEUDO_KEYS: [&str; 3] = ["_owner", "_created_at", "_updated_at"];

/// 表示設定（タグ付き enum・`kind` で種別を分ける）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewSpec {
    /// 表形式（既定）。
    Grid(GridView),
    /// 単一選択の列でカラム分けしたカード。
    Kanban(KanbanView),
    /// 日付・日時の列で月の暦に配置する。
    Calendar(CalendarView),
    /// カードの一覧。
    Gallery(GalleryView),
}

impl Default for ViewSpec {
    fn default() -> Self {
        ViewSpec::Grid(GridView::default())
    }
}

/// 表形式の表示設定。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GridView {
    /// 列の設定（**並び順がそのまま表示順**）。ここに無い列はスキーマの順で末尾に表示する。
    #[serde(default)]
    pub columns: Vec<ColumnConfig>,
    /// 行の高さ。
    #[serde(default)]
    pub row_height: RowHeight,
    /// 行グループ化（先頭から順に入れ子・最大 3 段）。グループ化した列は並べ替えの先頭キーになる。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<GroupConfig>,
    /// 条件付き書式（上から順に評価し、最初に一致した規則の色を使う）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditional_format: Vec<ConditionalFormat>,
}

/// 1 列の表示設定（表示順は `GridView.columns` の並び順）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ColumnConfig {
    /// 列の参照名（`key`）か擬似列（`_owner` / `_created_at` / `_updated_at`）。
    pub field: String,
    /// このビューだけの見出し（省略時はスキーマの表示名）。テーブルの表示名は変えない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 列幅（px・40〜1,200）。省略時は既定幅。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u16>,
    /// 非表示。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// 左に固定する（固定列は先頭から連続している必要がある）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub frozen: bool,
    /// この列で並べ替えるときに代わりに使う列（日本語の読み仮名列など・data-platform §3.4）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
}

/// 行の高さ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RowHeight {
    #[default]
    Short,
    Medium,
    Tall,
}

/// 行グループ化の 1 段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupConfig {
    /// グループ化する列（索引を使って並べ替えられる列）。
    pub field: String,
    /// 降順。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub descending: bool,
    /// 初期表示で畳む。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub collapsed: bool,
}

/// 条件付き書式の色（閉じた語彙・テーマのトークンに写す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FormatColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Gray,
}

/// 条件付き書式の 1 規則（条件は**閉じた条件木**＝クエリ IR の `Condition` と同じ語彙・意味論）。
///
/// 条件は表示中の行の値（閲覧者本人に見える値）で評価する。マスク列・リンクの半結合・`matches` は書けない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalFormat {
    pub condition: Condition,
    pub color: FormatColor,
    /// 塗る列（空なら行全体）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// 太字にする。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
}

/// カンバン（単一選択の列でカラム分け）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KanbanView {
    /// カラム分けに使う単一選択（`select`）の列。
    pub group_field: String,
    /// カードに出す列（先頭がカードの見出し）。
    #[serde(default)]
    pub card_fields: Vec<String>,
    /// 1 枚もカードが無いカラムを隠す。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hide_empty: bool,
}

/// カレンダー（日付・日時の列で月の暦に配置）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CalendarView {
    /// 配置に使う日付（`date` / `date_time`）の列か擬似列 `_created_at` / `_updated_at`。
    pub date_field: String,
    /// 予定の見出しにする列（省略時はレコードの先頭の列）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_field: Option<String>,
}

/// ギャラリー（カードの一覧）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GalleryView {
    /// カードの見出しにする列（省略時はレコードの先頭の列）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_field: Option<String>,
    /// カードに出す列。
    #[serde(default)]
    pub card_fields: Vec<String>,
}

/// 検証の材料（スキーマ・保存者に見えない列・テナントの上限）。
pub(crate) struct ViewCheck<'a> {
    pub schema: &'a TableSchema,
    /// 保存者本人に**読めない**列の参照名（フィールドマスク）。
    pub masked: &'a HashSet<String>,
    /// 1 ビューのルックアップ・ロールアップ列の上限（テナント設定）。
    pub max_lookup_columns: u32,
}

/// 列の解決結果（擬似列はスキーマに無い）。
enum Col<'a> {
    Pseudo,
    Field(&'a FieldDef),
}

/// 表示設定の不正（422・構造化した理由。画面がそのまま理由を出せる）。
fn invalid(msg: impl Into<String>) -> DataError {
    DataError::Rejected(Box::new(QueryRejection::new(RejectCode::InvalidQuery, msg)))
}

impl ViewCheck<'_> {
    /// 列を解決する（存在しない列は 422 相当の不正入力・マスク列の参照は 403）。
    fn col(&self, key: &str, what: &str) -> Result<Col<'_>, DataError> {
        if PSEUDO_KEYS.contains(&key) {
            return Ok(Col::Pseudo);
        }
        let f = self.schema.field(key).ok_or_else(|| {
            invalid(format!(
                "{what}: フィールド '{key}' はスキーマに存在しません"
            ))
        })?;
        if self.masked.contains(key) {
            // マスク列は表示・並べ替え・条件のいずれにも出せない（PIT-19・クエリと同じ 403）。
            return Err(DataError::Forbidden);
        }
        Ok(Col::Field(f))
    }

    /// 並べ替え・グループ化に使える列か（閲覧者ごと・時刻依存の派生列は索引が無く並べ替えられない）。
    fn sortable(&self, key: &str, what: &str) -> Result<(), DataError> {
        match self.col(key, what)? {
            Col::Pseudo => Ok(()),
            Col::Field(f) => {
                if let Some(m @ (Materialization::PerViewer | Materialization::Volatile)) =
                    f.materialization
                {
                    let why = if m == Materialization::PerViewer {
                        "閲覧者ごとに値が変わる列"
                    } else {
                        "時刻に依存する列"
                    };
                    return Err(invalid(format!(
                        "{what}: フィールド '{key}' は{why}のため並べ替え・グループ化に使えません"
                    )));
                }
                if matches!(f.field_type, FieldType::MultiSelect)
                    || f.field_type == FieldType::RecordRef
                {
                    return Err(invalid(format!(
                        "{what}: フィールド '{key}' は複数の値を持つため並べ替え・グループ化に使えません"
                    )));
                }
                Ok(())
            }
        }
    }

    fn check_cards(&self, fields: &[String], what: &str) -> Result<(), DataError> {
        if fields.len() > MAX_CARD_FIELDS {
            return Err(invalid(format!(
                "{what}: カードに出す列は {MAX_CARD_FIELDS} 個までです"
            )));
        }
        let mut seen = HashSet::new();
        for f in fields {
            if !seen.insert(f.as_str()) {
                return Err(invalid(format!(
                    "{what}: フィールド '{f}' が重複しています"
                )));
            }
            self.col(f, what)?;
        }
        Ok(())
    }

    fn check_condition(
        &self,
        c: &Condition,
        depth: usize,
        leaves: &mut usize,
    ) -> Result<(), DataError> {
        if depth > MAX_CONDITION_DEPTH {
            return Err(invalid("条件付き書式: 条件の入れ子が深すぎます"));
        }
        match c {
            Condition::All(v) | Condition::Any(v) => {
                for x in v {
                    self.check_condition(x, depth + 1, leaves)?;
                }
                Ok(())
            }
            Condition::Not(x) => self.check_condition(x, depth + 1, leaves),
            Condition::Link(_) => Err(invalid(
                "条件付き書式: リンク先の条件は使えません（表示中の行の値だけで判定します）",
            )),
            Condition::Cmp(cmp) => {
                *leaves += 1;
                if *leaves > MAX_CONDITION_LEAVES {
                    return Err(invalid("条件付き書式: 条件が多すぎます"));
                }
                self.col(&cmp.field, "条件付き書式")?;
                match cmp.op {
                    CondOp::Matches => {
                        Err(invalid("条件付き書式: 正規表現（matches）は使えません"))
                    }
                    CondOp::Exists | CondOp::IsNull => Ok(()),
                    CondOp::In => match &cmp.value {
                        Some(serde_json::Value::Array(a)) if a.len() <= 100 => Ok(()),
                        _ => Err(invalid(
                            "条件付き書式: in の値は 100 個までの配列で指定してください",
                        )),
                    },
                    _ => match &cmp.value {
                        Some(v) if !v.is_array() && !v.is_object() && !v.is_null() => Ok(()),
                        _ => Err(invalid(format!(
                            "条件付き書式: '{}' の比較の値を指定してください",
                            cmp.field
                        ))),
                    },
                }
            }
        }
    }

    fn check_grid(&self, g: &GridView) -> Result<(), DataError> {
        if g.columns.len() > MAX_VIEW_COLUMNS {
            return Err(invalid(format!("列の設定は {MAX_VIEW_COLUMNS} 個までです")));
        }
        let mut seen = HashSet::new();
        let mut frozen_run_ended = false;
        for c in &g.columns {
            if !seen.insert(c.field.as_str()) {
                return Err(invalid(format!(
                    "列の設定: フィールド '{}' が重複しています",
                    c.field
                )));
            }
            // 非表示のマスク列は何も見せないので許す（他の人の画面から写した設定で保存を止めない）。
            if c.hidden && self.masked.contains(&c.field) {
                if c.sort_by.is_some() || c.label.is_some() {
                    return Err(DataError::Forbidden);
                }
                continue;
            }
            self.col(&c.field, "列の設定")?;
            if let Some(label) = &c.label {
                let n = label.chars().count();
                if n == 0 || n > MAX_COLUMN_LABEL_CHARS || label.chars().any(char::is_control) {
                    return Err(invalid(format!(
                        "列の設定: '{}' の見出しは 1〜{MAX_COLUMN_LABEL_CHARS} 文字（改行なし）で指定してください",
                        c.field
                    )));
                }
            }
            if let Some(w) = c.width {
                if !(MIN_COLUMN_WIDTH..=MAX_COLUMN_WIDTH).contains(&w) {
                    return Err(invalid(format!(
                        "列の設定: '{}' の幅は {MIN_COLUMN_WIDTH}〜{MAX_COLUMN_WIDTH} で指定してください",
                        c.field
                    )));
                }
            }
            if let Some(alt) = &c.sort_by {
                self.sortable(alt, "列の並べ替えの代替列")?;
            }
            if !c.hidden {
                if c.frozen && frozen_run_ended {
                    return Err(invalid("列の設定: 固定列は先頭から続けて並べてください"));
                }
                if !c.frozen {
                    frozen_run_ended = true;
                }
            }
        }
        if g.group_by.len() > MAX_VIEW_GROUPS {
            return Err(invalid(format!(
                "グループ化は {MAX_VIEW_GROUPS} 段までです"
            )));
        }
        let mut gseen = HashSet::new();
        for gc in &g.group_by {
            if !gseen.insert(gc.field.as_str()) {
                return Err(invalid(format!(
                    "グループ化: フィールド '{}' が重複しています",
                    gc.field
                )));
            }
            self.sortable(&gc.field, "グループ化")?;
        }
        if g.conditional_format.len() > MAX_CONDITIONAL_FORMATS {
            return Err(invalid(format!(
                "条件付き書式は {MAX_CONDITIONAL_FORMATS} 個までです"
            )));
        }
        for cf in &g.conditional_format {
            let mut leaves = 0;
            self.check_condition(&cf.condition, 1, &mut leaves)?;
            for f in &cf.fields {
                self.col(f, "条件付き書式の対象列")?;
            }
        }
        Ok(())
    }

    /// 表示設定を検証する（保存時）。
    pub(crate) fn check(&self, spec: &ViewSpec) -> Result<(), DataError> {
        match spec {
            ViewSpec::Grid(g) => self.check_grid(g)?,
            ViewSpec::Kanban(k) => {
                match self.col(&k.group_field, "カンバンのカラム")? {
                    Col::Field(f) if f.field_type == FieldType::Select => {}
                    _ => {
                        return Err(invalid(format!(
                        "カンバンのカラム: フィールド '{}' は単一選択（select）の列にしてください",
                        k.group_field
                    )))
                    }
                }
                self.check_cards(&k.card_fields, "カンバンのカード")?;
            }
            ViewSpec::Calendar(c) => {
                match self.col(&c.date_field, "カレンダーの日付")? {
                    Col::Pseudo if c.date_field != "_owner" => {}
                    Col::Field(f)
                        if matches!(f.field_type, FieldType::Date | FieldType::DateTime) =>
                    {
                        self.sortable(&c.date_field, "カレンダーの日付")?;
                    }
                    _ => {
                        return Err(invalid(format!(
                            "カレンダーの日付: フィールド '{}' は日付・日時の列にしてください",
                            c.date_field
                        )))
                    }
                }
                if let Some(t) = &c.title_field {
                    self.col(t, "カレンダーの見出し")?;
                }
            }
            ViewSpec::Gallery(g) => {
                if let Some(t) = &g.title_field {
                    self.col(t, "ギャラリーの見出し")?;
                }
                self.check_cards(&g.card_fields, "ギャラリーのカード")?;
            }
        }
        Ok(())
    }

    /// 投影（取得する列）のルックアップ・ロールアップ列の本数を上限と照らす。
    pub(crate) fn check_lookup_budget(&self, projection: &[String]) -> Result<(), DataError> {
        let n = projection
            .iter()
            .filter_map(|k| self.schema.field(k))
            .filter(|f| matches!(f.field_type, FieldType::Lookup | FieldType::Rollup))
            .count();
        let cap = usize::try_from(self.max_lookup_columns).unwrap_or(usize::MAX);
        if n > cap {
            let mut r = QueryRejection::new(
                RejectCode::LimitExceeded,
                format!(
                    "1 つのビューに表示できるルックアップ・ロールアップ列は {cap} 本までです（{n} 本）"
                ),
            )
            .hint("ルックアップ・ロールアップ列をいくつか非表示にしてください");
            r.limit = Some(Box::new(RejectLimit {
                name: "max_view_lookup_columns".into(),
                max: u64::from(self.max_lookup_columns),
            }));
            return Err(DataError::Rejected(Box::new(r)));
        }
        Ok(())
    }

    /// 並べ替えのキー（クエリの `sort`）を検証する。
    pub(crate) fn check_sort_keys<'k>(
        &self,
        keys: impl IntoIterator<Item = &'k str>,
    ) -> Result<(), DataError> {
        for k in keys {
            self.sortable(k, "並べ替え")?;
        }
        Ok(())
    }
}

impl ViewSpec {
    /// この表示で取得する列（参照名・擬似列は含めない）。
    ///
    /// 表は非表示でない列（設定に無い列は表示）、カード系は見出し・カード・配置の列。保存ビューの実行で
    /// クエリに投影（`select`）が無ければこれを使い、非表示のルックアップ・ロールアップを解決しない。
    pub fn projection(&self, schema: &TableSchema) -> Vec<String> {
        let user_field = |k: &str| schema.field(k).is_some();
        let mut out: Vec<String> = Vec::new();
        let push = |k: &str, out: &mut Vec<String>| {
            if user_field(k) && !out.iter().any(|x| x == k) {
                out.push(k.to_string());
            }
        };
        match self {
            ViewSpec::Grid(g) => {
                let hidden: HashSet<&str> = g
                    .columns
                    .iter()
                    .filter(|c| c.hidden)
                    .map(|c| c.field.as_str())
                    .collect();
                for c in g.columns.iter().filter(|c| !c.hidden) {
                    push(&c.field, &mut out);
                }
                for f in &schema.fields {
                    if !hidden.contains(f.key.as_str()) {
                        push(&f.key, &mut out);
                    }
                }
            }
            ViewSpec::Kanban(k) => {
                push(&k.group_field, &mut out);
                for f in &k.card_fields {
                    push(f, &mut out);
                }
            }
            ViewSpec::Calendar(c) => {
                push(&c.date_field, &mut out);
                if let Some(t) = &c.title_field {
                    push(t, &mut out);
                }
            }
            ViewSpec::Gallery(g) => {
                if let Some(t) = &g.title_field {
                    push(t, &mut out);
                }
                for f in &g.card_fields {
                    push(f, &mut out);
                }
            }
        }
        out
    }

    /// グループ化の列（並べ替えの先頭キーになる）。
    pub fn group_fields(&self) -> Vec<&str> {
        match self {
            ViewSpec::Grid(g) => g.group_by.iter().map(|x| x.field.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    /// 列の並べ替えの代替列（`sort_by`）。
    pub fn sort_alternatives(&self) -> Vec<&str> {
        match self {
            ViewSpec::Grid(g) => g
                .columns
                .iter()
                .filter_map(|c| c.sort_by.as_deref())
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
#[path = "view_spec_tests.rs"]
mod tests;
