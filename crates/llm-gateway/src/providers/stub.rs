//! 決定的スタブプロバイダ（テスト/CI・外部依存なし）。
//!
//! 実 LLM を持たない環境（CI・オフライン）でパイプライン全体（chat run・SSE・会計・
//! agent-core ループ）をエンドツーエンドで検証するための決定的アダプタ。挙動:
//! - 直近 user メッセージ本文を語単位でストリーミングして返す。
//! - `slow:<秒> <本文>` は本文を出したあと指定秒だけ生成を続ける（生成中の UI＝順番待ち・停止の検証用）。
//! - `long:<段落数>` は番号付き段落の長文を数文字ずつ流す（生成中に読み返すスクロールの検証用）。
//!   `longthink:<段落数>` は同じ数の段落の思考を流してから長文を流す（思考パネルを開いたままの検証用）。
//! - リクエストにツールがあり、かつ最初のターン（tool_result がまだ無い）で本文が
//!   既知のプレフィックス（`search:` / `python:` / `websearch:` / `webfetch:`）で始まるとき、
//!   対応するツールを 1 回だけ呼び出す（agent ループ・各ツールの決定的検証）。プレフィックス
//!   に対応するツールが提示されていなければ最初のツールへフォールバックする（後方互換）。
//! - 自律プロファイル（Phase 5）検証用の駆動プレフィックス:
//!   - `plan: A, B, C` … 1 ターン目に `plan` メタツールをカンマ区切りのサブタスクで呼ぶ（計画分解の検証）。
//!   - `loop:` … tool_result の有無に関わらず**毎ターン** tools[0] を空入力で呼び続ける
//!     （ループ検出・ステップ/予算上限・長ホライズンの決定的駆動）。`loop:<ツール名>` は
//!     tools[0] の代わりにその名前のツールを呼ぶ（提示されていなければ tools[0]）。
//! - tool search 検証用の `toolsearch:<query>` … `tool_search` で検索し、読み込んだ先頭の
//!   遅延ツールを次のターンで呼ぶ（[`super::stub_triggers::tool_search_call`]）。
//! - 既存のワークフローを直す `fixwf:<語>` … `list_workflows` で探し、先頭を `read_workflow` で読み、説明を書き換えて
//!   `emit_workflow` で新しい版として保存する（[`super::stub_triggers::fix_workflow_call`]・GAP-31）。
//! - `echosystem:` … 受け取った system プロンプトをそのまま本文で返す（system の組み立て
//!   ＝今日の日付などが実際にモデルへ届くことの検証用・#546）。
//! - `echoeffort:` … 受け取った思考の深さ（無ければ `none`）を `effort: <値>` で返す（ノードの思考の深さの解決が
//!   実際にモデルへ届く値になることの検証用・D-75）。
//! - `echohistory:` … 受け取ったメッセージの本文とツールの結果を改行でつないで返す（次の回答の文脈に何が渡るかの
//!   検証用・[`super::stub_triggers::echo_history_call`]・PIT-84）。
//! - `echotool:<ツール名> <入力 JSON>` … 1 ターン目にそのツールを呼び、結果が返ったらその本文をそのまま回答の本文で
//!   返す（観測が回答・共有の相手の画面・次の回答の文脈のどこまで届くかの検証用・
//!   [`super::stub_triggers::echo_tool_call`]・PIT-84）。
//! - `<conversation>…</conversation>` … 包みの中の最初の行を返す（会話の自動タイトル用。
//!   「回答: 」を付けない）。
//! - 構造化出力（形の指定があるとき）… スキーマに合う最小の値・`json:` / `jsonalways:` /
//!   `refuse:` / `truncate:`（[`super::stub_structured`]・語はどの行の頭に置いてもよい）。
//! - `fail:`（形の指定が無いとき・どの行の頭に置いてもよい）… 送り直されない誤り（`LlmError::BadRequest`）で失敗する
//!   （ワークフローの llm.invoke の、再試行しない失敗の出口の検証用・[`super::stub_structured::failure_call`]）。
//! - `truncate:<本文>`（形の指定が無いとき）… 「回答: <本文>」を流して出力の上限（`MaxTokens`）で終える
//!   （チャットの「途中で打ち切りました」の注記の検証用・GAP-106）。
//! - `truncatecall:`（ツールがあるとき）… 1 ターン目に tools[0] の呼び出しを引数の途中で切って `MaxTokens` で
//!   終え（`ToolUseStart` だけを流す・BUG-225 の後のアダプタと同じ形）、ツールの結果が返ったら通常の応答に戻る
//!   （切れた呼び出しの回復の検証用・#1196）。`truncatecallmix:`（ツールが 2 つ以上）は、その前に tools[1] の
//!   引数まで届いた呼び出しを 1 つ混ぜる。
//!
//! 決定的なのは応答の中身だけで、ツール呼び出しの id は実プロバイダと同じく呼び出しごとに一意
//! （[`super::stub_stream::stub_tool_call_id`]・承認の鍵が前の決定に当たらないように）。

use std::time::Duration;

use futures::stream::{self, StreamExt};

