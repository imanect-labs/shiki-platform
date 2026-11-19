//! ステップ境界でモデルへ渡す**観測**の組み立て（`agent.rs` から分割・行数規約）。
//!
//! どちらも**最後のツール結果の `content` へ書き足す**。独立した user メッセージにはできない:
//! Anthropic は user メッセージの連続を受け付けず、OpenAI 互換は `Role::Tool` の Text ブロックを
//! 捨てる。system プロンプトへ足す案は、毎ステップ変わる値なのでプロンプトキャッシュの前置きを
//! 毎回壊す。ツール結果なら両プロバイダに確実に届き、その step の新規ぶんに乗る。
//!
//! UI には出ない（`agent_tools::emit_tool_events` は書き足す**前**の `outcome.content` で発火する）。

use std::time::Instant;

use llm_gateway::{Block, Message as LlmMessage, StopReason};

use crate::budget::{Budget, Spent};

/// run 内で変化しない上限と計画の作法。残量はツール結果で更新し、system のキャッシュを保つ。
pub(crate) fn budget_system_prompt(
    system: Option<&str>,
    budget: &Budget,
    has_tools: bool,
) -> String {
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
         この run の上限は {} ステップです。LLM の生成 1 回が 1 ステップで、\
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

/// **出力上限（`max_tokens`）で切れたツール呼び出し**へ返す結果を組む。上限回数を超えたら `None`。
///
/// 切れた呼び出しの引数は途中までしか無く、プロバイダは解釈できない引数を `{}` にして返す
/// （openai.rs / anthropic.rs）。それを実行すると空の編集・既定値での操作になるので**実行しない**。
/// 黙って run を畳むと、画面ではツールカードが出たまま止まり、利用者には何が起きたか分からない
/// （実測: エージェントモードで大きな表の編集・委譲の指示文が切れ、そのまま終わっていた）。
/// 「切れた・小さく分けて呼び直せ」をツール結果として返し、次の生成で書き直させる。
/// tool_use と対の tool_result にするのは、両プロバイダが確実に受け付ける形だから（冒頭の注記）。
/// 出力上限で切れた呼び出しへ返す本文（実行していない・小さく分けて呼び直す）。
pub(crate) fn truncated_call_message(name: &str, max_tokens: Option<u32>) -> String {
    let limit = max_tokens.map_or_else(String::new, |n| format!("（{n} トークン）"));
    format!(
        "{TRUNCATED_CALL_MARK}応答の出力上限{limit}に達して引数の途中で切れたため、\
         実行していません（{name} は何も変更していません）。\
         1 回の呼び出しを小さく分けて呼び直してください（例: 編集する範囲・操作を\
         数回に分ける、長い本文は複数回に分けて書く）。思考は短くしてください。"
    )
}

pub(crate) fn truncated_call_results(
    messages: &[LlmMessage],
    call_ids: &[(String, String)],
    max_tokens: Option<u32>,
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
    Some(
        call_ids
            .iter()
            .map(|(id, name)| {
                Block::tool_result(id.clone(), truncated_call_message(name, max_tokens), true)
            })
            .collect(),
    )
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

/// 1 run で「思考だけで出力上限に達した」応答を書き直させる回数の上限。超えたら従来どおり
/// 空応答の催促・Truncated へ落とす（思考を絞っても書けないモデルで予算を空費しない）。
const MAX_THINKING_RETRIES: usize = 4;

/// **思考だけで出力上限（`max_tokens`）に達した**応答（本文もツール呼び出しも無い）への催促。
/// 上限回数を超えた、または添える先が無ければ `false`。
///
/// 集計の多い作業で、ツール結果を受けた後の思考が 1 回の上限を使い切り、何も出さずに
/// 止まることがある（実測: プレビューのエージェントモードで、欠損チェックの SQL が拒否された
/// 直後に思考だけで 8192 トークンを使い切り、「続けて」の案内で run が畳まれた）。
/// 空応答の催促（[`EMPTY_RESPONSE_NUDGE`]）は「本文として書け」なので、作業の途中では
/// 筋が違う（まだ書く材料がそろっていない）。思考を切り上げて**次の手を出せ**と返し、
/// 次の生成は effort を下げて走らせる（[`thinking_was_exhausted`]）。
///
/// 添える先は履歴の**最後のメッセージ**（ツール結果か user 発話）。切れた応答は履歴へ積まない
/// （思考だけの assistant を送り返す意味は無く、content が空のメッセージを拒否するプロバイダもある）
/// ので、直後の生成で必ず最後に見える。
pub(crate) fn nudge_thinking_exhausted(
    messages: &mut [LlmMessage],
    max_tokens: Option<u32>,
) -> bool {
    let tried: usize = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(block_text)
        .map(|t| t.matches(llm_gateway::THINKING_EXHAUSTED_MARK).count())
        .sum();
    if tried >= MAX_THINKING_RETRIES {
        return false;
    }
    let limit = max_tokens.map_or_else(String::new, |n| format!("（{n} トークン）"));
    let note = format!(
        "\n\n{} 直前の応答は思考だけで出力上限{limit}に達し、本文もツール呼び出しも\
         出せませんでした（何も実行していません。利用者の発話ではなくシステムからの連絡です）。\
         思考は短く切り上げ、次のツール呼び出しか回答の本文をすぐに出してください。\
         大きな処理は小さく分けて 1 手ずつ進めてください。",
        llm_gateway::THINKING_EXHAUSTED_MARK
    );
    let Some(last) = messages.last_mut() else {
        return false;
    };
    let is_user = last.role == llm_gateway::Role::User;
    let target = last.content.iter_mut().rev().find_map(|b| match b {
        Block::ToolResult { content, .. } => Some(content),
        Block::Text { text } if is_user => Some(text),
        _ => None,
    });
    match target {
        Some(t) => {
            t.push_str(&note);
            true
        }
        None => false,
    }
}

/// 出力上限で切れた応答のうち、**見えている本文が出力量に比べてごく短い**もの（出力の大半を
/// 思考に使い切った）か。`completion_tokens` が届かないプロバイダ（0）では判定しない。
///
/// 実測（プレビュー・Claude Haiku・上限 8192）: 20 枚のスライドを数枚ずつ足す作業で、次の 6 枚の
/// 原稿を思考の中で書き進めて上限を使い切り、本文は「9〜14枚目を追記します。」の 1 行だけで
/// 切れた。本文の途中切れとして「続きを書いて簡潔に終えて」と促すと、次の生成も同じように思考で
/// 使い切って止まり、「続けて」の案内で run が畳まれた。本文 1 文字はおおむね 1 トークン以上なので、
/// 本文が出力量の 1/4 に満たなければ、切れたのは本文ではなく思考とみなす。
pub(crate) fn output_mostly_hidden(text: &str, completion_tokens: u64) -> bool {
    completion_tokens > 0 && (text.chars().count() as u64).saturating_mul(4) < completion_tokens
}

/// 本文が短いまま思考で出力上限に達した応答への催促（**user 発話**として積む。直前に切れた
/// 本文を assistant として積んだ後なので、user の連続にはならない）。上限を超えたら `None`。
///
/// 本文が短いことは「作業予告だけ」とは限らない（思考の後に回答を書き始め、表の途中で切れた
/// こともあり得る）。そこで**本文の続きの催促でもある**形にする: 先頭に
/// [`llm_gateway::CONTINUATION_MARK`] を置き（次の生成は思考を画面へ流さず、本文が直前と連結
/// される・回数は続きの催促と合わせて数える）、[`llm_gateway::THINKING_EXHAUSTED_MARK`] も
/// 含めて次の生成の effort を下げる。指示は「ツールの途中ならすぐ呼ぶ・本文の途中なら切れた
/// 位置から続ける」の両方を書く。
pub(crate) fn thinking_exhausted_after_text(
    messages: &[LlmMessage],
    max_tokens: Option<u32>,
) -> Option<LlmMessage> {
    let continued = messages.iter().filter(|m| is_continuation_nudge(m)).count();
    let tried: usize = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(block_text)
        .map(|t| t.matches(llm_gateway::THINKING_EXHAUSTED_MARK).count())
        .sum();
    if continued >= MAX_TEXT_CONTINUATIONS || tried >= MAX_THINKING_RETRIES {
        return None;
    }
    let limit = max_tokens.map_or_else(String::new, |n| format!("（{n} トークン）"));
    Some(LlmMessage::text(
        llm_gateway::Role::User,
        format!(
            "{} {} 直前の応答は思考に出力上限{limit}の大半を使い、本文が短いまま切れました\
             （利用者の発話ではなくシステムからの連絡です。何も実行していません）。\
             思考は短く切り上げてください。ツールを呼ぶ前の説明だったなら、書いた本文は繰り返さず\
             次のツール呼び出しをすぐに出してください（1 回の呼び出しで書く量は小さく分ける）。\
             回答の本文の途中だったなら、切れた位置の直後から続きを書いてください\
             （前置き・繰り返しは書かない。文・表の行・```ブロックの途中ならその続きの文字から）。",
            llm_gateway::CONTINUATION_MARK,
            llm_gateway::THINKING_EXHAUSTED_MARK
        ),
    ))
}

/// 出力上限で呼び出しが切れた直後のステップに使う設定（出力上限を 2 倍・窓の残りまで）。
/// 引き上げられない（切れていない・窓を知らない・残りが無い）なら `None`。
///
/// 実測（Claude Haiku 5.5・refactor-xl・32k）: 1 応答に編集を 4〜5 件と思考を書いて 4096 トークンで
/// 末尾が切れ、「小さく分けて」と返しても同じ形で 3 回続けて切れ、ループ検出で止まった。切れた
/// 直後の 1 ステップだけ上限を上げる（Claude Code の出力上限の引き上げと同じ考え方）。
pub(crate) fn boosted_options(
    opts: &crate::profile::AgentOptions,
    messages: &[LlmMessage],
    limits: &crate::context::ContextLimits,
    gauge: &crate::context::ContextGauge,
) -> Option<crate::profile::AgentOptions> {
    let truncated = messages.last().is_some_and(|m| {
        m.content.iter().any(
            |b| matches!(b, Block::ToolResult { content, .. } if content.starts_with(TRUNCATED_CALL_MARK)),
        )
    });
    if !opts.aids.repair_calls || !truncated {
        return None;
    }
    let base = opts.max_tokens?;
    let window = limits.window?;
    // 窓の残り（実トークン）。見積りの誤差ぶん余白を取る。
    let room = (window.effective + base as usize).saturating_sub(gauge.tokens(messages) + 512);
    let boosted = base
        .saturating_mul(2)
        .min(u32::try_from(room).unwrap_or(u32::MAX));
    (boosted > base).then(|| crate::profile::AgentOptions {
        max_tokens: Some(boosted),
        ..opts.clone()
    })
}

/// 次の生成が「思考だけで上限に達した」直後の書き直しか（履歴の最後に催促がある）。
pub(crate) fn thinking_was_exhausted(messages: &[LlmMessage]) -> bool {
    messages.last().is_some_and(|m| {
        m.content
            .iter()
            .filter_map(block_text)
            .any(|t| t.contains(llm_gateway::THINKING_EXHAUSTED_MARK))
    })
}

fn block_text(b: &Block) -> Option<&str> {
    match b {
        Block::ToolResult { content, .. } => Some(content),
        Block::Text { text } => Some(text),
        _ => None,
    }
}

/// 自律版の作業の作法（小さいモデル向け・docs/agent/small-model-harness.md）。
///
/// 実測で見えた崩れに対応する: 呼び出しを予告だけして終える、古い結果が畳まれ・要約されて
/// 値を失う、数十個の数値を暗算でまとめて誤る、日付の差を数え違える、変わらない呼び出しを
/// 繰り返す、確かめずに完了と言う。
pub(crate) const DISCIPLINE_PROMPT: &str = "\n\n## 作業の作法\n\
- ツールを使うと書いたら、その応答で実際に呼ぶ。予告だけで応答を終えない。\n\
- 長いツール結果から後で使う値（数値・ID・パス・判断の根拠）は、読んだ直後に本文へ短く書き留める。\
古い結果は文脈を空けるために省かれ、要約に置き換わることがある。思考（考え）に書いた内容は\
次の応答では見えないので、残す値は必ず本文に書く。\n\
- 多くの数値の合計・比較は、暗算で一度に済ませず、項目ごとに途中の値を書きながら計算する。\
日付の差も、月ごとの日数を数えて確かめる。\n\
- 同じ呼び出しを同じ引数で繰り返しても結果は変わらない。失敗や停滞したら、原因を調べてやり方を変える。\n\
- 完了と報告する前に、依頼の項目をすべて満たしたかを結果で確かめる。";

/// 未完了の計画を確かめさせる注記の目印（1 run で 1 度だけ・冪等判定）。
const UNFINISHED_PLAN_MARK: &str = "［自動確認: 計画に未完了の項目が残っています］";

/// 計画に未完了（todo / doing）が残ったまま本文で終えたら、利用者発話として 1 度だけ確かめさせる。
/// 仕込んだら `true`（ループは続ける）。
///
/// 実測（Gemma 4 31B）: 集計の表まで本文に書いたところで終え、依頼された `submit_answer` を
/// 呼ばなかった。計画を更新しないまま正しく終えるモデルも多いので、「終わっているなら最終回答を
/// 書くだけでよい」と明記し、2 度目は止める（余分な手は 1 ステップに留める）。
pub(crate) fn nudge_unfinished_plan(
    messages: &mut Vec<LlmMessage>,
    plan: &crate::plan::Plan,
) -> bool {
    use crate::plan::SubtaskStatus;
    let open: Vec<&str> = plan
        .subtasks
        .iter()
        .filter(|t| matches!(t.status, SubtaskStatus::Todo | SubtaskStatus::Doing))
        .map(|t| t.title.as_str())
        .collect();
    let already = messages.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, Block::Text { text } if text.starts_with(UNFINISHED_PLAN_MARK)))
    });
    if open.is_empty() || already {
        return false;
    }
    messages.push(LlmMessage::text(
        llm_gateway::Role::User,
        format!(
            "{UNFINISHED_PLAN_MARK}\n未完了: {}\n依頼された作業（提出・保存・処理などの操作を含む）で\
             まだ実行していないものがあれば、今すぐツールで実行してください。すべて終わっているなら、\
             計画は更新せずに最終回答だけを書いてください。",
            open.join(" / ")
        ),
    ));
    true
}

