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
///
/// `extensible` は、この run の上限が途中で 1 回だけ広がりうるか（[`crate::BudgetExtension`]・D-204）。
/// 広がった後は [予算] の残量が広げた上限で数えられるので、ここに書いた上限より残量を正とさせる
/// （system は run 内で変えない。どの道具で広がるかは書かない＝広げるために道具を呼ばせない）。
pub(crate) fn budget_system_prompt(
    system: Option<&str>,
    budget: &Budget,
    has_tools: bool,
    extensible: bool,
) -> String {
    let extension_note = if extensible {
        "作業の内容によっては、この上限が途中で広がることがあります。\
         そのときはツール結果の [予算] の残量を正としてください。"
    } else {
        ""
    };
    let tool_plan = if has_tools {
        "複数工程の依頼では、何をどの順で調べ、何を実行して\
         どう結果を確かめるかを、利用者に見える本文として最初に 1〜3 文で伝えてください。\
         思考欄だけに書かず、内部の予算数値や長い作業一覧は説明に含めないでください。\
         単純な質問はそのまま回答して構いません。\
         計画だけのために応答を終えず、計画の本文と最初のツール呼び出しを同じ生成で返してください。"
    } else {
        "ツールは提示されていません。指定された出力形式を守り、成果物をそのまま返してください。"
    };
    format!(
        "{}\n\n[実行予算と計画]\n\
         この run の上限は {} ステップです。{extension_note}LLM の生成 1 回が 1 ステップで、\
         ツール結果を受けた後の生成・計画ツールの呼び出し・最終回答も数えます。\
         同じ生成で複数ツールを呼んでもステップ数は 1 ですが、依存する操作は結果を見てから進めてください。\n\
         最初に、依頼の完了条件と、必要な調査・操作・結果確認・最終回答までを上限内に収める\
         短い手順を見積もってください。{tool_plan}\
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

/// 出力上限で切れたツール呼び出しへ返す結果の目印（回数を数えるのにも使う）。
const TRUNCATED_CALL_MARK: &str = "[出力上限] この呼び出しは";

/// 1 run で「切れた呼び出し」を書き直させる回数の上限。超えたら Truncated で畳む
/// （毎回同じ大きさで書いて切れ続けるモデルで、ステップ予算を空費させない）。
const MAX_TRUNCATED_RETRIES: usize = 3;

/// 出力の上限（`max_tokens`）で止まった応答のツール呼び出し（[`truncated_call_results`] の入力・(id, 名前)）。
pub(crate) struct TruncatedCalls<'a> {
    /// 引数の途中で切れた呼び出し（始まりだけ届き、引数の届かなかったもの・BUG-225）。
    pub(crate) cut: &'a [(String, String)],
    /// 同じ応答の、引数まで届いた呼び出し（応答が上限で止まったので、これも実行しない）。
    pub(crate) whole: &'a [(String, String)],
}

/// **出力上限（`max_tokens`）で止まった応答のツール呼び出し**へ返す結果を組む。上限回数を超えたら `None`。
///
/// 切れた呼び出しの引数は途中までしか無く、アダプタは読めない引数の呼び出しを渡さない（BUG-225）。
/// ループは始まりだけ届いた呼び出しを `{}` で履歴に積み、ここで閉じる。**実行しない**（空の編集・既定値での
/// 操作になる）。黙って run を畳むと、画面ではツールカードが出たまま止まり、利用者には何が起きたか分からない
/// （実測: エージェントモードで大きな表の編集・委譲の指示文が切れ、そのまま終わっていた）。
/// 「切れた・小さく分けて呼び直せ」をツール結果として返し、次の生成で書き直させる。
/// tool_use と対の tool_result にするのは、両プロバイダが確実に受け付ける形だから（冒頭の注記）。
///
/// 同じ応答の、引数まで届いた呼び出し（`whole`）も実行しない（応答が途中で止まっているので、モデルが続けて
/// 出すはずだった呼び出しとの順や組を保てない）。それには「切れた」とは言わず、同じ呼び出しを送り直させる。
///
/// `widened_to` は、この切れた呼び出しで次の応答の出力上限が広がったとき（作成系の道具・D-204）の新しい上限。
/// そのときは分けて呼ばせず、同じ呼び出しを書き直させる（分けると版や編集が余分に増える）。
pub(crate) fn truncated_call_results(
    messages: &[LlmMessage],
    calls: &TruncatedCalls<'_>,
    max_tokens: Option<u32>,
    widened_to: Option<u32>,
) -> Option<Vec<Block>> {
    // 回数は**切れた応答（ステップ）単位**で数える（1 応答に複数の呼び出しがあっても 1 回）。
    let retried = messages
        .iter()
        .filter(|m| {
            m.content.iter().any(|b| {
                matches!(b, Block::ToolResult { content, .. } if content.starts_with(TRUNCATED_CALL_MARK))
            })
        })
        .count();
    if retried >= MAX_TRUNCATED_RETRIES {
        return None;
    }
    let limit = max_tokens.map_or_else(String::new, |n| format!("（{n} トークン）"));
    let widened = widened_to.filter(|n| max_tokens.is_some_and(|m| *n > m));
    let retry = match widened {
        Some(n) => format!(
            "次の応答からは出力上限を {n} トークンに広げたので、思考を短くして同じ呼び出しを\
             書き直してください。"
        ),
        None => "1 回の呼び出しを小さく分けて呼び直してください（例: 編集する範囲・操作を\
                 数回に分ける、長い本文は複数回に分けて書く）。思考は短くしてください。"
            .to_string(),
    };
    let whole = calls.whole.iter().map(|(id, name)| {
        Block::tool_result(
            id.clone(),
            format!(
                "{TRUNCATED_CALL_MARK}、応答が出力上限{limit}で止まったため実行していません\
                 （{name} は何も変更していません）。引数は届いているので、同じ呼び出しをもう一度\
                 送ってください。"
            ),
            true,
        )
    });
    let cut = calls.cut.iter().map(|(id, name)| {
        Block::tool_result(
            id.clone(),
            format!(
                "{TRUNCATED_CALL_MARK}応答の出力上限{limit}に達して引数の途中で切れたため、\
                 実行していません（{name} は何も変更していません）。{retry}"
            ),
            true,
        )
    });
    Some(whole.chain(cut).collect())
}

/// 1 run で「本文の続き」を書かせる回数の上限（1 回の上限 × (この回数 + 1) までの本文を書ける）。
/// 超えたら Truncated で畳む（同じ所を書き直し続けるモデルで、ステップ予算を空費させない）。
const MAX_TEXT_CONTINUATIONS: usize = 4;

/// **本文の途中で出力上限（`max_tokens`）に達した**応答の続きを書かせる催促。上限回数を超えたら `None`。
///
/// ツール呼び出しの無い長い回答（報告書・表・図の多い分析）は、最後の 1 応答だけで上限を
/// 使い切ることがある。そこで run を畳むと、利用者が「続けて」と送り直さないと終わらない
/// （実測: エージェントモードで、長い分析の報告が「## 6.」の見出しで切れて止まった）。
/// 切れた assistant 応答の直後に user 発話として足し、同じ run のまま続きを生成させる。
/// 直前が assistant なので user の連続にはならない（冒頭の注記の制約に当たらない）。
/// 続きの本文は画面で直前の本文ブロックへそのまま連結される（chat の sink）。
pub(crate) fn text_continuation(messages: &[LlmMessage]) -> Option<LlmMessage> {
    let continued = messages.iter().filter(|m| is_continuation_nudge(m)).count();
    if continued >= MAX_TEXT_CONTINUATIONS {
        return None;
    }
    Some(LlmMessage::text(
        llm_gateway::Role::User,
        format!(
            "{} 直前のあなたの応答は 1 回の出力上限に達して途中で切れました（利用者の発話ではなく\
             システムからの連絡です）。切れた位置の直後から、そのまま続きを書いてください。\
             前置き・お詫び・ここまでの要約・書いた部分の繰り返しは書かないでください。\
             文・表の行・```ブロックの途中で切れていれば、その続きの文字から書いてください。\
             残りが長いときは簡潔にまとめて回答を終えてください。",
            llm_gateway::CONTINUATION_MARK
        ),
    ))
}

/// 次の生成が**本文の続き**を書く生成か（履歴の最後が続きの催促）。
pub(crate) fn is_text_continuation(messages: &[LlmMessage]) -> bool {
    messages.last().is_some_and(is_continuation_nudge)
}

fn is_continuation_nudge(m: &LlmMessage) -> bool {
    m.role == llm_gateway::Role::User
        && m.content.iter().any(|b| {
            matches!(b, Block::Text { text } if text.starts_with(llm_gateway::CONTINUATION_MARK))
        })
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

    /// 本文の続きの催促は assistant の直後に user として足し、上限回数で打ち切る。
    #[test]
    fn text_continuation_is_a_user_turn_and_bounded() {
        let mut messages = vec![LlmMessage::text(LlmRole::User, "報告を書いて")];
        for _ in 0..MAX_TEXT_CONTINUATIONS {
            messages.push(LlmMessage::text(LlmRole::Assistant, "途中まで"));
            let nudge = text_continuation(&messages).unwrap();
            assert_eq!(nudge.role, LlmRole::User);
            let Block::Text { text } = &nudge.content[0] else {
                panic!("本文ブロック: {nudge:?}");
            };
            assert!(text.starts_with(llm_gateway::CONTINUATION_MARK), "{text}");
            assert!(text.contains("続き"), "{text}");
            messages.push(nudge);
        }
        assert!(is_text_continuation(&messages), "最後が催促なら続きの生成");
        messages.push(LlmMessage::text(LlmRole::Assistant, "まだ途中"));
        assert!(!is_text_continuation(&messages));
        assert!(text_continuation(&messages).is_none(), "上限回数で打ち切る");
    }

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

    /// 全部が引数の途中で切れた呼び出しの組。
    fn cut(ids: &[(String, String)]) -> TruncatedCalls<'_> {
        TruncatedCalls {
            cut: ids,
            whole: &[],
        }
    }

    /// 同じ応答に引数まで届いた呼び出しと切れた呼び出しが混ざったら、届いた方を「切れた」と言わず、
    /// 切れた方にだけ「切れた」と返す（どちらも実行しない・届いた順に返す）。
    #[test]
    fn whole_and_cut_calls_in_one_truncated_answer_get_their_own_results() {
        let whole = vec![("w1".to_string(), "doc_search".to_string())];
        let cut = vec![("c1".to_string(), "emit_workflow".to_string())];
        let calls = TruncatedCalls {
            cut: &cut,
            whole: &whole,
        };
        let messages = vec![LlmMessage::text(LlmRole::User, "作って")];
        let blocks = truncated_call_results(&messages, &calls, Some(8192), Some(32768)).unwrap();
        let results: Vec<(&str, &str)> = blocks
            .iter()
            .map(|b| match b {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error: true,
                    ..
                } => (tool_use_id.as_str(), content.as_str()),
                other => panic!("形が違う: {other:?}"),
            })
            .collect();
        assert_eq!(results.len(), 2, "{results:?}");
        let (id, whole_text) = results[0];
        assert_eq!(id, "w1");
        assert!(whole_text.starts_with(TRUNCATED_CALL_MARK), "{whole_text}");
        assert!(!whole_text.contains("切れた"), "{whole_text}");
        assert!(whole_text.contains("引数は届いている"), "{whole_text}");
        assert!(whole_text.contains("実行していません"), "{whole_text}");
        let (id, cut_text) = results[1];
        assert_eq!(id, "c1");
        assert!(cut_text.contains("引数の途中で切れた"), "{cut_text}");
        assert!(cut_text.contains("32768 トークンに広げた"), "{cut_text}");
        // 2 つの呼び出しがあっても、書き直させた回数は応答単位で 1 回。
        let mut messages = messages;
        messages.push(LlmMessage {
            role: LlmRole::Tool,
            content: blocks,
        });
        for _ in 1..MAX_TRUNCATED_RETRIES {
            assert!(truncated_call_results(&messages, &calls, Some(8192), None).is_some());
            messages.push(messages[1].clone());
        }
        assert!(truncated_call_results(&messages, &calls, Some(8192), None).is_none());
    }

    /// 切れた呼び出しは失敗の結果で閉じ、上限回数を超えたら諦める。
    #[test]
    fn truncated_calls_are_closed_and_retried_a_bounded_number_of_times() {
        // 1 応答に 2 件の呼び出しがあっても、数えるのは応答（ステップ）単位。
        let ids = vec![
            ("c1".to_string(), "office.excel_edit".to_string()),
            ("c2".to_string(), "drive.move".to_string()),
        ];
        let mut messages = vec![LlmMessage::text(LlmRole::User, "埋めて")];
        for _ in 0..MAX_TRUNCATED_RETRIES {
            let blocks = truncated_call_results(&messages, &cut(&ids), Some(8192), None).unwrap();
            let Block::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } = &blocks[0]
            else {
                panic!("形が違う");
            };
            assert_eq!(tool_use_id, "c1");
            assert!(*is_error);
            assert!(content.contains("8192 トークン"), "{content}");
            assert!(content.contains("実行していません"), "{content}");
            assert!(content.contains("小さく分けて"), "{content}");
            messages.push(LlmMessage {
                role: LlmRole::Tool,
                content: blocks,
            });
        }
        assert!(
            truncated_call_results(&messages, &cut(&ids), Some(8192), None).is_none(),
            "上限回数を超えたら書き直させない"
        );
    }

    /// 切れた呼び出しで次の応答の上限が広がったとき（D-204）は、分けさせず同じ呼び出しを書き直させる。
    #[test]
    fn truncated_call_with_a_widened_limit_asks_to_rewrite_the_same_call() {
        let ids = vec![("c1".to_string(), "emit_workflow".to_string())];
        let messages = vec![LlmMessage::text(LlmRole::User, "作って")];
        let blocks =
            truncated_call_results(&messages, &cut(&ids), Some(8192), Some(32768)).unwrap();
        let Block::ToolResult { content, .. } = &blocks[0] else {
            panic!("形が違う");
        };
        assert!(content.contains("8192 トークン"), "{content}");
        assert!(content.contains("32768 トークンに広げた"), "{content}");
        assert!(content.contains("同じ呼び出しを"), "{content}");
        assert!(!content.contains("小さく分けて"), "{content}");
        // 広がっていない（同じ値）なら従来の文。
        let same = truncated_call_results(&messages, &cut(&ids), Some(8192), Some(8192)).unwrap();
        let Block::ToolResult { content, .. } = &same[0] else {
            panic!("形が違う");
        };
        assert!(content.contains("小さく分けて"), "{content}");
    }

    /// 上限が途中で広がりうる run（D-204）だけ、system の上限より [予算] の残量を正とさせる。
    /// どの道具で広がるかは書かない（広げるために道具を呼ばせない）。
    #[test]
    fn system_prompt_mentions_the_extension_only_when_it_can_happen() {
        let budget = Budget::chat(6);
        let plain = budget_system_prompt(Some("前置き"), &budget, true, false);
        assert!(plain.starts_with("前置き\n\n[実行予算と計画]"), "{plain}");
        assert!(plain.contains("上限は 6 ステップです。"), "{plain}");
        assert!(!plain.contains("広がる"), "{plain}");
        let wide = budget_system_prompt(Some("前置き"), &budget, true, true);
        assert!(wide.contains("上限は 6 ステップです。"), "{wide}");
        assert!(wide.contains("上限が途中で広がることがあります"), "{wide}");
        assert!(wide.contains("[予算] の残量を正としてください"), "{wide}");
        assert!(!wide.contains("emit_workflow"), "{wide}");
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
