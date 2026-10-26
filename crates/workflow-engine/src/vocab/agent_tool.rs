//! agent.invoke に提示できる道具（ツール）の閉集合と、その単一定義（BUG-6・D-22・D-29・codegen が正）。
//!
//! 道具の名前は能力 API の名前（`rag.search` など）にそろえる。要求スコープは [`Scope::for_api`] の表から
//! 引くので、scope 天井・保存時の宣言の補完・同意計画が同じ表を使う。モデルに見せる説明と入力の
//! JSON Schema・結果の上限もここだけに置き、api（ツールのアダプタ）・web（設定フォームの一覧）・
//! AI の説明文（chat の `workflow_tool::guide`）はこの定義から作る（手書きの対応物を持たない）。
//!
//! v1 は**読み取りの道具だけ**を提示する（書込は後段の能力ノードで行う・D-29）。データの道具
//! （`data.*`）は DataPort の委譲台帳を通す形で後から足すので、語彙だけ先に予約する。

use serde::Serialize;
use serde_json::{json, Value};
use ts_rs::TS;

use super::{vocab_enum, Scope};

vocab_enum! {
    /// agent.invoke の `allowed_tools` に書ける道具の閉集合。予約分は V3 が `ir.unknown_tool` で拒否する。
    pub enum AgentTool {
        /// 権限を考慮した社内検索（SearchService の pre/post-filter）。
        RagSearch => "rag.search",
        /// フォルダの中の一覧。
        StorageList => "storage.list",
        /// テキストのファイルを窓で読む（非テキストは doc.read へ誘導する）。
        StorageRead => "storage.read",
        /// 索引済みの文書（PDF・Word など）の本文を窓で読む。
        DocRead => "doc.read",
        /// 公開 web の検索（検索語が検索サービスへ送られる）。
        WebSearch => "web.search",
        /// 管理者が許可したサイトのページの取得。
        WebFetch => "web.fetch",
        /// 隔離環境で Python を実行する（外部への通信は遮断・成果物は保存しない）。
        CodeRun => "code.run",
        // ---- 予約（DataPort の委譲台帳を通す形で後から足す）----
        DataListTables => "data.list_tables",
        DataQuery => "data.query",
        DataGet => "data.get",
    }
}

/// 1 回の実行の手順の上限の既定（ノードで省略したとき）。
pub const DEFAULT_AGENT_STEPS: u32 = 8;
/// 手順の上限の天井（ノード設定でもデプロイ設定でもこれを超えない）。
pub const MAX_AGENT_STEPS: u32 = 24;
/// 実行時間の上限の既定（秒・ir.md §7 の表）。
pub const DEFAULT_AGENT_DURATION_SEC: u32 = 600;
/// 実行時間の上限の範囲（秒・ir.md §7 の表）。
pub const AGENT_DURATION_RANGE_SEC: (u32, u32) = (10, 3600);
/// 依頼文の上限（バイト）。
pub const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;
/// 参考データ（context）の上限（JSON にしたときのバイト）。
pub const MAX_CONTEXT_BYTES: usize = 64 * 1024;
/// 接続を許可する外部サイトの数の上限。
pub const MAX_EGRESS_HOSTS: usize = 20;
/// どの道具の結果にも掛ける最終の上限（字）。超えた分は切り、目印を付ける。
pub const MAX_TOOL_RESULT_CHARS: usize = 32_000;
/// 検索語の上限（字）。
pub const MAX_QUERY_CHARS: usize = 2_000;
/// URL の上限（字）。
pub const MAX_URL_CHARS: usize = 2_048;
/// 実行するコードの上限（バイト）。
pub const MAX_CODE_BYTES: usize = 64 * 1024;
/// 社内検索の件数の上限。
pub const MAX_SEARCH_TOP_K: u32 = 20;
/// 社内検索の 1 件の本文の上限（字）。
pub const MAX_SEARCH_SNIPPET_CHARS: usize = 1_500;
/// 1 回に読む字数の上限（storage.read・doc.read）。
pub const MAX_READ_WINDOW_CHARS: u32 = 20_000;
/// 1 回に読む字数の既定（storage.read・doc.read）。
pub const DEFAULT_READ_WINDOW_CHARS: u32 = 8_000;
/// 実行したコードの標準出力の上限（バイト）。
pub const MAX_CODE_STDOUT_BYTES: usize = 16 * 1024;
/// web 検索の件数の上限。
pub const MAX_WEB_RESULTS: usize = 10;

