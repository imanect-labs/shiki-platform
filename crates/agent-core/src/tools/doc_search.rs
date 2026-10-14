//! `doc_search` ツール（Task 3.4）＋古典 RAG 注入の共有ヘルパ。
//!
//! Phase 2 の permission-aware 検索（`rag::SearchService`）を**呼び出し時のユーザー権限で**実行する。
//! 二段 authz（pre-filter＋post-filter・file 粒度 HigherConsistency）と引用監査は `SearchService`
//! 内で走るため、ここは `&AuthContext` を素通しするだけで confused-deputy を避けられる。
//!
//! - **エージェントモード**: [`DocSearchTool`]（LLM が自律的に呼ぶ）。
//! - **通常チャット（OFF）**: chat ドメインが [`run_doc_search`] を事前に直接呼び、文脈注入する。

use std::sync::Arc;

use authz::AuthContext;
use rag::{SearchMode, SearchResult, SearchScope, SearchService};

use crate::tool::{Citation, Tool, ToolError, ToolOutcome};

/// doc_search の実行結果（引用＋モデル/注入用テキスト）。
#[derive(Debug, Clone, PartialEq)]
pub struct DocSearchResult {
    /// UI/監査へ流す引用チャンク。
    pub citations: Vec<Citation>,
    /// モデルが読む観測テキスト（tool_result content）／古典注入の文脈本文。
    pub context_text: String,
}

/// 該当なしの観測テキスト（絞り込みの交差が空で検索しなかった場合も同じ文言にする）。
const NO_RESULTS: &str = "検索結果はありませんでした（権限内に該当文書なし）。";

/// モデルが 1 回の検索で求められる件数の上限。設定の `max_top_k`（API 向け・既定 50）より
/// 小さく抑える: 観測は丸ごとコンテキストに載るので、50 件だと 1 回で数万トークンになり得る。
const MAX_TOOL_TOP_K: u32 = 20;

/// モデルが一度に指定できる絞り込み対象の上限（フォルダ・ファイルそれぞれ）。
const MAX_NARROW_IDS: usize = 20;

/// 検索結果 → Citation（番号は未採番。ループの台帳が振る・#508）。
fn to_citation(r: &SearchResult) -> Citation {
    Citation {
        cite_id: 0,
        node_id: r.file_id.to_string(),
        chunk_id: r.chunk_id.to_string(),
        snippet: r.content.clone(),
        page: r.page,
        heading_path: r.heading_path.clone(),
        score: r.score,
        version: Some(r.version),
        anchor: r.anchor,
        quote: r.quote.clone(),
        boxes: r.boxes.clone(),
    }
}

/// permission-aware 検索を呼び出しユーザーの権限で実行し、引用＋文脈テキストへ写す。
///
/// エージェントの doc_search ツールと通常チャットの古典 RAG 注入の**単一実装**。
///
/// 文脈テキストの番号は仮の印（[`crate::cite::placeholder`]）で書く。呼び出し側が
/// [`crate::CitationLedger::number`] で応答内の通し番号に置き換える（呼び出しごとに 1 から
/// 振ると、2 回目の検索の `[n]` が別の文書を指してしまう・#508）。
pub async fn run_doc_search(
    search: &SearchService,
    ctx: &AuthContext,
    query: &str,
    top_k: Option<u32>,
    scope: Option<&SearchScope>,
    trace_id: Option<&str>,
) -> Result<DocSearchResult, ToolError> {
    // 古典注入にはツールループが無く、モデルが ID を使い道が無いので載せない（トークンの無駄・
    // 回答へ UUID が写る）。
    search_docs(
        search,
        ctx,
        &SearchParams {
            query,
            top_k,
            mode: SearchMode::Hybrid,
            base: scope,
            narrow: &SearchScope::default(),
            with_ids: false,
        },
        trace_id,
    )
    .await
}

/// 検索パラメータ（[`search_docs`] の引数束）。
struct SearchParams<'a> {
    query: &'a str,
    top_k: Option<u32>,
    mode: SearchMode,
    /// 知識スコープ（skill）。None は全可読範囲。
    base: Option<&'a SearchScope>,
    /// モデルの絞り込み（空 = 絞らない）。`base` との交差で検索する。
    narrow: &'a SearchScope,
    /// 観測テキストに各結果の `file_id`（と `folder_id`）を載せるか（ツール経路のみ）。
    /// モデルが「この文書に絞って再検索」（[`DocSearchTool`] の `file_ids`）や本文の読み出しを
    /// 指定できるようにするため。載るのは post-filter を通過したチャンクの ID だけなので、
    /// 読めない文書の存在は漏れない。
    with_ids: bool,
}