/// 最後に呼ぶべきツール（[`crate::profile::AgentOptions::final_tool`]）を呼ばずに本文で終えたときの目印。
const MISSING_FINAL_MARK: &str = "［自動確認: 最後の操作がまだです］";

/// 最後に呼ぶべきツールを成功させないまま本文で終えたら、1 度だけ呼ぶよう促す。仕込んだら `true`。
///
/// 実測（Gemma 4 31B・Claude Haiku 5.5）: 答えを本文に書いて終え、依頼された提出ツールを
/// 呼ばなかった。計画を使わない run では [`nudge_unfinished_plan`] が効かないので、ツール名で見る。
pub(crate) fn nudge_missing_final_tool(messages: &mut Vec<LlmMessage>, final_tool: &str) -> bool {
    let mut ids: Vec<&str> = Vec::new();
    for b in messages.iter().flat_map(|m| m.content.iter()) {
        if let Block::ToolUse { id, name, .. } = b {
            if name == final_tool {
                ids.push(id.as_str());
            }
        }
    }
    let succeeded = messages.iter().flat_map(|m| m.content.iter()).any(|b| {
        matches!(b, Block::ToolResult { tool_use_id, is_error: false, .. }
            if ids.contains(&tool_use_id.as_str()))
    });
    let already = messages.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, Block::Text { text } if text.starts_with(MISSING_FINAL_MARK)))
    });
    if succeeded || already {
        return false;
    }
    messages.push(LlmMessage::text(
        llm_gateway::Role::User,
        format!(
            "{MISSING_FINAL_MARK}\n依頼では、結果を `{final_tool}` ツールで提出することになっていますが、\
             まだ成功していません。本文に書いただけでは提出されません。作業が終わっているなら、今すぐ \
             `{final_tool}` を呼んでください。まだ終わっていないなら、残りの作業を続けてから呼んでください。"
        ),
    ));
    true
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

