//! stub プロバイダのストリーム生成ヘルパ（`stub.rs` から分割・行数規約）。
//!
//! ここは「どういう delta 列を返すか」だけを持つ。どの入力でどれを返すかの判定は `stub.rs`。

use std::time::Duration;

use futures::stream::{self, StreamExt};

use crate::model::{StopReason, StreamDelta, Usage};
use crate::provider::{DeltaStream, LlmError};

/// stub のツール呼び出しの id（`stubtool_<並びの番号>_<乱数>`）。
///
/// 実プロバイダ（Anthropic の `toolu_*`・OpenAI の `call_*`）と同じく**呼び出しごとに一意**にする。
/// 承認は (run, tool_call_id) を鍵にする（`chat` の `run_approval`）ので、手の番号だけの固定の id では、
/// job の再試行や同じ run の 2 枚目の承認が前の決定に当たり、承認のカード無しに素通りする
/// （stub で回すテストが偽の緑になる）。`n` は 1 ステップの中の並び（1 始まり）で、読みやすさのために残す。
pub(super) fn stub_tool_call_id(n: usize) -> String {
    format!("stubtool_{n}_{}", uuid::Uuid::new_v4().simple())
}

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
        let id = stub_tool_call_id(i + 1);
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
            ..Default::default()
        },
    }));
    stream::iter(events).boxed()
}

/// **出力上限で切れたツール呼び出し**のストリーム（`MaxTokens`）。
///
/// 実プロバイダは引数 JSON の途中で切れると、停止理由を `max_tokens`（OpenAI 互換は `length`）で返す。
/// アダプタは読めない引数の呼び出しを渡さない（BUG-225・[`super::tool_args`]）ので、届くのは `ToolUseStart`
/// だけで `ToolUseStop` は来ない。大きな表の編集などで実際に起きる形。切れるのは `cut` の呼び出し（id は
/// `stubtool_cut`）。`whole` があれば、その前に引数まで届いた呼び出し（id は `stubtool_whole`・引数 `{}`）を
/// 1 つ流す（1 応答に完全な呼び出しと切れた呼び出しが混ざる形）。
pub(super) fn truncated_tool_call_stream(
    cut: String,
    whole: Option<String>,
    prompt_tokens: u64,
) -> DeltaStream {
    let mut events = vec![Ok(StreamDelta::TextDelta {
        text: "表に書き込みます。".to_string(),
    })];
    if let Some(name) = whole {
        let id = "stubtool_whole".to_string();
        events.push(Ok(StreamDelta::ToolUseStart {
            id: id.clone(),
            name,
        }));
        events.push(Ok(StreamDelta::ToolUseStop {
            id,
            input: serde_json::json!({}),
        }));
    }
    events.push(Ok(StreamDelta::ToolUseStart {
        id: "stubtool_cut".to_string(),
        name: cut,
    }));
    events.push(Ok(StreamDelta::Done {
        stop_reason: StopReason::MaxTokens,
        usage: Usage {
            prompt_tokens,
            completion_tokens: 0,
            ..Default::default()
        },
    }));
    stream::iter(events).boxed()
}

/// 本文だけを流して自然終了するストリーム（`EndTurn`）。
///
/// 語単位で TextDelta に割る（実プロバイダのストリーミングと同じ形で UI の逐次描画を通す）。
pub(super) fn text_stream(reply: &str, prompt_tokens: u64) -> DeltaStream {
    text_stream_ending(reply, StopReason::EndTurn, prompt_tokens)
}

/// 本文の途中で出力上限に達して止まるストリーム（`MaxTokens`）。
pub(super) fn truncated_text_stream(reply: &str, prompt_tokens: u64) -> DeltaStream {
    text_stream_ending(reply, StopReason::MaxTokens, prompt_tokens)
}

/// 思考だけを流して出力上限で止まるストリーム（本文もツール呼び出しも無い `MaxTokens`）。
pub(super) fn thinking_only_stream(thinking: &str, prompt_tokens: u64) -> DeltaStream {
    stream::iter([
        Ok(StreamDelta::ThinkingDelta {
            text: thinking.to_string(),
        }),
        Ok(StreamDelta::Done {
            stop_reason: StopReason::MaxTokens,
            usage: Usage {
                prompt_tokens,
                completion_tokens: 1,
                ..Default::default()
            },
        }),
    ])
    .boxed()
}