/// [`run_doc_search`] と [`DocSearchTool`] の本体。
async fn search_docs(
    search: &SearchService,
    ctx: &AuthContext,
    p: &SearchParams<'_>,
    trace_id: Option<&str>,
) -> Result<DocSearchResult, ToolError> {
    let query = p.query.trim();
    if query.is_empty() {
        return Err(ToolError::Invalid("query is empty".into()));
    }
    // 絞り込みは知識スコープとの交差（union だと skill の範囲を広げられる）。交差が空なら
    // 検索せずに 0 件・交差で検索するときは元の知識スコープで候補を再検証する（SearchService 側）。
    let out = search
        .search_narrowed(ctx, query, p.top_k, p.mode, p.base, p.narrow, trace_id)
        .await
        .map_err(|e| ToolError::Unavailable(format!("doc_search failed: {e}")))?;

    let citations: Vec<Citation> = out.results.iter().map(to_citation).collect();
    let context_text = context_text(&out.results, p.with_ids);
    Ok(DocSearchResult {
        citations,
        context_text,
    })
}

/// 検索結果 → モデルが読む観測テキスト。
fn context_text(results: &[SearchResult], with_ids: bool) -> String {
    if results.is_empty() {
        NO_RESULTS.to_string()
    } else {
        use std::fmt::Write as _;
        // 引用の書き方を結果と一緒に示す（モデルは番号の書式を結果から真似る）。
        let mut s = format!(
            "検索結果 {} 件（回答で根拠にした箇所は、その文の末尾に結果の番号を [n] の形で付ける）:\n",
            results.len()
        );
        for (i, r) in results.iter().enumerate() {
            let heading = if r.heading_path.is_empty() {
                String::new()
            } else {
                format!(" / {}", r.heading_path.join(" > "))
            };
            let ids = if with_ids {
                let folder = r
                    .folder_id
                    .map(|f| format!(", folder_id: {f}"))
                    .unwrap_or_default();
                format!("（file_id: {}{folder}）", r.file_id)
            } else {
                String::new()
            };
            // 文書側に仮の印の形の文字列があっても、番号に化けないよう無害化する。
            let _ = write!(
                s,
                "{} 出典: {}{}{}\n{}\n\n",
                crate::cite::placeholder(i),
                crate::cite::defuse(&r.file_name),
                crate::cite::defuse(&heading),
                ids,
                crate::cite::defuse(r.content.trim())
            );
        }
        s
    }
}

/// `doc_search` ツール（エージェントモード）。呼び出しユーザーの権限で検索する。
///
/// モデルは `query` に加えて件数（`top_k`）・モード（`hybrid` / `keyword`）・絞り込み
/// （`file_ids` / `folder_ids`）を指定できる（#423）。絞り込みは知識スコープとの**交差**で、
/// 読める範囲を広げる方向には一切働かない（最終可読性は SearchService の post-filter が再検証）。
pub struct DocSearchTool {
    search: Arc<SearchService>,
    /// 知識スコープ（skill・Task 6.8）。None は全可読範囲。
    scope: Option<SearchScope>,
}

impl DocSearchTool {
    pub fn new(search: Arc<SearchService>) -> Self {
        DocSearchTool {
            search,
            scope: None,
        }
    }

    /// 知識スコープ付きで構成する（skill 適用時）。スコープは絞り込みのみで、
    /// 最終可読性は SearchService の post-filter が常に再検証する。
    pub fn with_scope(search: Arc<SearchService>, scope: Option<SearchScope>) -> Self {
        DocSearchTool {
            search,
            scope: scope.filter(|s| !s.is_empty()),
        }
    }
}

impl DocSearchTool {
    /// ツールで求められる件数の上限（設定の `max_top_k` とツール上限の小さい方）。
    fn max_top_k(&self) -> u32 {
        u32::try_from(self.search.max_top_k())
            .unwrap_or(u32::MAX)
            .min(MAX_TOOL_TOP_K)
    }
}

/// ツール引数を解釈したもの。
#[derive(Debug, PartialEq)]
struct DocSearchInput {
    query: String,
    top_k: Option<u32>,
    mode: SearchMode,
    narrow: SearchScope,
}

