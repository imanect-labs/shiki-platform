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
    /// 名前に ` (2)` ` (3)` … を付けて、空いている名前で保存する（上限 [`MAX_NUMBERED_NAME`]）。
    AddNumber,
}

/// 番号を付けて試す上限（` (2)` 〜 ` (50)`）。正本は `storage::MAX_NUMBERED_NAME`（同じ数の写し）。
pub const MAX_NUMBERED_NAME: u32 = 50;

/// `attempt` 回目に試す名前（1 回目は元の名前・2 回目からは拡張子の前に ` (n)`）。
///
/// 正本は `storage::numbered_name`（番号は StorageService が付ける・D-112）。ここは、ポートの要求が扱いを運ばない
/// `csv.write` の試し直しと、上限まで埋まったときの誤りの文言（「名前 (50).md」）のための写し。workflow-engine は
/// storage に依存しない（`nodes::ports`）ので写しを持ち、api のテストで両者が同じ名前を作ることを照らす。
#[must_use]
pub fn numbered_name(file_name: &str, attempt: u32) -> String {
    if attempt <= 1 {
        return file_name.to_string();
    }
    match file_name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem} ({attempt}).{ext}"),
        None => format!("{file_name} ({attempt})"),
    }
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