/// 本文だけを流し、指定の停止理由で終えるストリーム（出力の上限での打ち切り `truncate:` など）。
pub(super) fn text_stream_ending(reply: &str, stop: StopReason, prompt_tokens: u64) -> DeltaStream {
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
        stop_reason: stop,
        usage: Usage {
            prompt_tokens,
            completion_tokens,
            ..Default::default()
        },
    }));
    stream::iter(events).boxed()
}

/// 本文を `chunk` 文字ずつ `gap` 間隔で流す（実 LLM と同じく行の途中で区切れる。
/// 生成途中の描画を目で追える速さにする・#530）。
pub(super) fn paced_chunks_stream(
    reply: &str,
    prompt_tokens: u64,
    chunk: usize,
    gap: Duration,
) -> DeltaStream {
    paced_thinking_then_text_stream("", reply, prompt_tokens, chunk, gap)
}

/// 思考（`ThinkingDelta`）を流し切ってから本文を流す。どちらも `chunk` 文字ずつ `gap` 間隔。
pub(super) fn paced_thinking_then_text_stream(
    thinking: &str,
    reply: &str,
    prompt_tokens: u64,
    chunk: usize,
    gap: Duration,
) -> DeltaStream {
    let pieces = |s: &str| -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        chars
            .chunks(chunk.max(1))
            .map(|c| c.iter().collect())
            .collect()
    };
    let deltas: Vec<StreamDelta> = pieces(thinking)
        .into_iter()
        .map(|text| StreamDelta::ThinkingDelta { text })
        .chain(
            pieces(reply)
                .into_iter()
                .map(|text| StreamDelta::TextDelta { text }),
        )
        .collect();
    let completion_tokens = deltas.len() as u64;
    stream::iter(deltas)
        .then(move |delta| async move {
            tokio::time::sleep(gap).await;
            Ok(delta)
        })
        .chain(stream::once(async move {
            Ok(StreamDelta::Done {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    prompt_tokens,
                    completion_tokens,
                    ..Default::default()
                },
            })
        }))
        .boxed()
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
                    ..Default::default()
                },
            })
        }))
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ストリームの ToolUseStart と ToolUseStop の id を出た順に集める。
    async fn tool_ids(mut s: DeltaStream) -> (Vec<String>, Vec<String>) {
        let (mut starts, mut stops) = (Vec::new(), Vec::new());
        while let Some(ev) = s.next().await {
            match ev.unwrap() {
                StreamDelta::ToolUseStart { id, .. } => starts.push(id),
                StreamDelta::ToolUseStop { id, .. } => stops.push(id),
                _ => {}
            }
        }
        (starts, stops)
    }

    /// 1 ステップの複数の呼び出しの id は互いに違い、同じ呼び出しを流し直しても前の id と重ならない
    /// （承認の鍵 (run, tool_call_id) が前の決定に当たらない）。開始と終了は同じ id を持つ。
    #[tokio::test]
    async fn tool_call_ids_are_unique_per_call_and_per_stream() {
        let calls = || {
            vec![
                ("web_search".to_string(), serde_json::json!({"query": "a"})),
                ("web_fetch".to_string(), serde_json::json!({"url": "b"})),
            ]
        };
        let (first, first_stops) = tool_ids(tool_calls_stream(calls(), 1)).await;
        let (again, _) = tool_ids(tool_calls_stream(calls(), 1)).await;
        assert_eq!(first.len(), 2);
        assert_eq!(first, first_stops, "開始と終了は同じ id");
        assert_ne!(first[0], first[1], "1 ステップの中で重ならない");
        for id in &again {
            assert!(!first.contains(id), "流し直しで同じ id が出た: {id}");
        }
        assert!(first[0].starts_with("stubtool_1_"), "{}", first[0]);
        assert!(first[1].starts_with("stubtool_2_"), "{}", first[1]);
    }
}
