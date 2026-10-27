//! コンテキストの使用量（チャット画面のコンテキスト表示）。
//!
//! 1 回の LLM リクエストに載った prompt を、**何がどれだけ占めたか**のカテゴリに分けて返す。
//! 総量はプロバイダが報告した prompt トークン（実測）を使う。プロバイダは内訳を返さないので、
//! カテゴリごとの比は手元の見積り（[`crate::context::estimate_prompt_tokens`] と同じ近似）で
//! 出し、実測の総量をその比で按分する。報告が無い（0）プロバイダでは見積りをそのまま使い、
//! `measured = false` で区別する。
//!
//! 窓（`window_tokens`）が分からないモデルでは割合を出せない。UI はトークン数だけを出す。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use llm_gateway::{Block, Message, ToolDef};

use crate::context::text_tokens;
use crate::ToolName;

/// prompt の内訳のカテゴリ（表示順）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextCategory {
    /// system プロンプト（スキルの指示・開いている文書・古典 RAG の検索結果を含む）。
    System,
    /// 文脈に載ったツール定義（遅延ロードで未読込のものは含まない）。
    Tools,
    /// 会話（発話・回答・思考・ツール呼び出しの引数）。
    Messages,
    /// 資料（文書検索・ファイル/文書の読み取り・Web 取得の結果）。
    Documents,
    /// そのほかのツール結果。
    ToolResults,
}

/// 1 カテゴリの使用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ContextCategoryUsage {
    pub category: ContextCategory,
    pub tokens: u64,
}

/// 1 回のリクエストのコンテキスト使用量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ContextUsage {
    /// 生成したモデル（論理名）。
    pub model: String,
    /// モデルのコンテキスト窓（未設定なら `None`＝割合は出せない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_tokens: Option<u32>,
    /// 応答（出力）のために空けておく上限（`max_tokens`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_output_tokens: Option<u32>,
    /// prompt の総量（`categories` の合計と一致する）。
    pub used_tokens: u64,
    /// `used_tokens` がプロバイダの実測か（`false` は手元の見積り）。
    pub measured: bool,
    /// カテゴリ別の内訳（[`ContextCategory`] の順・0 のカテゴリも含む）。
    pub categories: Vec<ContextCategoryUsage>,
}

const ORDER: [ContextCategory; 5] = [
    ContextCategory::System,
    ContextCategory::Tools,
    ContextCategory::Messages,
    ContextCategory::Documents,
    ContextCategory::ToolResults,
];

/// 送ったリクエストの中身と実測の prompt トークンからカテゴリ別のトークン数を出す
/// （[`estimate`] → [`fit`]）。
#[must_use]
pub fn measure<'a>(
    system: Option<&str>,
    tools: impl IntoIterator<Item = &'a ToolDef>,
    messages: &[Message],
    reported_prompt_tokens: u64,
) -> ([u64; 5], bool) {
    fit(estimate(system, tools, messages), reported_prompt_tokens)
}

/// カテゴリ別の見積り（[`ContextCategory`] の順）。
///
/// `tools` には**文脈に載った定義**だけを渡す（[`llm_gateway::tool_loading::context_tools`]）。
#[must_use]
pub fn estimate<'a>(
    system: Option<&str>,
    tools: impl IntoIterator<Item = &'a ToolDef>,
    messages: &[Message],
) -> [u64; 5] {
    let mut est = [0u64; 5];
    est[0] = system.map_or(0, text_tokens) as u64;
    est[1] = tools
        .into_iter()
        .map(|t| {
            text_tokens(&t.name)
                + text_tokens(&t.description)
                + text_tokens(&t.input_schema.to_string())
        })
        .sum::<usize>() as u64;
    // ツール結果を呼び出し元のツール名で振り分ける（id → name）。
    let mut names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for b in messages.iter().flat_map(|m| m.content.iter()) {
        match b {
            Block::Text { text } | Block::Thinking { text } => est[2] += text_tokens(text) as u64,
            Block::ToolUse { id, name, input } => {
                names.insert(id, name);
                est[2] += (text_tokens(name) + text_tokens(&input.to_string())) as u64;
            }
            Block::Reasoning { item, .. } => est[2] += text_tokens(&item.to_string()) as u64,
            Block::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                let slot = match names.get(tool_use_id.as_str()) {
                    Some(name) if reads_material(name) => 3,
                    _ => 4,
                };
                est[slot] += text_tokens(content) as u64;
            }
        }
    }
    est
}

