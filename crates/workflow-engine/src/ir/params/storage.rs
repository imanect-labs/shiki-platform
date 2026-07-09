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

/// `storage.write` — ファイル書き（新バージョン追記・engine-dedup で高々 1 回）。
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
    /// Content-Type（省略時 application/octet-stream）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content_type: Option<ValueExpr>,
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