/// 揃ったツール呼び出しがあるのに停止理由が `ToolUse` 以外なら、ツール呼び出しとして扱う。
///
/// 実測（プレビュー・委譲 6 件を一度に出した応答）: 呼び出しは全部届いたのに停止理由が
/// 「自然終了」で、ループは呼び出しを実行せずに完了扱いで畳んだ（10 秒・本文 1 行で run が終わり、
/// 委譲カードが出たまま止まる）。プロバイダ・中継（OpenAI 互換のゲートウェイ等）によって
/// 停止理由の付け方が揃わないので、ここで正規化する。出力上限（`MaxTokens`）は引数が切れている
/// かもしれないので、**全部の引数が揃っている**ときだけ昇格させる。
///
/// 出力上限で切れた呼び出しは、アダプタが解釈できない引数を `{}` にして渡す。逆に、中身のある
/// 引数は最後まで届いている（実測: 完結した SQL の csv.query 2 件が「切れた」扱いで捨てられ、
/// 同じ呼び出しを 4 ステップ繰り返した）。1 件でも `{}` があれば従来どおり切れた経路で扱う。
///
/// 自然終了・その他でも同じ条件を課す。OpenAI 互換のストリームが終端理由なしで切れたとき、
/// アダプタは未完の引数を `{}` にして呼び出しを渡す（`providers/openai.rs` の `drain_stops`）。
/// 中身のある JSON オブジェクトとして解析できた引数は閉じ括弧まで届いている（#1301 のレビュー）。
pub(crate) fn settle_stop(stop: StopReason, has_calls: bool, all_args_present: bool) -> StopReason {
    match stop {
        StopReason::EndTurn | StopReason::Other | StopReason::MaxTokens
            if has_calls && all_args_present =>
        {
            StopReason::ToolUse
        }
        other => other,
    }
}