/// 見積りを実測の総量に合わせる。報告が無い（0）なら見積りのまま（戻り値の bool＝実測か）。
#[must_use]
pub fn fit(est: [u64; 5], reported_prompt_tokens: u64) -> ([u64; 5], bool) {
    if reported_prompt_tokens == 0 {
        return (est, false);
    }
    (scale(est, reported_prompt_tokens), true)
}

impl ContextUsage {
    /// 1 回のリクエスト（送った system・ツール定義・履歴）と実測の prompt トークンから組み立てる。
    ///
    /// `model` は実効モデル（`None` はゲートウェイの既定）。窓はそのモデルのカタログから引く。
    #[must_use]
    pub fn of_request<'a>(
        gateway: &llm_gateway::LlmGateway,
        model: Option<&str>,
        max_tokens: Option<u32>,
        system: Option<&str>,
        tools: impl IntoIterator<Item = &'a ToolDef>,
        messages: &[Message],
        reported_prompt_tokens: u64,
    ) -> Self {
        Self::new(
            model.unwrap_or_else(|| gateway.default_model()).to_string(),
            gateway.context_window_tokens(model),
            max_tokens,
            measure(system, tools, messages, reported_prompt_tokens),
        )
    }

    /// [`measure`] の結果から組み立てる。
    #[must_use]
    pub fn new(
        model: String,
        window_tokens: Option<u32>,
        reserved_output_tokens: Option<u32>,
        (tokens, measured): ([u64; 5], bool),
    ) -> Self {
        ContextUsage {
            model,
            window_tokens,
            reserved_output_tokens,
            used_tokens: tokens.iter().sum(),
            measured,
            categories: ORDER
                .iter()
                .zip(tokens)
                .map(|(&category, tokens)| ContextCategoryUsage { category, tokens })
                .collect(),
        }
    }
}

/// 見積りの比で `total` を按分する（合計は `total` に一致させる）。
///
/// 見積りが全て 0 なら全量を会話に寄せる（内訳は出せないが総量は実測どおりに見せる）。
fn scale(est: [u64; 5], total: u64) -> [u64; 5] {
    let sum: u64 = est.iter().sum();
    if sum == 0 {
        let mut out = [0; 5];
        out[2] = total;
        return out;
    }
    let mut out = [0u64; 5];
    for (o, e) in out.iter_mut().zip(est) {
        *o = u64::try_from(u128::from(e) * u128::from(total) / u128::from(sum)).unwrap_or(0);
    }
    // 切り捨ての端数は最大のカテゴリへ寄せる。
    let rest = total - out.iter().sum::<u64>();
    if let Some((i, _)) = est.iter().enumerate().max_by_key(|(_, e)| **e) {
        out[i] += rest;
    }
    out
}

