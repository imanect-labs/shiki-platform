//! stub プロバイダのストリーム生成ヘルパ（`stub.rs` から分割・500 行規約）。
//!
//! ここは「どういう delta 列を返すか」だけを持つ。どの入力でどれを返すかの判定は `stub.rs`。

use std::time::Duration;

use futures::stream::{self, StreamExt};

use crate::model::{StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError};

/// 単一ツール呼び出し（ToolUse で停止）のストリームを組む決定的ヘルパ。
pub(super) fn tool_call_stream(
    name: String,
    input: serde_json::Value,
    prompt_tokens: u64,
) -> DeltaStream {
    tool_calls_stream(vec![(name, input)], prompt_tokens)
}

/// **1 ステップで複数ツール**を呼ぶストリーム（同一ステップ＝並行実行の決定的駆動）。
///
/// 冪等 read（web_search / web_fetch / doc_search）を複数返すと agent-core が有界並列で
/// 走らせる（#349）。UI の「並行して N 件」表示や step グルーピングはこれでしか再現できない。
pub(super) fn tool_calls_stream(
    calls: Vec<(String, serde_json::Value)>,
    prompt_tokens: u64,
) -> DeltaStream {
    let mut events = Vec::with_capacity(calls.len() * 2 + 1);
    for (i, (name, input)) in calls.into_iter().enumerate() {
        let id = format!("stubtool_{}", i + 1);
        events.push(Ok(StreamDelta::ToolUseStart {
            id: id.clone(),
            name,
        }));
        events.push(Ok(StreamDelta::ToolUseStop { id, input }));
    }
    events.push(Ok(StreamDelta::Done {
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            prompt_tokens,
            completion_tokens: 0,
        },
    }));
    stream::iter(events).boxed()
}

/// 本文だけを流して自然終了するストリーム（`EndTurn`）。
///
/// 語単位で TextDelta に割る（実プロバイダのストリーミングと同じ形で UI の逐次描画を通す）。
pub(super) fn text_stream(reply: &str, prompt_tokens: u64) -> DeltaStream {
    let words: Vec<String> = reply
        .split_inclusive(char::is_whitespace)
        .map(str::to_string)
        .collect();
    let completion_tokens = words.len() as u64;
    let mut events: Vec<Result<StreamDelta, LlmError>> = words
        .into_iter()
        .map(|w| Ok(StreamDelta::TextDelta { text: w }))
        .collect();
    events.push(Ok(StreamDelta::Done {
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            prompt_tokens,
            completion_tokens,
        },
    }));
    stream::iter(events).boxed()
}

/// 本文を出し切ってから `hold` だけ待って終える（生成中の UI を試すための遅延）。
///
/// 先に本文を出すのは、e2e が「生成が始まった」ことを本文の出現で待てるようにするため。
pub(super) fn slow_text_stream(reply: &str, prompt_tokens: u64, hold: Duration) -> DeltaStream {
    let completion_tokens = reply.split_whitespace().count() as u64;
    let text = reply.to_string();
    stream::once(async move { Ok(StreamDelta::TextDelta { text }) })
        .chain(stream::once(async move {
            tokio::time::sleep(hold).await;
            Ok(StreamDelta::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    prompt_tokens,
                    completion_tokens,
                },
            })
        }))
        .boxed()
}