impl DocSearchInput {
    fn parse(input: &serde_json::Value) -> Result<Self, ToolError> {
        let query = input
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::Invalid("missing 'query'".into()))?
            .trim()
            .to_string();
        if query.is_empty() {
            return Err(ToolError::Invalid("query is empty".into()));
        }
        let top_k = match input.get("top_k") {
            None | Some(serde_json::Value::Null) => None,
            // `10.0` のように小数表記で来る整数も受ける（非 strict のツール呼び出しで起きる）。
            // 上限は SearchService が max_top_k で clamp し、ツールではさらに MAX_TOOL_TOP_K に抑える。
            Some(v) => Some(
                v.as_f64()
                    .filter(|n| n.fract() == 0.0 && *n >= 1.0)
                    // 1 以上・上限で丸めた整数値なので符号落ち・切り捨ては起きない。
                    .map(|n| {
                        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                        let n = n.min(f64::from(MAX_TOOL_TOP_K)) as u32;
                        n
                    })
                    .ok_or_else(|| ToolError::Invalid("'top_k' must be an integer >= 1".into()))?,
            ),
        };
        // 閉じた語彙。`dense` 単独はデバッグ比較用なのでモデルには開かない。
        let mode = match input.get("mode") {
            None | Some(serde_json::Value::Null) => SearchMode::Hybrid,
            Some(v) => match v.as_str() {
                Some("hybrid") => SearchMode::Hybrid,
                Some("keyword") => SearchMode::Keyword,
                _ => {
                    return Err(ToolError::Invalid(format!(
                        "'mode' must be \"hybrid\" or \"keyword\" (got {v})"
                    )))
                }
            },
        };
        let narrow = SearchScope {
            folders: parse_ids(input, "folder_ids")?,
            files: parse_ids(input, "file_ids")?,
        };
        Ok(DocSearchInput {
            query,
            top_k,
            mode,
            narrow,
        })
    }
}

/// `file_ids` / `folder_ids` を UUID 列として読む（未指定は空）。
fn parse_ids(input: &serde_json::Value, key: &str) -> Result<Vec<uuid::Uuid>, ToolError> {
    let Some(v) = input.get(key).filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let items = v
        .as_array()
        .ok_or_else(|| ToolError::Invalid(format!("'{key}' must be an array of ids")))?;
    if items.len() > MAX_NARROW_IDS {
        return Err(ToolError::Invalid(format!(
            "'{key}' accepts at most {MAX_NARROW_IDS} ids"
        )));
    }
    items
        .iter()
        .map(|item| {
            item.as_str()
                .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
                .ok_or_else(|| {
                    ToolError::Invalid(format!("'{key}' contains an invalid id: {item}"))
                })
        })
        .collect()
}