/// 資料（文書・ファイル・Web）を読み出すツールか。
///
/// 語彙に無い名前（chat 側のツール・メタツール）は資料に数えない。**網羅 match** なので、
/// 新ツールは追加時にここで区分を決める。
fn reads_material(name: &str) -> bool {
    let Some(tool) = ToolName::parse(name) else {
        return false;
    };
    match tool {
        ToolName::DocSearch
        | ToolName::DocRead
        | ToolName::WebSearch
        | ToolName::WebFetch
        | ToolName::FsList
        | ToolName::FsRead
        | ToolName::Grep
        | ToolName::ReadWorkflow
        | ToolName::SearchWorkflows
        | ToolName::DocumentRead
        | ToolName::SlideRead
        | ToolName::OfficeRead
        | ToolName::CsvQuery
        | ToolName::DataQuery
        | ToolName::DriveFind => true,
        ToolName::Skill
        | ToolName::SkillSearch
        | ToolName::QuoteCheck
        | ToolName::CodeInterpreter
        | ToolName::FsWrite
        | ToolName::FsAppend
        | ToolName::FsEdit
        | ToolName::FsDelete
        | ToolName::Shell
        | ToolName::Subagent
        // 書き方の説明で、回答の根拠になる資料ではない。
        | ToolName::UiReference
        | ToolName::EmitUi
        | ToolName::EmitWorkflow
        | ToolName::DocumentEdit
        | ToolName::DocumentEmbed
        | ToolName::SaveNote
        | ToolName::SaveSlide
        | ToolName::SaveCsv
        | ToolName::SaveSkill
        | ToolName::SaveDocument
        | ToolName::SaveSheet
        | ToolName::SlideEdit
        | ToolName::OfficeEdit
        | ToolName::OfficeLiveEdit
        | ToolName::OfficeExcelEdit
        | ToolName::OfficeWordEdit
        | ToolName::CsvPatch
        | ToolName::CsvWrite
        | ToolName::DataSchema
        | ToolName::DataRecordWrite
        | ToolName::DriveMove => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_gateway::Role;

    fn tool_def(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: "d".repeat(40),
            input_schema: serde_json::json!({"type": "object"}),
            defer_loading: false,
        }
    }

    fn call_and_result(id: &str, name: &str, content: &str) -> [Message; 2] {
        [
            Message {
                role: Role::Assistant,
                content: vec![Block::ToolUse {
                    id: id.into(),
                    name: name.into(),
                    input: serde_json::json!({}),
                }],
            },
            Message {
                role: Role::Tool,
                content: vec![Block::tool_result(id, content, false)],
            },
        ]
    }

    #[test]
    fn splits_tool_results_by_tool() {
        let mut msgs = vec![Message::text(Role::User, "あいうえお")];
        msgs.extend(call_and_result("a", "doc_search", &"x".repeat(400)));
        msgs.extend(call_and_result("b", "code_interpreter", &"y".repeat(800)));
        let (t, measured) = measure(Some("sys!"), [&tool_def("t")], &msgs, 0);
        assert!(!measured);
        assert_eq!(t[0], 1);
        assert!(t[1] > 0);
        assert_eq!(t[3], 100);
        assert_eq!(t[4], 200);
        // 発話 5 字 + 呼び出し 2 件（名前と `{}`）。
        assert_eq!(t[2], 5 + 2 + 4);
    }

    #[test]
    fn unknown_tool_results_are_not_material() {
        let msgs = call_and_result("a", "tool_search", &"z".repeat(40));
        let (t, _) = measure(None, [], &msgs, 0);
        assert_eq!(t[3], 0);
        assert_eq!(t[4], 10);
    }

    #[test]
    fn scales_to_reported_total() {
        let msgs = vec![Message::text(Role::User, "a".repeat(400))];
        let (t, measured) = measure(Some(&"s".repeat(400)), [], &msgs, 1001);
        assert!(measured);
        assert_eq!(t.iter().sum::<u64>(), 1001);
        assert!(t[0].abs_diff(t[2]) <= 1);
    }

    #[test]
    fn empty_estimate_puts_total_in_messages() {
        let (t, _) = measure(None, [], &[], 50);
        assert_eq!(t, [0, 0, 50, 0, 0]);
    }

    #[test]
    fn usage_lists_every_category_in_order() {
        let u = ContextUsage::new("m".into(), Some(1000), Some(100), ([1, 2, 3, 4, 5], true));
        assert_eq!(u.used_tokens, 15);
        let cats: Vec<_> = u.categories.iter().map(|c| c.category).collect();
        assert_eq!(cats, ORDER);
        let json = serde_json::to_value(&u).unwrap();
        assert_eq!(json["categories"][4]["category"], "tool_results");
    }
}