use super::stub_deep_research::deep_research_call;
use super::stub_fixtures::{long_reply, long_thinking, yui_reply};
use super::stub_stream::{
    paced_chunks_stream, paced_thinking_then_text_stream, slow_text_stream, stub_tool_call_id,
    text_stream, text_stream_ending, thinking_only_stream, tool_call_stream, tool_calls_stream,
    truncated_text_stream, truncated_tool_call_stream,
};
use super::stub_structured::{failure_call, structured_call};
use super::stub_triggers::{
    conversation_title_call, echo_effort_call, echo_history_call, echo_system_call, echo_tool_call,
    fix_workflow_call, generic_tool_call, note_suggest_call, note_tool_call, tool_search_call,
};
use super::timeouts::RequestClock;
use crate::config::LlmTimeouts;
use crate::model::{Block, GenerateRequest, Role, StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError, LlmProvider};

/// 決定的スタブ。
#[derive(Debug, Default, Clone)]
pub struct StubProvider {
    /// `slow:` の待ちにも実プロバイダと同じ上限を掛ける（#554）。
    timeouts: LlmTimeouts,
}

impl StubProvider {
    pub fn new() -> Self {
        StubProvider::default()
    }

    /// タイムアウト（最初の応答・無通信・全体）を指定する。既定は [`LlmTimeouts::default`]。
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: LlmTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }
}

/// 直近の user メッセージ本文を取り出す。
fn last_user_text(req: &GenerateRequest) -> String {
    req.messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| {
            m.content
                .iter()
                .filter_map(|b| match b {
                    // 前のターンの作業記録（chat の履歴が足す）は利用者の発話ではない。
                    Block::Text { text } if !text.starts_with(crate::WORK_LOG_MARK) => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// 最初の user 発話の本文（続きの催促の後でも、最初に送った駆動の語を見るため）。
fn first_user_text(req: &GenerateRequest) -> Option<&str> {
    req.messages
        .iter()
        .find(|m| m.role == Role::User)
        .and_then(|m| {
            m.content.iter().find_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
        })
}

/// `truncatetext:` の駆動（本文の途中で出力上限に達した応答の続きを書かせる回復）。
fn truncated_text_call(
    req: &GenerateRequest,
    user_text: &str,
    prompt_tokens: u64,
) -> Option<DeltaStream> {
    let rest = first_user_text(req)?.strip_prefix("truncatetext:")?;
    if !user_text.starts_with(crate::CONTINUATION_MARK) {
        return Some(truncated_text_stream("報告の前半です。表の", prompt_tokens));
    }
    if rest.trim() == "forever" {
        return Some(truncated_text_stream("まだ途中です。", prompt_tokens));
    }
    // 実プロバイダと同じく、続きの生成も思考から始まることがある（本文の連結を崩さないこと）。
    Some(paced_thinking_then_text_stream(
        "続きを考える",
        "続きの行です。回答を終えます。",
        prompt_tokens,
        64,
        Duration::ZERO,
    ))
}

/// `thinkonly:` の駆動（ツール結果を受けた後の応答が思考だけで出力上限に達する回復）。
///
/// 1 手目は tools[0] を呼ぶ。結果を受けた応答は思考だけで `MaxTokens` 停止し、催促
/// （[`crate::THINKING_EXHAUSTED_MARK`]）を受けたら本文を返す。本文には受け取った effort を
/// 書く（催促後の生成で思考を絞っていることをテストが確かめる）。`thinkonly:forever` は
/// 催促にも思考だけで止まり続ける。
fn thinking_exhausted_call(req: &GenerateRequest, prompt_tokens: u64) -> Option<DeltaStream> {
    let rest = first_user_text(req)?.strip_prefix("thinkonly:")?;
    if !has_tool_result(req) {
        let tool = req.tools.first()?;
        return Some(tool_call_stream(
            tool.name.clone(),
            serde_json::json!({}),
            prompt_tokens,
        ));
    }
    let nudged = req.messages.last().is_some_and(|m| {
        m.content.iter().any(|b| match b {
            Block::ToolResult { content: t, .. } | Block::Text { text: t } => {
                t.contains(crate::THINKING_EXHAUSTED_MARK)
            }
            _ => false,
        })
    });
    if nudged && rest.trim() != "forever" {
        let effort = req.effort.map_or("none", |e| e.as_str());
        return Some(text_stream(
            &format!("集計を終えました。回答です（effort={effort}）。"),
            prompt_tokens,
        ));
    }
    Some(thinking_only_stream(
        "どの列を数えるかを考え続ける",
        prompt_tokens,
    ))
}

/// 履歴の語数を prompt トークンとみなす。**ツール結果も数える**（実プロバイダと同じく観測は
/// prompt に載る）。本文だけにすると、ツールを回し続けるループで prompt が伸びず、
/// `Spent::fresh_tokens`（#404 で上限判定に使う軸）が永久に増えないため、トークン予算が
/// 「ステップ上限で先に止まる」ことでしか終われなくなる。
fn prompt_tokens(req: &GenerateRequest) -> u64 {
    req.messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            Block::Text { text } | Block::ToolResult { content: text, .. } => {
                Some(text.split_whitespace().count() as u64)
            }
            _ => None,
        })
        .sum()
}

/// これまでにツール結果があるか（＝2 ターン目以降）。
fn has_tool_result(req: &GenerateRequest) -> bool {
    req.messages.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, Block::ToolResult { .. }))
    })
}