/// 引数に中身がある（出力上限で切れて `{}` になっていない）。
pub(crate) fn has_args(input: &serde_json::Value) -> bool {
    input.as_object().is_some_and(|o| !o.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use llm_gateway::Role as LlmRole;

    #[test]
    fn output_limit_is_raised_only_right_after_a_truncated_call() {
        let mut opts = crate::profile::AgentOptions::autonomous(10, None, 1_000_000, 1_000_000);
        opts.max_tokens = Some(4_096);
        let limits = crate::context::ContextLimits::resolve(24_000, 6, Some(32_768), Some(4_096));
        let gauge = crate::context::ContextGauge::with_overhead(0);
        let cut = vec![LlmMessage {
            role: LlmRole::Tool,
            content: vec![Block::tool_result(
                "a",
                truncated_call_message("edit_file", Some(4_096)),
                true,
            )],
        }];
        let boosted = boosted_options(&opts, &cut, &limits, &gauge).unwrap();
        assert_eq!(boosted.max_tokens, Some(8_192));
        // 切れていなければ引き上げない。
        let ok = vec![LlmMessage {
            role: LlmRole::Tool,
            content: vec![Block::tool_result("a", "ok", false)],
        }];
        assert!(boosted_options(&opts, &ok, &limits, &gauge).is_none());
        // 窓の残りまでに抑える（残りが少なければ引き上げない）。
        let mut busy = cut.clone();
        busy.insert(0, LlmMessage::text(LlmRole::User, "字".repeat(26_000)));
        let partial = boosted_options(&opts, &busy, &limits, &gauge).unwrap();
        assert!(partial.max_tokens.unwrap() > 4_096 && partial.max_tokens.unwrap() < 8_192);
        let mut full = cut.clone();
        full.insert(0, LlmMessage::text(LlmRole::User, "字".repeat(30_000)));
        assert!(boosted_options(&opts, &full, &limits, &gauge).is_none());
        // 窓を知らないモデルでは引き上げない。
        let unknown = crate::context::ContextLimits::resolve(24_000, 6, None, Some(4_096));
        assert!(boosted_options(&opts, &cut, &unknown, &gauge).is_none());
    }

    #[test]
    fn missing_final_tool_is_nudged_once_until_it_succeeds() {
        let call = |id: &str| LlmMessage {
            role: LlmRole::Assistant,
            content: vec![Block::ToolUse {
                id: id.into(),
                name: "submit_answer".into(),
                input: serde_json::json!({}),
            }],
        };
        let result = |id: &str, err: bool| LlmMessage {
            role: LlmRole::Tool,
            content: vec![Block::tool_result(id, "r", err)],
        };
        // 失敗した提出だけでは、まだ提出していない。
        let mut msgs = vec![
            LlmMessage::text(LlmRole::User, "集計して submit_answer で提出"),
            call("a"),
            result("a", true),
            LlmMessage::text(LlmRole::Assistant, "答えは 42 です"),
        ];
        assert!(nudge_missing_final_tool(&mut msgs, "submit_answer"));
        assert!(matches!(&msgs.last().unwrap().content[0],
            Block::Text { text } if text.contains("`submit_answer` を呼んで")));
        // 2 度目は促さない。
        assert!(!nudge_missing_final_tool(&mut msgs, "submit_answer"));
        // 成功していれば促さない。
        let mut done = vec![call("b"), result("b", false)];
        assert!(!nudge_missing_final_tool(&mut done, "submit_answer"));
    }

    #[test]
    fn unfinished_plan_is_checked_only_once() {
        let mut plan = crate::plan::Plan::default();
        plan.revise(vec![
            crate::plan::SubtaskInput {
                title: "読む".into(),
                status: Some("done".into()),
            },
            crate::plan::SubtaskInput {
                title: "提出する".into(),
                status: Some("todo".into()),
            },
        ]);
        let mut messages = vec![LlmMessage::text(LlmRole::User, "依頼")];
        assert!(nudge_unfinished_plan(&mut messages, &plan));
        let last = format!("{:?}", messages.last().unwrap());
        assert!(last.contains("提出する") && !last.contains("読む /"));
        // 2 度目は確かめない（余分な手は 1 ステップに留める）。
        assert!(!nudge_unfinished_plan(&mut messages, &plan));
        // 未完了が無ければ何もしない。
        let mut fresh = vec![LlmMessage::text(LlmRole::User, "依頼")];
        assert!(!nudge_unfinished_plan(
            &mut fresh,
            &crate::plan::Plan::default()
        ));
    }

    #[test]
    fn complete_calls_with_a_non_tool_stop_are_run() {
        use StopReason::{EndTurn, MaxTokens, Other, ToolUse};
        assert_eq!(settle_stop(EndTurn, true, true), ToolUse);
        assert_eq!(settle_stop(Other, true, true), ToolUse);
        // 終端理由なしで切れた OpenAI 互換のストリーム（`{}` が混じる）は実行しない。
        assert_eq!(settle_stop(EndTurn, true, false), EndTurn);
        assert_eq!(settle_stop(Other, true, false), Other);
        // 呼び出しが無ければ、そのまま（完了・その他・出力上限）。
        assert_eq!(settle_stop(EndTurn, false, true), EndTurn);
        assert_eq!(settle_stop(Other, false, true), Other);
        assert_eq!(settle_stop(MaxTokens, false, true), MaxTokens);
        // 出力上限: 引数が全部揃っていれば実行し、`{}`（切れた）が混じれば切れた経路のまま。
        assert_eq!(settle_stop(MaxTokens, true, true), ToolUse);
        assert_eq!(settle_stop(MaxTokens, true, false), MaxTokens);
        assert_eq!(settle_stop(ToolUse, true, false), ToolUse);
    }

    #[test]
    fn empty_args_mean_a_cut_call() {
        assert!(has_args(&serde_json::json!({"sql": "SELECT 1"})));
        assert!(!has_args(&serde_json::json!({})));
        assert!(!has_args(&serde_json::json!(null)));
    }

    /// 思考だけの応答への催促は、最後のメッセージ（ツール結果か user 発話）へ添え、上限回数で打ち切る。
    #[test]
    fn thinking_exhausted_nudge_goes_to_the_last_message_and_is_bounded() {
        // 1 手目から思考だけ: 添える先は user 発話。
        let mut messages = vec![LlmMessage::text(LlmRole::User, "集計して")];
        assert!(!thinking_was_exhausted(&messages));
        assert!(nudge_thinking_exhausted(&mut messages, Some(8192)));
        assert!(thinking_was_exhausted(&messages));
        assert_eq!(messages.len(), 1, "user の連続を作らない");
        // ツール結果の後: 添える先はツール結果。
        messages.push(LlmMessage::text(LlmRole::Assistant, "調べます"));
        messages.push(LlmMessage {
            role: LlmRole::Tool,
            content: vec![Block::tool_result(
                "c1".to_string(),
                "ok".to_string(),
                false,
            )],
        });
        assert!(
            !thinking_was_exhausted(&messages),
            "催促は直前の 1 回だけを見る"
        );
        assert!(nudge_thinking_exhausted(&mut messages, Some(8192)));
        assert!(
            matches!(&messages[2].content[0], Block::ToolResult { content, .. }
                if content.starts_with("ok") && content.contains("8192 トークン"))
        );
        for _ in 2..MAX_THINKING_RETRIES {
            assert!(nudge_thinking_exhausted(&mut messages, Some(8192)));
        }
        assert!(
            !nudge_thinking_exhausted(&mut messages, Some(8192)),
            "上限で打ち切る"
        );
    }

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

    #[test]
    fn output_mostly_hidden_compares_visible_text_with_the_output() {
        assert!(output_mostly_hidden("9〜14枚目を追記します。", 8192));
        assert!(
            !output_mostly_hidden(&"表の行".repeat(2000), 8192),
            "長い本文の途中切れ"
        );
        assert!(
            !output_mostly_hidden("短い", 0),
            "出力量が届かなければ判定しない"
        );
    }

    #[test]
    fn thinking_exhausted_after_text_is_a_user_turn_and_shares_the_bound() {
        let mut messages = vec![LlmMessage::text(LlmRole::User, "スライドを作って")];
        for _ in 0..MAX_THINKING_RETRIES {
            messages.push(LlmMessage::text(LlmRole::Assistant, "追記します。"));
            let nudge = thinking_exhausted_after_text(&messages, Some(8192)).unwrap();
            assert_eq!(nudge.role, LlmRole::User);
            assert!(
                thinking_was_exhausted(std::slice::from_ref(&nudge)),
                "effort を下げる目印"
            );
            assert!(
                is_text_continuation(std::slice::from_ref(&nudge)),
                "本文の続きとして連結する"
            );
            let Block::Text { text } = &nudge.content[0] else {
                panic!("本文ブロック: {nudge:?}");
            };
            assert!(text.contains("8192"), "{text}");
            messages.push(nudge);
        }
        assert!(thinking_exhausted_after_text(&messages, Some(8192)).is_none());
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
            let blocks = truncated_call_results(&messages, &ids, Some(8192)).unwrap();
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
            messages.push(LlmMessage {
                role: LlmRole::Tool,
                content: blocks,
            });
        }
        assert!(
            truncated_call_results(&messages, &ids, Some(8192)).is_none(),
            "上限回数を超えたら書き直させない"
        );
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
