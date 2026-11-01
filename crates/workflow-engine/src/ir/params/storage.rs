//! storage / rag ノードの params 契約（ir.md §7）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ir::expr::ValueExpr;

/// `storage.read` — ファイル読み（≤10MB・pure）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StorageReadParams {
    /// 読むファイル id（UUID に解決される値）。
    #[serde(default = "super::unset")]
    pub file: ValueExpr,
}

/// 保存先に同じ名前のファイルがあるときの扱い（`storage.write`・`csv.write`・BUG-97・D-97）。
///
/// 省略は [`NameTakenPolicy::Fail`]（この項目が無かったころの定義の挙動を変えない）。エディタで新しく置く
/// ブロックは [`NameTakenPolicy::AddNumber`] を入れる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NameTakenPolicy {
    /// 保存せずに失敗にする（`name_taken`・既存のファイルは変えない）。
    #[default]
    Fail,
    /// 名前に ` (2)` ` (3)` … を付けて、空いている名前で保存する（` (50)` まで）。番号は StorageService が付け
    /// （D-112・D-124）、規則は `storage::numbered_name` の 1 か所にある（エンジンは写しを持たない）。
    AddNumber,
}

/// 新しく保存する CSV の文字コード（`csv.write`・`storage.write` で CSV として保存するとき・GAP-47・D-120）。
///
/// 省略は [`CsvEncoding::Utf8`]（BOM なし。この項目が無かったころの定義の挙動を変えない）。エディタで新しく置く
/// ブロックは [`CsvEncoding::Utf8Bom`] を入れる（日本語の Windows の Excel は、BOM の無い UTF-8 を Shift_JIS として
/// 開いて文字化けする）。値の名前は保存される IR の値なので変えない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CsvEncoding {
    /// UTF-8（BOM 付き）。日本語の Windows の Excel でそのまま開ける。
    Utf8Bom,
    /// UTF-8（BOM なし）。
    #[default]
    Utf8,
    /// Shift_JIS（CP932）。Shift_JIS だけを読む古い仕組みへ渡す CSV など。Shift_JIS に無い文字は書けない。
    ShiftJis,
}

/// ファイルの種類を決めない保存で、ファイル名の拡張子から決める種類（D-97）。
///
/// エディタの「ファイルの種類」の選択肢と同じ 5 つ（社内文書の検索が読む種類）だけを決め、ほかは `None`
/// （呼び出し側は種類の分からないファイル `application/octet-stream` として保存する）。
#[must_use]
pub fn content_type_from_name(file_name: &str) -> Option<&'static str> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    match ext.to_ascii_lowercase().as_str() {
        "txt" => Some("text/plain"),
        "md" | "markdown" => Some("text/markdown"),
        "csv" => Some("text/csv"),
        "json" => Some("application/json"),
        "html" | "htm" => Some("text/html"),
        _ => None,
    }
}

/// `storage.write` — 新しいファイルの保存（engine-dedup で高々 1 回）。
///
/// 既存のファイルを上書きせず、新しいバージョンも足さない。同じ名前のファイルがあるときは
/// [`Self::on_name_taken`] に従う（失敗にするか、番号を付けた名前で新しいファイルを作る）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StorageWriteParams {
    /// 書き先フォルダ id（省略時は実行主体のルート）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub folder: Option<ValueExpr>,
    /// ファイル名。
    pub name: ValueExpr,
    /// 内容（文字列＝UTF-8・`{ "base64": ... }`＝バイナリ・その他 JSON＝直列化）。
    pub content: ValueExpr,
    /// Content-Type（省略時はファイル名の拡張子から決め・決まらなければ application/octet-stream）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content_type: Option<ValueExpr>,
    /// 同じ名前のファイルがあるとき（省略時は失敗にする）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub on_name_taken: Option<NameTakenPolicy>,
    /// CSV として保存するとき（種類が `text/csv`）の文字コード（省略時は BOM なしの UTF-8・D-120）。ほかの種類では
    /// 使わない（文章は UTF-8 のまま保存する）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub encoding: Option<CsvEncoding>,
}

/// `storage.list` — フォルダ一覧（pure）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct StorageListParams {
    /// 一覧するフォルダ id（省略時はルート）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub folder: Option<ValueExpr>,
    /// 続きの位置（前の「フォルダ一覧」の結果の `next_cursor`・同じフォルダーのもの）。省略・null・空の文字は先頭から
    /// 読む（この項目が無かったころの定義の挙動・D-131）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cursor: Option<ValueExpr>,
}

/// `rag.search` — permission-aware 検索（必要スコープは `rag.query`・pure）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RagSearchParams {
    /// 検索クエリ。
    pub query: ValueExpr,
    /// 取得件数（省略時は検索サービスの既定）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub top_k: Option<ValueExpr>,
}