/// `reasoning:` で始まり、ツールが提示されていれば [`reasoning_round_trip`] で返す。
fn reasoning_call(
    req: &GenerateRequest,
    user_text: &str,
    prompt_tokens: u64,
) -> Option<DeltaStream> {
    (!req.tools.is_empty() && user_text.starts_with("reasoning:"))
        .then(|| reasoning_round_trip(req, prompt_tokens))
}

/// `reasoning:` の決定的駆動（[`Block::Reasoning`] の往復を agent-core の結合テストで確かめる）。
fn reasoning_round_trip(req: &GenerateRequest, prompt_tokens: u64) -> DeltaStream {
    if has_tool_result(req) {
        // 1 ターン目の assistant メッセージが、届いた順（本文・推論アイテム・本文・ツール呼び出し）の
        // まま戻っているか。本文を 1 か所にまとめると推論アイテムの後ろの本文が前へ動く。
        let carried = req.messages.iter().any(|m| {
            matches!(
                m.content.as_slice(),
                [
                    Block::Text { text: before },
                    Block::Reasoning { provider, item },
                    Block::Text { text: after },
                    Block::ToolUse { .. },
                ] if before == "考えます。"
                    && provider == "stub"
                    && item["n"] == 1
                    && after == "呼びます。"
            )
        });
        let reply = if carried {
            "推論を引き継ぎました"
        } else {
            "推論が戻っていません"
        };
        return text_stream(reply, prompt_tokens);
    }
    let call = tool_call_stream(
        req.tools[0].name.clone(),
        serde_json::json!({}),
        prompt_tokens,
    );
    let head = stream::iter([
        Ok(StreamDelta::TextDelta {
            text: "考えます。".to_string(),
        }),
        Ok(StreamDelta::ReasoningItem {
            provider: "stub".to_string(),
            item: serde_json::json!({ "n": 1 }),
        }),
        Ok(StreamDelta::TextDelta {
            text: "呼びます。".to_string(),
        }),
    ]);
    head.chain(call).boxed()
}

/// プレフィックス → (対応ツール名, 入力 JSON のキー)。入力値は本文の残り。
/// 対応ツールが提示に無ければ `None` を返し、呼び出し側が tools[0] にフォールバックする。
fn tool_trigger(user_text: &str) -> Option<(&'static str, &'static str, String)> {
    const MAP: &[(&str, &str, &str)] = &[
        ("search:", "doc_search", "query"),
        ("websearch:", "web_search", "query"),
        ("python:", "code_interpreter", "code"),
        ("webfetch:", "web_fetch", "url"),
    ];
    MAP.iter().find_map(|(prefix, tool, key)| {
        user_text
            .strip_prefix(prefix)
            .map(|rest| (*tool, *key, rest.trim().to_string()))
    })
}

/// `emitwf:` 駆動の固定ワークフロー IR（Task 10.13 の決定的検証）。
/// `emitwf:ok <name>` は保存パイプライン（V1〜V7）を通る IR、`emitwf:bad <name>` は
/// 語彙外ノード type を含む不正 IR（検証エラー全件のフォールバック検証用）。
fn emitwf_ir(kind: &str) -> serde_json::Value {
    let (variant, name) = kind.split_once(' ').unwrap_or((kind, "stub-flow"));
    let node_type = if variant == "bad" {
        "bogus.node"
    } else {
        "script.run"
    };
    serde_json::json!({
        "ir_version": 1,
        "name": name.trim(),
        "display_name": "スタブフロー",
        "declared_scopes": [],
        "triggers": [{ "kind": "interactive" }],
        "nodes": [{
            "id": "compute",
            "type": node_type,
            "params": { "source": { "inline": "function main(i){ return { ok: true }; }" } }
        }],
        "edges": []
    })
}

/// `subagent:<テーマ>` — 1 ステップで `subagent` を**3 体**呼ぶ（境界を重複なく割った形）。
///
/// 委譲の並列（#391）と UI の「並行して N 件」表示を決定的に再現する唯一の入口。提示ツールに
/// `subagent` が無ければ `None`（呼び出し側が通常経路へ落ちる）。
fn subagent_call(
    req: &GenerateRequest,
    user_text: &str,
    prompt_tokens: u64,
) -> Option<DeltaStream> {
    let topic = user_text.strip_prefix("subagent:")?.trim();
    let tool = req.tools.iter().find(|t| t.name == "subagent")?;
    let calls = [
        ("国内・直近 3 年の実数", "国内"),
        ("同期間の海外比較", "北米・欧州"),
        ("規制・政策の動き", "法改正のみ"),
    ]
    .into_iter()
    .map(|(objective, boundary)| {
        (
            tool.name.clone(),
            serde_json::json!({
                "objective": format!("{topic}: {objective}"),
                "boundary": boundary,
            }),
        )
    })
    .collect();
    Some(tool_calls_stream(calls, prompt_tokens))
}