impl AgentTool {
    /// 今のステージで提示できるか（予約分は false）。
    #[must_use]
    pub const fn available(self) -> bool {
        !matches!(
            self,
            AgentTool::DataListTables | AgentTool::DataQuery | AgentTool::DataGet
        )
    }

    /// 道具が要するスコープ（[`Scope::for_api`] の表から引く・単一の対応表）。
    #[must_use]
    pub fn required_scope(self) -> Scope {
        // 語彙の全名前は for_api の表にある（テスト `every_tool_has_a_scope` で固定）。
        Scope::for_api(self.as_str()).unwrap_or(Scope::SandboxExec)
    }

    /// 社外へ届くか（検索語・取得先の URL が社外に出る）。設定フォームと同意画面で目印を出す。
    #[must_use]
    pub const fn reaches_outside(self) -> bool {
        matches!(self, AgentTool::WebSearch | AgentTool::WebFetch)
    }

    /// 利用者向けの日本語の名前。
    #[must_use]
    pub const fn label_ja(self) -> &'static str {
        match self {
            AgentTool::RagSearch => "社内検索",
            AgentTool::StorageList => "フォルダの一覧",
            AgentTool::StorageRead => "テキストのファイルを読む",
            AgentTool::DocRead => "文書の本文を読む",
            AgentTool::WebSearch => "Web 検索",
            AgentTool::WebFetch => "Web ページを読む",
            AgentTool::CodeRun => "プログラムで計算する",
            AgentTool::DataListTables => "テーブルの一覧",
            AgentTool::DataQuery => "テーブルを調べる",
            AgentTool::DataGet => "テーブルの行を読む",
        }
    }

    /// 利用者向けの 1 行の説明（何に届くか）。
    #[must_use]
    pub const fn description_ja(self) -> &'static str {
        match self {
            AgentTool::RagSearch => "実行する人が読める社内文書を探す",
            AgentTool::StorageList => "フォルダの中のファイルを一覧する",
            AgentTool::StorageRead => "テキストのファイル（.txt・.md・.csv など）を少しずつ読む",
            AgentTool::DocRead => "検索用の索引がある PDF・Word などの本文を少しずつ読む",
            AgentTool::WebSearch => "公開 Web を検索する（検索語が検索サービスに送られる）",
            AgentTool::WebFetch => "管理者が許可したサイトのページを読む",
            AgentTool::CodeRun => "隔離された環境で Python を動かす（外部へは通信しない）",
            AgentTool::DataListTables | AgentTool::DataQuery | AgentTool::DataGet => "近日対応",
        }
    }

    /// モデルに見せる説明（道具を使うかの判断材料）。
    #[must_use]
    pub const fn model_description(self) -> &'static str {
        match self {
            AgentTool::RagSearch => {
                "社内文書を検索し、関係する断片（file_id・ファイル名・本文の抜粋）を返す。\
                 実行している人が読める文書だけが返る。本文を通して読むときは doc.read に file_id を渡す。"
            }
            AgentTool::StorageList => {
                "フォルダの中のファイルとフォルダの一覧（id・名前・種類・大きさ）を返す。folder_id を省くと最上位を返す。"
            }
            AgentTool::StorageRead => {
                "テキストのファイルを offset から max_chars 字だけ読む（既定 8000 字・最大 20000 字）。\
                 続きがあれば next_offset が返るので、それを offset に渡して続きを読む。\
                 PDF・Word などテキストでないファイルは doc.read で読む。"
            }
            AgentTool::DocRead => {
                "検索用の索引がある文書（PDF・Word など）の本文を読む。位置は page（そのページ以降）か \
                 heading（見出しの一部）で指定し、どちらも無ければ先頭から読む。1 回に max_chars 字\
                 （既定 8000）まで返し、続きがあれば結果に from（と offset）の値が書いてあるので、それを渡して続きを読む。"
            }
            AgentTool::WebSearch => {
                "公開 web を検索し、タイトル・URL・抜粋を返す。社外の最新の情報が要るときだけ使う。\
                 社内の文書は rag.search で探す。"
            }
            AgentTool::WebFetch => {
                "URL のページを取得して本文を返す（リダイレクトは追わない）。取得できるのはこの手順で\
                 許可されたサイトだけ。長いページは query に知りたい論点を書くと関係する節だけが返る。"
            }
            AgentTool::CodeRun => {
                "隔離された環境で Python のコードを実行し、標準出力を返す。計算・集計・文字列の整形に使う。\
                 外部への通信はできず、ファイルは保存されない。結果は print で出す。"
            }
            AgentTool::DataListTables | AgentTool::DataQuery | AgentTool::DataGet => "",
        }
    }

    /// モデルに見せる入力の JSON Schema。
    #[must_use]
    pub fn input_schema(self) -> Value {
        let uuid = |what: &str| json!({ "type": "string", "format": "uuid", "description": what });
        let window = json!({
            "type": "integer", "minimum": 1, "maximum": MAX_READ_WINDOW_CHARS,
            "description": "1 回に読む字数（既定 8000）"
        });
        match self {
            AgentTool::RagSearch => json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "maxLength": MAX_QUERY_CHARS, "description": "探す内容（自然文・キーワード）" },
                    "top_k": { "type": "integer", "minimum": 1, "maximum": MAX_SEARCH_TOP_K, "description": "返す件数（既定 8）" }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            AgentTool::StorageList => json!({
                "type": "object",
                "properties": { "folder_id": uuid("一覧するフォルダ（省くと最上位）") },
                "additionalProperties": false
            }),
            AgentTool::StorageRead => json!({
                "type": "object",
                "properties": {
                    "file_id": uuid("読むファイル"),
                    "offset": { "type": "integer", "minimum": 0, "description": "読み始める位置（字・前回の next_offset）" },
                    "max_chars": window
                },
                "required": ["file_id"],
                "additionalProperties": false
            }),
            AgentTool::DocRead => json!({
                "type": "object",
                "properties": {
                    "file_id": uuid("読む文書（rag.search の結果の file_id）"),
                    "version": { "type": "integer", "minimum": 1, "description": "読み始めた版（前回の結果に書かれていれば渡す）" },
                    "page": { "type": "integer", "minimum": 1, "description": "このページ以降を読む" },
                    "heading": { "type": "string", "maxLength": MAX_QUERY_CHARS, "description": "この文字列を含む見出しから読む" },
                    "from": { "type": "integer", "minimum": 0, "description": "続きの位置（前回の結果に書かれた値）" },
                    "offset": { "type": "integer", "minimum": 0, "description": "続きの位置の中の字数（前回の結果に書かれていれば渡す）" },
                    "max_chars": window
                },
                "required": ["file_id"],
                "additionalProperties": false
            }),
            AgentTool::WebSearch => json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "maxLength": MAX_QUERY_CHARS, "description": "検索語（社外の検索サービスに送られる）" }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            AgentTool::WebFetch => json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "maxLength": MAX_URL_CHARS, "description": "取得する URL（http/https・許可されたサイトだけ）" },
                    "query": { "type": "string", "maxLength": MAX_QUERY_CHARS, "description": "知りたい論点（任意）" },
                    "offset": { "type": "integer", "minimum": 0, "description": "続きを読む開始位置（前回の結果に示された値）" }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            AgentTool::CodeRun => json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "実行する Python のコード（結果は print で出す）" }
                },
                "required": ["code"],
                "additionalProperties": false
            }),
            AgentTool::DataListTables | AgentTool::DataQuery | AgentTool::DataGet => {
                json!({ "type": "object", "additionalProperties": false })
            }
        }
    }
}

