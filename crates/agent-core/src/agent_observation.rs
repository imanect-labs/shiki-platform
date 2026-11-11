//! ステップ境界でモデルへ渡す**観測**の組み立て（`agent.rs` から分割・行数規約）。
//!
//! どちらも**最後のツール結果の `content` へ書き足す**。独立した user メッセージにはできない:
//! Anthropic は user メッセージの連続を受け付けず、OpenAI 互換は `Role::Tool` の Text ブロックを
//! 捨てる。system プロンプトへ足す案は、毎ステップ変わる値なのでプロンプトキャッシュの前置きを
//! 毎回壊す。ツール結果なら両プロバイダに確実に届き、その step の新規ぶんに乗る。
//!
//! UI には出ない（`agent_tools::emit_tool_events` は書き足す**前**の `outcome.content` で発火する）。

use std::time::Instant;

use llm_gateway::{Block, Message as LlmMessage};

use crate::budget::{Budget, Spent};

/// run 内で変化しない上限と計画の作法。残量はツール結果で更新し、system のキャッシュを保つ。
pub(crate) fn budget_system_prompt(system: Option<&str>, budget: &Budget) -> String {
    format!(
        "{}\n\n[実行予算と計画]\n\
         この run の上限は {} ステップです。LLM の生成 1 回が 1 ステップで、\
         ツール結果を受けた後の生成・計画ツールの呼び出し・最終回答も数えます。\
         同じ生成で複数ツールを呼んでもステップ数は 1 ですが、依存する操作は結果を見てから進めてください。\n\
         最初に、依頼の完了条件と、必要な調査・操作・結果確認・最終回答までを上限内に収める\
         短い手順を見積もってください。複数工程の依頼では、何をどの順で調べ、何を実行して\
         どう結果を確かめるかを、利用者に見える本文として最初に 1〜3 文で伝えてください。\
         思考欄だけに書かず、内部の予算数値や長い作業一覧は説明に含めないでください。\
         単純な質問はそのまま回答して構いません。\
         計画だけのために応答を終えず、計画の本文と最初のツール呼び出しを同じ生成で返してください。\
         既存の手順が計画や承認を要求する場合は従ってください。\n\
         ツール結果の [予算] は次の生成前の残量です。再開時は実行済みの作業と残量から続け、\
         残量と未完の必須工程に合わせて手順を更新してください。\
         任意の追加調査より、依頼された操作と結果確認・最終回答の手数を確保してください。\
         必須の確認・承認は省略せず、実行していない操作を完了と報告しないでください。\
         必須工程が収まらないと分かったら、追加探索を続けず、不足条件と実行済み・未実行を明示してください。",
        system.unwrap_or_default(),
        budget.max_steps,
    )
}

/// 残り予算を**最後のツール結果へ書き足す**（#407）。
///
/// 独立した user メッセージにはできない: Anthropic は user メッセージの連続を受け付けず、
/// OpenAI 互換は `Role::Tool` の Text ブロックを捨てる。両プロバイダに確実に届くのは
/// ツール結果の `content` だけ。**その step の新規ぶんに乗る**ので、system プロンプトへ
/// 埋める案と違いプロンプトキャッシュの前置きを壊さない。
///
/// ツール結果が 1 つも無いステップは終端（ループが抜ける）なので、添える先も要らない。
pub(crate) fn append_budget_observation(blocks: &mut [Block], budget: &Budget, spent: &Spent) {
    let last = blocks
        .iter_mut()
        .rev()
        .find(|b| matches!(b, Block::ToolResult { .. }));
    if let Some(Block::ToolResult { content, .. }) = last {
        content.push_str("\n\n");
        content.push_str(&budget.remaining(spent, Instant::now()).observation());
    }
}

/// 空応答へ 1 度だけ入れる催促（**最後のツール結果に添える**）。
///
/// 独立した user メッセージにしないのは残り予算と同じ理由（user の連続を拒否するプロバイダが
/// あり、`Role::Tool` の Text ブロックを捨てるプロバイダもある）。ツール結果が無い＝1 手目から
/// 空だった場合は催促しない（材料が無く、同じ結果になる）。
const EMPTY_RESPONSE_NUDGE: &str = "\n\n[注意] 直前の応答は本文が空でした。\
     思考の中ではなく**応答の本文として**、ここまでに得た内容で成果物を書いてください。";

/// 予算超過で畳む直前に入れる**最後の指示**（ツールを外した 1 ターンで書かせる）。
const FINAL_ANSWER_INSTRUCTION: &str = "\n\n[予算] 上限に達しました。\
     **これ以上ツールは使えません。**ここまでに集めた内容だけで、依頼された成果物を\
     いま書いてください。足りない点は「確認できなかった」と明示すれば十分です。\
     次の応答が最後になります。";