/// `parallel:<クエリ>` — 1 ステップで `web_search` ＋ **別ホスト**の `web_fetch` ×3 を呼ぶ。
///
/// agent-core の有界並列（#349・同一ホストは politeness_key で直列化される）と、
/// UI の「並行して N 件」表示・step グルーピング（#386）を決定的に再現する唯一の入口。
/// 提示ツールに web 系が無ければ `None`（呼び出し側が通常経路へ落ちる）。
fn parallel_read_call(
    req: &GenerateRequest,
    user_text: &str,
    prompt_tokens: u64,
) -> Option<DeltaStream> {
    let query = user_text.strip_prefix("parallel:")?.trim();
    let mut calls: Vec<(String, serde_json::Value)> = Vec::new();
    if let Some(t) = req.tools.iter().find(|t| t.name == "web_search") {
        calls.push((t.name.clone(), serde_json::json!({ "query": query })));
    }
    if let Some(t) = req.tools.iter().find(|t| t.name == "web_fetch") {
        // ホストを分けないと同一レーンへ入り、並列にならない。
        for url in [
            "https://example.com/stub-1",
            "https://example.org/stub-2",
            "https://example.net/stub-3",
        ] {
            calls.push((t.name.clone(), serde_json::json!({ "url": url })));
        }
    }
    (!calls.is_empty()).then(|| tool_calls_stream(calls, prompt_tokens))
}

/// 本文を決め打ちで返す駆動（図・推論の引き継ぎ・受け取った system と思考の深さの返却）をまとめて試す。
fn fixed_reply_call(
    req: &GenerateRequest,
    user_text: &str,
    prompt_tokens: u64,
) -> Option<DeltaStream> {
    yui_call(user_text, prompt_tokens)
        .or_else(|| long_call(user_text, prompt_tokens))
        .or_else(|| reasoning_call(req, user_text, prompt_tokens))
        .or_else(|| echo_system_call(req, user_text, prompt_tokens))
        .or_else(|| echo_effort_call(req, user_text, prompt_tokens))
        .or_else(|| echo_history_call(req, user_text, prompt_tokens))
        .or_else(|| conversation_title_call(user_text, prompt_tokens))
}

/// 図の回答 `yui:<kind>`（#530）: ```ui ブロックを含む本文を数文字ずつ流す
/// （行の途中で区切れる＝書きかけの値を描かないことを e2e で確かめられる）。
fn yui_call(user_text: &str, prompt_tokens: u64) -> Option<DeltaStream> {
    let kind = user_text.strip_prefix("yui:")?;
    Some(paced_chunks_stream(
        &yui_reply(kind.trim()),
        prompt_tokens,
        3,
        Duration::from_millis(25),
    ))
}

/// `long:` / `longthink:` の段落数の上限（本文を先に組み立てるので、大きな数でメモリを使い切らない）。
const LONG_MAX_PARAGRAPHS: usize = 1000;

/// 長文の回答 `long:<段落数>`: 番号付き段落を数文字ずつ流す（生成中に上へスクロールして
/// 読み返しても追従で引き戻されないことを e2e で確かめる）。`longthink:<段落数>` は同じ数の
/// 段落の思考を先に流す（思考パネルを開いたまま読み返す検証用）。段落数の省略・不正は 40、上限は
/// [`LONG_MAX_PARAGRAPHS`]。
fn long_call(user_text: &str, prompt_tokens: u64) -> Option<DeltaStream> {
    let (rest, think) = match user_text.strip_prefix("longthink:") {
        Some(rest) => (rest, true),
        None => (user_text.strip_prefix("long:")?, false),
    };
    let n = rest.trim().parse().unwrap_or(40).min(LONG_MAX_PARAGRAPHS);
    let thinking = if think {
        long_thinking(n)
    } else {
        String::new()
    };
    Some(paced_thinking_then_text_stream(
        &thinking,
        &long_reply(n),
        prompt_tokens,
        6,
        Duration::from_millis(15),
    ))
}

#[async_trait::async_trait]
impl LlmProvider for StubProvider {
    fn name(&self) -> &'static str {
        "stub"
    }

    async fn stream(&self, req: &GenerateRequest) -> Result<DeltaStream, LlmError> {
        // 実プロバイダと同じ無通信・全体の上限を、どの応答（`slow:` の待ち・`yui:` の
        // 間隔付きの流し方など）にも掛ける（#554）。
        Ok(RequestClock::start(self.timeouts, "stub").guard(stub_stream(req)))
    }
}