#[async_trait::async_trait]
impl Tool for DocSearchTool {
    // トレイト定義が `-> &str` のため literal 返しでも &'static 化できない（allow）。
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        crate::vocab::ToolName::DocSearch.as_str()
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "社内文書を検索し、権限を守った引用チャンクを返す。ユーザーの質問に答えるための根拠が必要なときに使う。\n\
         - mode: 既定の hybrid は意味の近さと語の一致を合わせて探す。型番・エラーメッセージ・条文番号・\
         固有名詞など、表記どおりに一致する箇所が欲しいときは keyword にする。\n\
         - file_ids / folder_ids: 結果に出た文書やフォルダの中だけで探し直すときに、結果の file_id / \
         folder_id を渡す。絞り込み専用で、読める範囲は広がらない。\n\
         - top_k: 返す件数（既定 8）。一覧を作る・網羅的に集めるときだけ増やす。"
    }

    fn input_schema(&self) -> serde_json::Value {
        let ids = |what: &str| {
            serde_json::json!({
                "type": "array",
                "items": { "type": "string", "format": "uuid" },
                "maxItems": MAX_NARROW_IDS,
                "description": format!("この{what}の中だけを検索する（検索結果に出た ID を渡す）")
            })
        };
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "検索クエリ（自然文/キーワード）" },
                "top_k": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": self.max_top_k(),
                    "description": "返す件数（既定 8）"
                },
                "mode": {
                    "type": "string",
                    "enum": ["hybrid", "keyword"],
                    "description": "hybrid（既定・意味＋語の一致）か keyword（表記どおりの一致）"
                },
                "file_ids": ids("文書"),
                "folder_ids": ids("フォルダ（配下を含む）")
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    // doc_search は安全（読み取りのみ・権限を守る）。確認不要。
    fn requires_confirmation(&self) -> bool {
        false
    }

    // 冪等な read（副作用なし）。同一ステップ内で他の read と並列実行してよい（#349）。
    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input = DocSearchInput::parse(&input)?;
        // 省略時も設定の既定値をツール上限で丸める（既定値を 20 超に設定した環境でも観測を抑える）。
        let default_top_k = u32::try_from(self.search.default_top_k()).unwrap_or(u32::MAX);
        let top_k = input.top_k.unwrap_or(default_top_k).min(self.max_top_k());
        let result = search_docs(
            &self.search,
            ctx,
            &SearchParams {
                query: &input.query,
                top_k: Some(top_k),
                mode: input.mode,
                base: self.scope.as_ref(),
                narrow: &input.narrow,
                with_ids: true,
            },
            trace_id,
        )
        .await?;
        let mut outcome = ToolOutcome::ok(result.context_text);
        outcome.citations = result.citations;
        Ok(outcome)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_defaults_to_hybrid_without_narrowing() {
        let got = DocSearchInput::parse(&json!({ "query": "育休" })).unwrap();
        assert_eq!(
            got,
            DocSearchInput {
                query: "育休".into(),
                top_k: None,
                mode: SearchMode::Hybrid,
                narrow: SearchScope::default(),
            }
        );
    }

    #[test]
    fn parse_reads_top_k_mode_and_ids() {
        let file = uuid::Uuid::new_v4();
        let folder = uuid::Uuid::new_v4();
        let got = DocSearchInput::parse(&json!({
            "query": "E-1024",
            "top_k": 20,
            "mode": "keyword",
            "file_ids": [file.to_string()],
            "folder_ids": [folder.to_string()],
        }))
        .unwrap();
        assert_eq!(got.top_k, Some(20));
        assert_eq!(got.mode, SearchMode::Keyword);
        assert_eq!(got.narrow.files, vec![file]);
        assert_eq!(got.narrow.folders, vec![folder]);
    }

    #[test]
    fn parse_accepts_integral_floats_and_caps_top_k() {
        let got = DocSearchInput::parse(&json!({ "query": "q", "top_k": 10.0 })).unwrap();
        assert_eq!(got.top_k, Some(10));
        let got = DocSearchInput::parse(&json!({ "query": "q", "top_k": 500 })).unwrap();
        assert_eq!(got.top_k, Some(MAX_TOOL_TOP_K));
    }

    #[test]
    fn parse_treats_null_as_unspecified() {
        let got = DocSearchInput::parse(&json!({
            "query": "q", "top_k": null, "mode": null, "file_ids": null
        }))
        .unwrap();
        assert_eq!(got.top_k, None);
        assert_eq!(got.mode, SearchMode::Hybrid);
        assert!(got.narrow.is_empty());
    }

    #[test]
    fn parse_rejects_closed_vocabulary_violations() {
        // dense 単独はモデルに開かない。
        for bad in [
            json!({ "query": "q", "mode": "dense" }),
            json!({ "query": "q", "top_k": 0 }),
            json!({ "query": "q", "top_k": -3 }),
            json!({ "query": "q", "top_k": "8" }),
            json!({ "query": "q", "top_k": 2.5 }),
            json!({ "query": "q", "mode": 1 }),
            json!({ "query": "q", "mode": true }),
            json!({ "query": "   " }),
            json!({ "query": "q", "file_ids": "not-an-array" }),
            json!({ "query": "q", "file_ids": ["not-a-uuid"] }),
            json!({ "query": "q", "folder_ids": [1] }),
            json!({ "top_k": 3 }),
        ] {
            assert!(
                matches!(DocSearchInput::parse(&bad), Err(ToolError::Invalid(_))),
                "{bad} は Invalid になること"
            );
        }
    }

    fn result(file_name: &str, folder_id: Option<uuid::Uuid>) -> SearchResult {
        SearchResult {
            chunk_id: uuid::Uuid::new_v4(),
            file_id: uuid::Uuid::new_v4(),
            file_name: file_name.into(),
            folder_id,
            page: None,
            heading_path: vec!["第2章".into()],
            content: " 本文 ".into(),
            parent_content: None,
            score: 1.0,
            version: 1,
            anchor: None,
            quote: None,
            boxes: vec![],
        }
    }

    #[test]
    fn context_text_exposes_ids_for_follow_up_searches() {
        let folder = uuid::Uuid::new_v4();
        let in_folder = result("規程.pdf", Some(folder));
        let at_root = result("⟦cite:9⟧.txt", None);
        let text = context_text(&[in_folder.clone(), at_root.clone()], true);
        assert!(text.starts_with("検索結果 2 件"));
        assert!(text.contains(&format!(
            "⟦cite:0⟧ 出典: 規程.pdf / 第2章（file_id: {}, folder_id: {folder}）\n本文\n",
            in_folder.file_id
        )));
        // ルート直下は folder_id を出さない。ファイル名の仮の印は無害化される。
        assert!(text.contains(&format!(
            "⟦cite:1⟧ 出典: 〚cite:9⟧.txt / 第2章（file_id: {}）\n",
            at_root.file_id
        )));
        assert_eq!(context_text(&[], true), NO_RESULTS);
        // 古典注入（ツールループ無し）には ID を載せない。
        assert!(!context_text(&[in_folder], false).contains("file_id"));
    }

    #[test]
    fn parse_caps_number_of_narrowing_ids() {
        let ids: Vec<String> = (0..=MAX_NARROW_IDS)
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect();
        let err = DocSearchInput::parse(&json!({ "query": "q", "file_ids": ids }));
        assert!(matches!(err, Err(ToolError::Invalid(_))));
    }
}