/// 予算超過の「最後に書かせる」指示を仕込む。添える先が無ければ `false`。
///
/// 既に催促済みかは見ない（着地は 1 回しか呼ばれない＝ループを抜ける直前）。
pub(crate) fn demand_final_answer(messages: &mut [LlmMessage]) -> bool {
    append_to_last_tool_result(messages, FINAL_ANSWER_INSTRUCTION)
}

/// 空応答の催促を仕込む。既に催促済み、または添える先が無ければ `false`（＝諦めて畳む）。
pub(crate) fn nudge_empty_response(messages: &mut [LlmMessage]) -> bool {
    let already = last_tool_result(messages).is_some_and(|c| c.contains(EMPTY_RESPONSE_NUDGE));
    // 2 度目は打ち切る（空応答で無限に回さない）。
    !already && append_to_last_tool_result(messages, EMPTY_RESPONSE_NUDGE)
}

/// 履歴の**最後のツール結果**へ書き足す（無ければ `false`）。
fn append_to_last_tool_result(messages: &mut [LlmMessage], note: &str) -> bool {
    let Some(content) = last_tool_result(messages) else {
        return false;
    };
    content.push_str(note);
    true
}

/// 履歴の最後のツール結果の本文（可変参照）。
fn last_tool_result(messages: &mut [LlmMessage]) -> Option<&mut String> {
    messages
        .iter_mut()
        .rev()
        .flat_map(|m| m.content.iter_mut().rev())
        .find_map(|b| match b {
            Block::ToolResult { content, .. } => Some(content),
            _ => None,
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use llm_gateway::Role as LlmRole;

    fn result(id: &str, content: &str) -> Block {
        Block::tool_result(id, content, false)
    }

    /// 残量は**最後のツール結果**に 1 度だけ乗る（両プロバイダに確実に届く唯一の場所）。
    #[test]
    fn budget_observation_rides_on_the_last_tool_result() {
        let mut blocks = vec![result("a", "1 件"), result("b", "2 件")];
        let mut spent = Spent::default();
        spent.add_step(100, 50, 0);
        append_budget_observation(
            &mut blocks,
            &Budget::autonomous(8, None, 120_000, 1),
            &spent,
        );
        let Block::ToolResult { content: first, .. } = &blocks[0] else {
            panic!("形が違う");
        };
        let Block::ToolResult { content: last, .. } = &blocks[1] else {
            panic!("形が違う");
        };
        assert_eq!(first, "1 件", "先頭には付けない（1 ステップ 1 回）");
        assert!(last.starts_with("2 件"), "{last}");
        assert!(last.contains("[予算] 残り 7 ステップ"), "{last}");
    }

    /// 空応答は 1 度だけ書き直させ、2 度目は諦める（空応答で無限に回さない）。
    #[test]
    fn empty_response_is_nudged_exactly_once() {
        let mut messages = vec![LlmMessage {
            role: LlmRole::Tool,
            content: vec![result("a", "1 件")],
        }];
        assert!(nudge_empty_response(&mut messages), "1 度目は催促する");
        let Block::ToolResult { content, .. } = &messages[0].content[0] else {
            panic!("形が違う");
        };
        assert!(content.starts_with("1 件"), "{content}");
        assert!(content.contains("本文が空でした"), "{content}");
        assert!(!nudge_empty_response(&mut messages), "2 度目は諦める");
    }

    /// 1 手目から空（ツール結果が無い）なら催促しない — 材料が無く同じ結果になる。
    #[test]
    fn nothing_to_nudge_without_a_tool_result() {
        let mut messages = vec![LlmMessage::text(LlmRole::User, "調べて")];
        assert!(!nudge_empty_response(&mut messages));
    }

    /// 予算超過の着地指示は「ツールはもう使えない・いま書け」と伝える。
    #[test]
    fn final_answer_demand_forbids_more_tools() {
        let mut messages = vec![LlmMessage {
            role: LlmRole::Tool,
            content: vec![result("a", "12 件")],
        }];
        assert!(demand_final_answer(&mut messages));
        let Block::ToolResult { content, .. } = &messages[0].content[0] else {
            panic!("形が違う");
        };
        assert!(content.starts_with("12 件"), "{content}");
        assert!(content.contains("これ以上ツールは使えません"), "{content}");
        assert!(content.contains("確認できなかった"), "{content}");
        // 1 手も実行していない run には添える先が無い（書かせる材料も無い）。
        let mut bare = vec![LlmMessage::text(LlmRole::User, "調べて")];
        assert!(!demand_final_answer(&mut bare));
    }

    /// ツール結果が無いステップ（終端）では何もしない。
    #[test]
    fn budget_observation_needs_a_tool_result_to_ride_on() {
        let mut blocks = vec![Block::Text {
            text: "本文".into(),
        }];
        append_budget_observation(
            &mut blocks,
            &Budget::autonomous(8, None, 120_000, 1),
            &Spent::default(),
        );
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], Block::Text { text } if text == "本文"));
    }
}