/// 入力に応じた決定的な応答（タイムアウトは [`StubProvider::stream`] が掛ける）。
#[allow(clippy::too_many_lines)] // 既知プレフィックスごとの分岐で行数が伸びる。
fn stub_stream(req: &GenerateRequest) -> DeltaStream {
    let user_text = last_user_text(req);
    let prompt_tokens = prompt_tokens(req);

    // --- deep research（#387）: `/deep-research` 起動はフェーズを跨いで進むため最初に見る。 ---
    // 確認の段階（ask・plan）は門がツールを 1 つも渡さないので、ツールの有無で入口を絞らない
    // （調査の段階のツールの要否は deep_research_call の中で見る）。
    if let Some(s) = deep_research_call(req, prompt_tokens) {
        return s;
    }
    // --- 遅い生成 `slow:<秒> <本文>`: 生成中の挙動（順番待ち・停止）を決定的に試す。 ---
    if let Some(rest) = user_text.strip_prefix("slow:") {
        let (secs, body) = rest.split_once(' ').unwrap_or((rest, ""));
        let secs: u64 = secs.trim().parse().unwrap_or(3);
        return slow_text_stream(
            &format!("回答: {}", body.trim()),
            prompt_tokens,
            Duration::from_secs(secs),
        );
    }
    // --- 図 `yui:<kind>`（#530）・長文 `long:`・推論の引き継ぎ `reasoning:`（#542）・`echosystem:`（#546）。 ---
    if let Some(s) = fixed_reply_call(req, &user_text, prompt_tokens) {
        return s;
    }
    // --- 構造化出力（GAP-13）: 形の指定があればスキーマに合う値・`json:` 等の駆動で返す。 ---
    if let Some(s) = structured_call(req, &user_text, prompt_tokens) {
        return s;
    }
    // --- 形の指定が無い呼び出しの失敗 `fail:`（送り直されない誤り・ワークフローの失敗の出口）。 ---
    if let Some(s) = failure_call(&user_text) {
        return s;
    }
    // --- 出力の上限での打ち切り `truncate:<本文>`（形の指定が無いとき・GAP-106）。続きの催促（#1247）には何も
    //     書かずに上限で止まり、打ち切りのまま run を終える（回答の下の注記を確かめる駆動を保つ）。 ---
    if let Some(body) = user_text.strip_prefix("truncate:") {
        return text_stream_ending(
            &format!("回答: {}", body.trim()),
            StopReason::MaxTokens,
            prompt_tokens,
        );
    }
    if user_text.starts_with(crate::CONTINUATION_MARK)
        && first_user_text(req).is_some_and(|t| t.starts_with("truncate:"))
    {
        return text_stream_ending("", StopReason::MaxTokens, prompt_tokens);
    }
    // --- 自律駆動 `loop:`: 毎ターン tools[0] を空入力で呼び続ける（ループ/上限の決定的駆動）。
    //     `loop:<ツール名>`（空白を挟まない）はその名前のツールを呼ぶ（先頭が承認の要るツールの構成でも、
    //     承認待ちで止めずに手数を積む）。 ---
    if let Some(rest) = user_text.strip_prefix("loop:") {
        if let Some(first) = req.tools.first() {
            let named = rest.split(char::is_whitespace).next().unwrap_or("");
            let tool = req.tools.iter().find(|t| t.name == named).unwrap_or(first);
            return tool_call_stream(tool.name.clone(), serde_json::json!({}), prompt_tokens);
        }
    }
    // --- 出力上限で切れたツール呼び出し `truncatecall:`: 1 ターン目だけ tools[0] の引数の途中で
    //     `MaxTokens` 停止する（切れた呼び出しを黙って完了扱いにしない回復の決定的駆動・#1196）。
    //     `truncatecallmix:` は tools[1] の完全な呼び出しを前に混ぜる。
    //     本文の打ち切りの `truncate:`（GAP-106・上で先に拾う）とは別の語にする。 ---
    if !req.tools.is_empty() && !has_tool_result(req) {
        let whole = if user_text.starts_with("truncatecallmix:") {
            req.tools.get(1).map(|t| Some(t.name.clone()))
        } else if user_text.starts_with("truncatecall:") {
            Some(None)
        } else {
            None
        };
        if let Some(whole) = whole {
            return truncated_tool_call_stream(req.tools[0].name.clone(), whole, prompt_tokens);
        }
    }
    // --- 本文の途中で出力上限 `truncatetext:`: 最初の応答は本文の途中で `MaxTokens` 停止し、
    //     続きの催促には続きを返す（`truncatetext:forever` は催促にも切れ続ける）。 ---
    if let Some(s) = truncated_text_call(req, &user_text, prompt_tokens) {
        return s;
    }
    // --- 思考だけで出力上限 `thinkonly:`: ツール結果の後の応答が思考だけで `MaxTokens` 停止する。 ---
    if let Some(s) = thinking_exhausted_call(req, prompt_tokens) {
        return s;
    }
    // --- tool search 駆動 `toolsearch:`（検索→読み込み→呼び出しを 2 ターンに跨いで進む）。 ---
    if let Some(s) = tool_search_call(req, &user_text, prompt_tokens) {
        return s;
    }
    // --- 既存のワークフローを直す `fixwf:`（探す → 読む → 新しい版を保存する・GAP-31）。 ---
    if let Some(s) = fix_workflow_call(req, &user_text, prompt_tokens) {
        return s;
    }
    // --- 観測を写す `echotool:`（呼ぶ → 結果の本文をそのまま返す・PIT-84）。2 ターン目も見るので門の前に置く。 ---
    if let Some(s) = echo_tool_call(req, &user_text, prompt_tokens) {
        return s;
    }
    // --- 並行 read 駆動 `parallel:`（#386）。 ---
    if !has_tool_result(req) {
        if let Some(s) = parallel_read_call(req, &user_text, prompt_tokens) {
            return s;
        }
    }
    // --- 委譲の並列駆動 `subagent:`（#391）。 ---
    if !has_tool_result(req) {
        if let Some(s) = subagent_call(req, &user_text, prompt_tokens) {
            return s;
        }
    }
    // --- 自律駆動 `fswrite:`: 1 ターン目に fs_write を固定名で呼ぶ（ワークスペース書込の e2e）。 ---
    if !has_tool_result(req) {
        if let Some(rest) = user_text.strip_prefix("fswrite:") {
            if let Some(t) = req.tools.iter().find(|t| t.name == "fs_write") {
                return tool_call_stream(
                    t.name.clone(),
                    serde_json::json!({ "name": "agent-note.txt", "content": rest.trim() }),
                    prompt_tokens,
                );
            }
        }
    }
    // --- 自律駆動 `plan:`: 1 ターン目に plan メタツールをカンマ区切りサブタスクで呼ぶ。 ---
    if !has_tool_result(req) {
        if let Some(rest) = user_text.strip_prefix("plan:") {
            if let Some(plan_tool) = req.tools.iter().find(|t| t.name == "plan") {
                let subtasks: Vec<serde_json::Value> = rest
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|title| serde_json::json!({ "title": title }))
                    .collect();
                return tool_call_stream(
                    plan_tool.name.clone(),
                    serde_json::json!({ "subtasks": subtasks }),
                    prompt_tokens,
                );
            }
        }
    }

    // --- ノートツール駆動（issue #282 の e2e）: savenote:<name>→save_note / docembed:<id>→document.embed。 ---
    if !has_tool_result(req) {
        if let Some(s) = note_suggest_call(req, &user_text, prompt_tokens) {
            return s;
        }
        if let Some(s) = note_tool_call(req, &user_text, prompt_tokens) {
            return s;
        }
    }
    // helpsearch: はヘルプ UI の決定的検証専用（選択品質の評価ではない）。
    if let Some(query) = user_text.strip_prefix("helpsearch:") {
        if has_tool_result(req) {
            // 結果の 1 件目の「画面:」「図:」とページの URL（`[H1] <URL>`）を、実モデルへの指示と同じ形で使う。
            let result = req
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .find_map(|b| match b {
                    Block::ToolResult { content, .. } if content.contains("[H1] ") => {
                        Some(content.as_str())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            let first = |prefix: &str| {
                result
                    .lines()
                    .find_map(|l| l.strip_prefix(prefix))
                    .map(str::to_owned)
            };
            use std::fmt::Write as _;
            let mut text = "ヘルプの手順を確認してください。[H1]".to_owned();
            if let Some(screen) = first("画面: ") {
                let _ = write!(text, "\n\n操作する画面: {screen}");
            }
            if let Some(figure) = first("図: ") {
                let _ = write!(text, "\n\n{figure}");
            }
            if let Some(url) = first("[H1] ") {
                let _ = write!(
                    text,
                    "\n\n詳しくは [ヘルプのページ]({url}) を見てください。"
                );
            }
            return text_stream(&text, prompt_tokens);
        }
        if let Some(t) = req.tools.iter().find(|t| t.name == "help_search") {
            return tool_call_stream(
                t.name.clone(),
                serde_json::json!({"query":query.trim(),"top_k":1}),
                prompt_tokens,
            );
        }
    }

    // --- 汎用駆動 `calltool:<ツール名> <入力 JSON>`（Task 15.7）。 ---
    if !has_tool_result(req) && user_text.starts_with("calltool:") {
        if let Some(s) = generic_tool_call(req, &user_text, prompt_tokens) {
            return s;
        }
    }

    // --- AI ワークフロー編集駆動 `emitwf:`（Task 10.13 の決定的検証）。 ---
    if !has_tool_result(req) {
        if let Some(rest) = user_text.strip_prefix("emitwf:") {
            if let Some(t) = req.tools.iter().find(|t| t.name == "emit_workflow") {
                let ir = emitwf_ir(rest.trim());
                return tool_call_stream(
                    t.name.clone(),
                    serde_json::json!({ "ir": ir }),
                    prompt_tokens,
                );
            }
        }
    }

    // ツール呼び出し分岐（決定的）: 1 ターン目に既知プレフィックスで始まれば対応ツールを呼ぶ。
    if !req.tools.is_empty() && !has_tool_result(req) {
        if let Some((tool_name, key, value)) = tool_trigger(&user_text) {
            // 対応ツールが提示にあればそれを、無ければ tools[0] を呼ぶ（後方互換）。
            let name = req
                .tools
                .iter()
                .find(|t| t.name == tool_name)
                .map_or_else(|| req.tools[0].name.clone(), |t| t.name.clone());
            let id = stub_tool_call_id(1);
            let input = serde_json::json!({ key: value });
            let events = vec![
                Ok(StreamDelta::ToolUseStart {
                    id: id.clone(),
                    name,
                }),
                Ok(StreamDelta::ToolUseStop { id, input }),
                Ok(StreamDelta::Done {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage {
                        prompt_tokens,
                        completion_tokens: 0,
                        ..Default::default()
                    },
                }),
            ];
            return stream::iter(events).boxed();
        }
    }

    // 通常応答: 本文を語単位でストリーミング。
    let reply = if user_text.is_empty() {
        "（空の質問です）".to_string()
    } else {
        format!("回答: {user_text}")
    };
    text_stream(&reply, prompt_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Message;

    #[tokio::test]
    async fn stub_streams_reply_and_usage() {
        let req = GenerateRequest::new(vec![Message::text(Role::User, "hello world")]);
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut text = String::new();
        let mut done = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::TextDelta { text: t } => text.push_str(&t),
                StreamDelta::Done { usage, .. } => done = Some(usage),
                _ => {}
            }
        }
        assert!(text.contains("hello world"));
        let usage = done.unwrap();
        assert_eq!(usage.prompt_tokens, 2);
        assert!(usage.completion_tokens > 0);
    }

    /// deep research の確認の段階は、門がツールを 1 つも渡さない run でも ask を書く（#604）。
    #[tokio::test]
    async fn deep_research_asks_without_any_tools() {
        let req = GenerateRequest::new(vec![Message::text(Role::User, "/deep-research SaaS 市場")]);
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut text = String::new();
        while let Some(ev) = s.next().await {
            if let StreamDelta::TextDelta { text: t } = ev.unwrap() {
                text.push_str(&t);
            }
        }
        assert!(text.contains("```ui\nask "), "{text}");
    }

    /// 形の指定が無い `truncate:` は本文を返して出力の上限で終える（チャットの打ち切りの注記の検証用・GAP-106）。
    #[tokio::test]
    async fn stub_truncate_prefix_ends_at_max_tokens_without_schema() {
        let req = GenerateRequest::new(vec![Message::text(Role::User, "truncate:長い説明")]);
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut text = String::new();
        let mut stop = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::TextDelta { text: t } => text.push_str(&t),
                StreamDelta::Done { stop_reason, .. } => stop = Some(stop_reason),
                _ => {}
            }
        }
        assert_eq!(text, "回答: 長い説明");
        assert_eq!(stop, Some(StopReason::MaxTokens));
    }

    /// `truncate:` の後の続きの催促（#1247）には本文を書かずに出力の上限で止まる（打ち切りのまま run を終える）。
    #[tokio::test]
    async fn stub_truncate_stays_truncated_on_the_continuation() {
        let req = GenerateRequest::new(vec![
            Message::text(Role::User, "truncate:長い説明"),
            Message::text(Role::Assistant, "回答: 長い説明"),
            Message::text(
                Role::User,
                format!("{} 続きを書いてください。", crate::CONTINUATION_MARK),
            ),
        ]);
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut text = String::new();
        let mut stop = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::TextDelta { text: t } => text.push_str(&t),
                StreamDelta::Done { stop_reason, .. } => stop = Some(stop_reason),
                _ => {}
            }
        }
        assert_eq!(text, "");
        assert_eq!(stop, Some(StopReason::MaxTokens));
    }

    /// helpsearch: の 2 ターン目は、結果の 1 件目の画面・図・ページを実モデルへの指示と同じ形で使う。
    #[tokio::test]
    async fn helpsearch_answer_links_the_screen_figure_and_page() {
        let result = "見出し\n[H1] https://h.example/help/a/#x\n画面: [ログイン方法](/settings/security)\n図: ![画面](https://h.example/help/media/a.png)\n<document>本文</document>";
        let req = GenerateRequest::new(vec![
            Message::text(Role::User, "helpsearch:パスワード"),
            Message {
                role: Role::Tool,
                content: vec![Block::tool_result("t1", result, false)],
            },
        ]);
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut text = String::new();
        while let Some(ev) = s.next().await {
            if let StreamDelta::TextDelta { text: t } = ev.unwrap() {
                text.push_str(&t);
            }
        }
        assert!(text.contains("[H1]"), "{text}");
        assert!(
            text.contains("[ログイン方法](/settings/security)"),
            "{text}"
        );
        assert!(
            text.contains("![画面](https://h.example/help/media/a.png)"),
            "{text}"
        );
        assert!(text.contains("(https://h.example/help/a/#x)"), "{text}");
    }

    #[tokio::test]
    async fn stub_streams_ui_block_in_small_chunks() {
        let req = GenerateRequest::new(vec![Message::text(Role::User, "yui:flow")]);
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut chunks = Vec::new();
        while let Some(ev) = s.next().await {
            if let StreamDelta::TextDelta { text } = ev.unwrap() {
                chunks.push(text);
            }
        }
        let text = chunks.concat();
        assert!(text.contains("```ui\nflow 経費精算\n"));
        // 数文字ずつ、行の途中でも区切れて届く（書きかけの行を試せる）。
        assert!(chunks.iter().all(|c| c.chars().count() <= 3));
        assert!(chunks.iter().any(|c| !c.ends_with('\n')));
    }

    #[tokio::test]
    async fn stub_streams_long_reply_paragraph_by_paragraph() {
        let req = GenerateRequest::new(vec![Message::text(Role::User, "long:2")]);
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut chunks = Vec::new();
        while let Some(ev) = s.next().await {
            if let StreamDelta::TextDelta { text } = ev.unwrap() {
                chunks.push(text);
            }
        }
        let text = chunks.concat();
        assert!(text.starts_with("長い回答を書きます。"), "{text}");
        assert!(text.contains("## 第1章") && text.contains("段落1。") && text.contains("段落2。"));
        assert!(!text.contains("段落3。"), "{text}");
        assert!(chunks.iter().all(|c| c.chars().count() <= 6));
    }

    #[tokio::test]
    async fn stub_streams_long_thinking_before_reply() {
        let req = GenerateRequest::new(vec![Message::text(Role::User, "longthink:1")]);
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let (mut thinking, mut text) = (String::new(), String::new());
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::ThinkingDelta { text: t } => {
                    assert!(text.is_empty(), "思考は本文より先に流し切る");
                    thinking.push_str(&t);
                }
                StreamDelta::TextDelta { text: t } => text.push_str(&t),
                _ => {}
            }
        }
        assert!(
            thinking.starts_with("思考1。") && !thinking.contains("思考2。"),
            "{thinking}"
        );
        assert!(text.contains("段落1。"), "{text}");
    }

    #[tokio::test]
    async fn stub_calls_tool_on_search_prefix() {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "search: 経費規程")]);
        req.tools.push(crate::model::ToolDef::new(
            "doc_search",
            "d",
            serde_json::json!({}),
        ));
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut tool_name = None;
        let mut stop = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::ToolUseStart { name, .. } => tool_name = Some(name),
                StreamDelta::Done { stop_reason, .. } => stop = Some(stop_reason),
                _ => {}
            }
        }
        assert_eq!(tool_name.as_deref(), Some("doc_search"));
        assert_eq!(stop, Some(StopReason::ToolUse));
    }

    #[tokio::test]
    async fn stub_selects_tool_by_prefix() {
        // websearch: プレフィックスは提示ツールの中から web_search を選ぶ（順序非依存）。
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "websearch: rust")]);
        for name in ["code_interpreter", "web_search", "web_fetch"] {
            req.tools
                .push(crate::model::ToolDef::new(name, "d", serde_json::json!({})));
        }
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut tool_name = None;
        let mut input = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::ToolUseStart { name, .. } => tool_name = Some(name),
                StreamDelta::ToolUseStop { input: i, .. } => input = Some(i),
                _ => {}
            }
        }
        assert_eq!(tool_name.as_deref(), Some("web_search"));
        assert_eq!(input.unwrap()["query"], "rust");
    }

    /// `loop:<ツール名>` は名前のツールを、`loop:` と `loop: 説明` は先頭のツールを毎ターン呼ぶ。
    #[tokio::test]
    async fn stub_loop_calls_named_tool_or_first() {
        for (text, want) in [
            ("loop:drive.find", "drive.find"),
            ("loop:", "drive.copy"),
            ("loop: drive.find を呼び続ける", "drive.copy"),
            ("loop:無い名前", "drive.copy"),
        ] {
            let mut req = GenerateRequest::new(vec![Message::text(Role::User, text)]);
            for name in ["drive.copy", "drive.find"] {
                req.tools
                    .push(crate::model::ToolDef::new(name, "d", serde_json::json!({})));
            }
            let mut s = StubProvider::new().stream(&req).await.unwrap();
            let mut tool_name = None;
            while let Some(ev) = s.next().await {
                if let StreamDelta::ToolUseStart { name, .. } = ev.unwrap() {
                    tool_name = Some(name);
                }
            }
            assert_eq!(tool_name.as_deref(), Some(want), "{text}");
        }
    }

    #[tokio::test]
    async fn stub_python_prefix_selects_code_interpreter() {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "python: print(1)")]);
        req.tools.push(crate::model::ToolDef::new(
            "code_interpreter",
            "d",
            serde_json::json!({}),
        ));
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut tool_name = None;
        let mut input = None;
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::ToolUseStart { name, .. } => tool_name = Some(name),
                StreamDelta::ToolUseStop { input: i, .. } => input = Some(i),
                _ => {}
            }
        }
        assert_eq!(tool_name.as_deref(), Some("code_interpreter"));
        assert_eq!(input.unwrap()["code"], "print(1)");
    }

    /// 同じ要求を 2 回流しても、ツール呼び出しの id は違う（job の再試行で前の承認に当たらない）。
    #[tokio::test]
    async fn stub_tool_call_id_differs_between_identical_requests() {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "search: 経費規程")]);
        req.tools.push(crate::model::ToolDef::new(
            "doc_search",
            "d",
            serde_json::json!({}),
        ));
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut s = StubProvider::new().stream(&req).await.unwrap();
            let (mut start, mut stop) = (None, None);
            while let Some(ev) = s.next().await {
                match ev.unwrap() {
                    StreamDelta::ToolUseStart { id, .. } => start = Some(id),
                    StreamDelta::ToolUseStop { id, .. } => stop = Some(id),
                    _ => {}
                }
            }
            assert_eq!(start, stop, "開始と終了は同じ id");
            ids.push(start.unwrap());
        }
        assert_ne!(ids[0], ids[1]);
        assert!(ids[0].starts_with("stubtool_1_"), "{}", ids[0]);
    }

    #[tokio::test]
    async fn stub_falls_back_to_first_tool_when_named_absent() {
        // websearch: だが web_search が提示に無い → tools[0]（doc_search）にフォールバック。
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "websearch: x")]);
        req.tools.push(crate::model::ToolDef::new(
            "doc_search",
            "d",
            serde_json::json!({}),
        ));
        let mut s = StubProvider::new().stream(&req).await.unwrap();
        let mut tool_name = None;
        while let Some(ev) = s.next().await {
            if let StreamDelta::ToolUseStart { name, .. } = ev.unwrap() {
                tool_name = Some(name);
            }
        }
        assert_eq!(tool_name.as_deref(), Some("doc_search"));
    }
}