/// 道具のカタログの 1 行（設定フォームの一覧・AI の説明文の単一定義）。
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct AgentToolCatalogEntry {
    /// 道具の名前（`allowed_tools` に書く値）。
    #[ts(type = "AgentTool")]
    pub tool: AgentTool,
    /// 日本語の名前。
    pub label_ja: &'static str,
    /// 1 行の説明（何に届くか）。
    pub description_ja: &'static str,
    /// 要るスコープ（保存すると declared_scopes に補われる）。
    #[ts(type = "Scope")]
    pub required_scope: Scope,
    /// 社外へ届くか（検索語・URL が社外に出る）。
    pub reaches_outside: bool,
    /// 今のステージで選べるか（false は予約＝「近日対応」）。
    pub available: bool,
}

/// 道具のカタログ全量（[`AgentTool::ALL`] と同じ順）。
#[must_use]
pub fn agent_tool_catalog() -> Vec<AgentToolCatalogEntry> {
    AgentTool::ALL
        .iter()
        .map(|t| AgentToolCatalogEntry {
            tool: *t,
            label_ja: t.label_ja(),
            description_ja: t.description_ja(),
            required_scope: t.required_scope(),
            reaches_outside: t.reaches_outside(),
            available: t.available(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 語彙の全名前が scope の表にある（要求スコープの対応表を 2 つ持たない）。
    #[test]
    fn every_tool_has_a_scope() {
        for t in AgentTool::ALL {
            assert!(Scope::for_api(t.as_str()).is_some(), "{}", t.as_str());
            assert_eq!(AgentTool::parse(t.as_str()), Some(*t));
        }
        assert_eq!(AgentTool::RagSearch.required_scope(), Scope::RagQuery);
        assert_eq!(AgentTool::StorageRead.required_scope(), Scope::StorageRead);
        assert_eq!(AgentTool::StorageList.required_scope(), Scope::StorageRead);
        assert_eq!(AgentTool::DocRead.required_scope(), Scope::DocRead);
        assert_eq!(AgentTool::WebSearch.required_scope(), Scope::HttpEgress);
        assert_eq!(AgentTool::WebFetch.required_scope(), Scope::HttpEgress);
        // コードの実行は独立の同意（予約の sandbox.exec を使う・D-29）。
        assert_eq!(AgentTool::CodeRun.required_scope(), Scope::SandboxExec);
        assert_eq!(AgentTool::DataQuery.required_scope(), Scope::DataRead);
    }

    /// v1 は読み取りの道具だけ（書込の道具は語彙にも無い・D-29）。予約は data.* だけ。
    #[test]
    fn v1_offers_read_tools_only() {
        let available: Vec<&str> = AgentTool::ALL
            .iter()
            .filter(|t| t.available())
            .map(|t| t.as_str())
            .collect();
        assert_eq!(
            available,
            [
                "rag.search",
                "storage.list",
                "storage.read",
                "doc.read",
                "web.search",
                "web.fetch",
                "code.run"
            ]
        );
        for t in AgentTool::ALL {
            assert_ne!(
                t.required_scope(),
                Scope::StorageWrite,
                "書込の道具がある: {}",
                t.as_str()
            );
            assert_ne!(t.required_scope(), Scope::DataWrite, "{}", t.as_str());
        }
    }

    /// 選べる道具は説明と入力の形を持つ（モデルが呼び方を知る）。
    #[test]
    fn available_tools_describe_themselves() {
        for t in AgentTool::ALL.iter().filter(|t| t.available()) {
            assert!(!t.model_description().is_empty(), "{}", t.as_str());
            let schema = t.input_schema();
            assert_eq!(schema["type"], "object", "{}", t.as_str());
            assert_eq!(schema["additionalProperties"], false, "{}", t.as_str());
        }
        let catalog = agent_tool_catalog();
        assert_eq!(catalog.len(), AgentTool::ALL.len());
        assert!(catalog
            .iter()
            .any(|e| e.tool == AgentTool::WebFetch && e.reaches_outside));
    }
}
