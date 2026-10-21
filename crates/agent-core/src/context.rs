//! コンテキスト管理・圧縮（Task 5.3）。
//!
//! 長ホライズンでコンテキスト長を超えないよう、**古いツール結果を剪定**する。直近 `keep_recent`
//! メッセージの観測は無傷で残し、それ以前の**大きな `tool_result`**だけを短い要約プレースホルダに
//! 畳み込む（窓の非常閾値を超えたときだけは、最後のツール結果以外を全て畳む）。重要事実・計画・成果物参照は agent-core 外（外部メモリ＝ワークスペースの `plan.md`/
//! `notes.md`・W2）に逃がす前提で、ここでは**冗長なツール出力の畳み込み**に徹する。
//!
//! 閾値はモデルのコンテキスト窓が分かればそれに合わせる（[`ContextLimits`]・#529）。窓に対する
//! 比率（省略 0.6・非常 0.85）は arXiv:2609.20804 の段階的管理に倣う。同論文では管理の効果の
//! 大半が**窓あふれ失敗の防止**で、窓が狭いほど効く。窓判定の見積りは、プロバイダが報告した
//! prompt トークンで補正する（[`ContextGauge`]・system/ツール定義の分を拾う）。
//!
//! 剪定は**純粋関数**（ループは [`ContextLimits::apply`]・固定閾値だけなら [`prune_history`]）。tool_use / text / thinking は保持し（呼び出しと結果の
//! 対応を壊さない）、`tool_result` の本文のみ縮める。引用は別イベントで既に外部化済みのため
//! 出所追跡は壊れない（Task 2.7 整合・PIT-5）。

use llm_gateway::{Block, Message, Role, ToolDef};

/// トークン概算（1 トークン ≒ 4 バイトの粗い近似・予算/剪定のトリガにのみ使う）。
#[must_use]
pub fn estimate_tokens(messages: &[Message]) -> usize {
    let bytes: usize = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .map(block_bytes)
        .sum();
    bytes / 4
}

/// prompt トークンの見積り（窓判定用）。ASCII は 4 バイト ≒ 1 トークン、それ以外は 1 文字 ≒
/// 1 トークンで数える。
///
/// bytes/4 は日本語（3 バイト/字）を 1 字 0.75 トークンと数え、実トークナイザより小さく出る。
/// 補正（[`ContextGauge`]）は直前ステップまでの差しか拾えないため、**このステップで入った大きな
/// 日本語のツール結果**を過小評価して非常閾値を素通りする。溢れの判定は過小評価側に倒さない。
#[must_use]
pub fn estimate_prompt_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .flat_map(|m| m.content.iter())
        .map(|b| match b {
            Block::Text { text } | Block::Thinking { text } => text_tokens(text),
            Block::ToolResult { content, .. } => text_tokens(content),
            Block::ToolUse { name, input, .. } => {
                text_tokens(name) + text_tokens(&input.to_string())
            }
            // 暗号化された推論もプロバイダ側では入力トークンとして数えられる。
            Block::Reasoning { item, .. } => text_tokens(&item.to_string()),
        })
        .sum()
}

/// system プロンプトとツール定義（毎回送る固定分）のトークン見積り。
///
/// 実測の補正が無いうち（新規開始・旧チェックポイントからの再開の最初のステップ）に使う。
/// 窓の小さいモデルでは、履歴が閾値未満でも固定分を足すと溢れうる。溢れて拒否されると実測
/// usage が得られず、以後の補正にも到達できない。
///
/// `tools` には**文脈に載る定義**だけを渡す（遅延ロードで未読込のものは数えない・
/// [`llm_gateway::tool_loading::context_tools`]）。
#[must_use]
pub fn estimate_static_tokens<'a>(
    system: Option<&str>,
    tools: impl IntoIterator<Item = &'a ToolDef>,
) -> usize {
    let tools: usize = tools
        .into_iter()
        .map(|t| {
            text_tokens(&t.name)
                + text_tokens(&t.description)
                + text_tokens(&t.input_schema.to_string())
        })
        .sum();
    system.map_or(0, text_tokens) + tools
}

/// ASCII は 4 バイト ≒ 1 トークン、それ以外は 1 文字 ≒ 1 トークン。
pub(crate) fn text_tokens(s: &str) -> usize {
    let ascii = s.bytes().filter(u8::is_ascii).count();
    let non_ascii = s.chars().filter(|c| !c.is_ascii()).count();
    ascii / 4 + non_ascii
}

fn block_bytes(b: &Block) -> usize {
    match b {
        Block::Text { text } | Block::Thinking { text } => text.len(),
        Block::ToolResult { content, .. } => content.len(),
        Block::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
        // 暗号化された推論もプロバイダ側では入力トークンとして数えられる。
        Block::Reasoning { item, .. } => item.to_string().len(),
    }
}

/// 剪定後に残す 1 ツール結果の先頭バイト数（要約プレースホルダの頭）。
const KEEP_HEAD_BYTES: usize = 400;
/// この長さ以下の tool_result は畳み込まない（畳み込みが割に合わない）。
const MIN_FOLD_BYTES: usize = 800;

/// 履歴を剪定する。**推定トークンが `soft_limit` を超える場合のみ**、直近 `keep_recent`
/// メッセージより古い大きな `tool_result` 本文を短縮する。戻り値は「剪定したか」。
///
/// - tool_use / text / thinking は縮めない（計画・意思決定の連続性を保つ）。
/// - 短い tool_result は縮めない（`MIN_FOLD_BYTES` 未満）。
/// - 既に畳み込み済みのものは再短縮しない（冪等）。
/// - 古い assistant メッセージのプロバイダ固有の推論アイテム（暗号化された推論）は落とす。送り返す
///   のは続きのツールのターンのためで、古いものは役目を終えている（溜まると剪定が効かなくなる・#542）。
pub fn prune_history(
    messages: &mut [Message],
    soft_limit_tokens: usize,
    keep_recent: usize,
) -> bool {
    if estimate_tokens(messages) <= soft_limit_tokens {
        return false;
    }
    fold_older_than(messages, keep_recent)
}

/// 直近 `keep_recent` メッセージより古い大きな `tool_result` 本文を畳む。戻り値は「畳んだか」。
fn fold_older_than(messages: &mut [Message], keep_recent: usize) -> bool {
    let cutoff = messages.len().saturating_sub(keep_recent);
    let dropped = drop_old_reasoning(messages, cutoff);
    fold_results(messages, cutoff, None) | dropped
}

/// `messages[..cutoff]` の assistant メッセージから、プロバイダ固有の推論アイテムを落とす
/// （推論以外のブロックが残るメッセージだけ）。戻り値は「落としたか」。
///
/// 送り返すのは続きのツールのターンのためで、古いものは役目を終えている（溜まると剪定が
/// 効かなくなる・#542）。非常閾値の畳み込みでは呼ばない（直近の推論は次のターンに要る）。
fn drop_old_reasoning(messages: &mut [Message], cutoff: usize) -> bool {
    let mut dropped = false;
    for msg in messages.iter_mut().take(cutoff) {
        if msg.role != Role::Assistant
            || msg
                .content
                .iter()
                .all(|b| matches!(b, Block::Reasoning { .. }))
        {
            continue;
        }
        let before = msg.content.len();
        msg.content
            .retain(|b| !matches!(b, Block::Reasoning { .. }));
        dropped |= msg.content.len() != before;
    }
    dropped
}

/// `messages[..cutoff]` の大きな `tool_result` 本文を畳む（`protect` の位置＝(メッセージ, ブロック)
/// は畳まない）。戻り値は「畳んだか」。
fn fold_results(messages: &mut [Message], cutoff: usize, protect: Option<(usize, usize)>) -> bool {
    let mut pruned = false;
    for (mi, msg) in messages.iter_mut().enumerate().take(cutoff) {
        if msg.role != Role::Tool {
            continue;
        }
        for (bi, block) in msg.content.iter_mut().enumerate() {
            if Some((mi, bi)) == protect {
                continue;
            }
            if let Block::ToolResult {
                content, is_error, ..
            } = block
            {
                if content.len() >= MIN_FOLD_BYTES && !is_folded(content) {
                    *content = fold(content, *is_error);
                    pruned = true;
                }
            }
        }
    }
    pruned
}

/// 畳み込みマーカー（再短縮の冪等判定に使う）。
const FOLD_MARK: &str = "…［古い出力を要約のため省略";

pub(crate) fn is_folded(s: &str) -> bool {
    s.contains(FOLD_MARK)
}

/// tool_result 本文を「先頭 KEEP_HEAD_BYTES ＋省略注記」に畳む（UTF-8 境界を守る）。
fn fold(content: &str, is_error: bool) -> String {
    let mut end = KEEP_HEAD_BYTES.min(content.len());
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    let omitted = content.len().saturating_sub(end);
    let tag = if is_error {
        "エラー出力"
    } else {
        "出力"
    };
    format!("{}{FOLD_MARK}: {tag} {omitted} バイト］", &content[..end])
}

/// 省略（畳み込み）を始める閾値の、有効窓に対する千分率。
const SOFT_PERMILLE: usize = 600;
/// 非常時の閾値（千分率）。超えたら最後のツール結果以外を全て畳む。
const HARD_PERMILLE: usize = 850;

/// **最後のツール結果ブロック**の位置（剪定で守る対象）。
///
/// 次の一手（あるいは「まとめて」等の追質問への回答）の根拠は直前の観測なので、通常の剪定でも
/// 非常時でも消さない（末尾がユーザー発話でも、その手前の結果を守る）。メッセージ単位ではなく
/// ブロック単位で守る: 並列実行の結果は 1 つの Tool メッセージにまとまるため、メッセージごと
/// 守ると大きな結果が複数同時に返ったときに畳めず、そのまま溢れる。
fn last_result_position(messages: &[Message]) -> Option<(usize, usize)> {
    messages.iter().enumerate().rev().find_map(|(mi, m)| {
        m.content
            .iter()
            .rposition(|b| matches!(b, Block::ToolResult { .. }))
            .map(|bi| (mi, bi))
    })
}

/// 1 run の剪定閾値（設定値とモデルの窓から決める）。
///
/// 判定は 2 系統ある。**設定値**（既定 24k）は従来どおり履歴の概算だけと比べる（この値は
/// その尺度で調整されてきたので、窓を知らないモデルの挙動は一切変えない）。**窓由来**の閾値は
/// system/ツール定義を含む実 prompt の見積り（[`ContextGauge`]）と比べる（溢れは prompt 全体で
/// 起きるため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextLimits {
    /// 履歴の概算がこれを超えたら畳む（0 で剪定そのものを無効化）。
    pub history_soft: usize,
    /// 窓由来の閾値（窓を知らなければ `None`）。
    pub window: Option<WindowLimits>,
    pub keep_recent: usize,
}

/// 有効窓（窓 − 1 応答の出力上限）に対する閾値。単位は prompt 全体のトークン。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowLimits {
    /// 超えたら `keep_recent` を残して畳む（有効窓の 0.6）。
    pub soft: usize,
    /// 超えたら最後のツール結果以外を全て畳む（有効窓の 0.85・溢れ直前の非常手段）。
    pub hard: usize,
}

impl ContextLimits {
    /// 設定値（`soft_limit`・0 で無効）とモデルの窓から閾値を決める。
    ///
    /// 窓は**閾値を下げる方向にだけ**効く。大きい窓のモデルで設定値より遅く畳むことはしない
    /// （毎ステップ再送する履歴が増えてコストが伸びる。それは評価を経て決める判断）。
    #[must_use]
    pub fn resolve(
        soft_limit: usize,
        keep_recent: usize,
        window_tokens: Option<u32>,
        max_output_tokens: Option<u32>,
    ) -> Self {
        let window = window_tokens
            .map(|w| w.saturating_sub(max_output_tokens.unwrap_or(0)) as usize)
            .filter(|&u| u > 0 && soft_limit > 0)
            .map(|u| WindowLimits {
                soft: u * SOFT_PERMILLE / 1000,
                hard: u * HARD_PERMILLE / 1000,
            });
        ContextLimits {
            history_soft: soft_limit,
            window,
            keep_recent,
        }
    }

    /// 履歴を閾値に収める。まず `keep_recent` を残して畳み、それでも非常閾値を超えるなら
    /// 最後のツール結果以外を全て畳む。戻り値は「畳んだか」。
    pub fn apply(&self, messages: &mut [Message], gauge: &ContextGauge) -> bool {
        if self.history_soft == 0 {
            return false;
        }
        let over_history = estimate_tokens(messages) > self.history_soft;
        let over_window = match self.window {
            Some(w) => gauge.tokens(messages) > w.soft,
            None => false,
        };
        if !over_history && !over_window {
            return false;
        }
        // 守る位置は畳む前に決める（`keep_recent = 0` でも最後の結果は残す）。窓を知らない
        // モデルでは保護しない（設定値だけの剪定は従来どおり・#529）。
        let protect = self.window.and_then(|_| last_result_position(messages));
        let cutoff = messages.len().saturating_sub(self.keep_recent);
        let mut pruned = drop_old_reasoning(messages, cutoff);
        pruned |= fold_results(messages, cutoff, protect);
        if self.window.is_some_and(|w| gauge.tokens(messages) > w.hard) {
            pruned |= fold_results(messages, messages.len(), protect);
        }
        pruned
    }
}

/// prompt 全体の実トークン数の見積り。履歴の見積り（[`estimate_prompt_tokens`]）に、プロバイダ
/// 実測との差（system プロンプト・ツール定義・トークナイザ差）を足す。差は直前ステップの実測で
/// 更新し、チェックポイントに載せて再開後も使う（再開直後のステップで差を落とさない）。
#[derive(Debug, Clone, Copy, Default)]
pub struct ContextGauge {
    overhead_tokens: usize,
    /// 差に含まれている（＝前回送った）ツール定義のトークン見積り。
    tool_tokens: usize,
}

impl ContextGauge {
    /// チェックポイントに保存した差から復元する。
    #[must_use]
    pub fn with_overhead(overhead_tokens: usize) -> Self {
        ContextGauge {
            overhead_tokens,
            tool_tokens: 0,
        }
    }

    /// 差に既に含まれているツール定義の見積り（以後の増分だけを [`Self::observe_tools`] で足す）。
    #[must_use]
    pub fn with_tool_tokens(mut self, tool_tokens: usize) -> Self {
        self.tool_tokens = tool_tokens;
        self
    }

    /// 次に送るツール定義の見積りを知らせる。
    ///
    /// `tool_search` が遅延ツールを読み込むと、次のリクエストから定義が文脈に増える。差は前回の
    /// 実測から求めたものなので、その増分を足しておかないと窓の判定が足りなくなる（溢れて
    /// 拒否されうる）。次の [`Self::calibrate`] で実測に置き換わる。
    pub fn observe_tools(&mut self, tool_tokens: usize) {
        if tool_tokens > self.tool_tokens {
            self.overhead_tokens = self
                .overhead_tokens
                .saturating_add(tool_tokens - self.tool_tokens);
        }
        self.tool_tokens = tool_tokens;
    }

    /// 現在の差（チェックポイントへ保存する）。
    #[must_use]
    pub fn overhead_tokens(&self) -> usize {
        self.overhead_tokens
    }

    /// 現在の履歴を送ったときの prompt トークンの見積り。
    #[must_use]
    pub fn tokens(&self, messages: &[Message]) -> usize {
        estimate_prompt_tokens(messages).saturating_add(self.overhead_tokens)
    }

    /// 送った履歴の見積り（`sent_estimate`＝[`estimate_prompt_tokens`]）と、プロバイダが報告した
    /// prompt トークンで差を更新する。報告が無い（0）プロバイダでは補正しない。
    pub fn calibrate(&mut self, sent_estimate: usize, reported_prompt_tokens: u64) {
        if reported_prompt_tokens > 0 {
            let reported = usize::try_from(reported_prompt_tokens).unwrap_or(usize::MAX);
            self.overhead_tokens = reported.saturating_sub(sent_estimate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_gateway::Block;

    fn tool_result(content: &str, is_error: bool) -> Message {
        Message {
            role: Role::Tool,
            content: vec![Block::tool_result("t1", content, is_error)],
        }
    }

    #[test]
    fn no_prune_under_limit() {
        let mut msgs = vec![tool_result(&"x".repeat(2000), false)];
        // soft_limit を高くすれば剪定しない。
        assert!(!prune_history(&mut msgs, 100_000, 0));
    }

    #[test]
    fn prunes_old_large_tool_results_only() {
        let big = "y".repeat(4000);
        let mut msgs = vec![
            tool_result(&big, false), // 古い→畳む
            tool_result(&big, false), // 直近1件→残す
        ];
        let pruned = prune_history(&mut msgs, 1, 1);
        assert!(pruned);
        // 先頭は畳まれ、末尾は無傷。
        if let Block::ToolResult { content, .. } = &msgs[0].content[0] {
            assert!(is_folded(content));
            assert!(content.len() < big.len());
        } else {
            panic!("expected tool_result");
        }
        if let Block::ToolResult { content, .. } = &msgs[1].content[0] {
            assert_eq!(content.len(), big.len());
        }
    }

    #[test]
    fn does_not_touch_text_or_tool_use() {
        let mut msgs = vec![
            Message::text(Role::Assistant, "z".repeat(5000)),
            Message {
                role: Role::Assistant,
                content: vec![Block::ToolUse {
                    id: "i".into(),
                    name: "shell".into(),
                    input: serde_json::json!({"cmd": "a".repeat(5000)}),
                }],
            },
        ];
        let before = msgs.clone();
        prune_history(&mut msgs, 1, 0);
        assert_eq!(msgs, before); // text/tool_use は縮めない
    }

    #[test]
    fn fold_is_idempotent() {
        let big = "e".repeat(4000);
        let mut msgs = vec![tool_result(&big, true)];
        assert!(prune_history(&mut msgs, 1, 0));
        let once = msgs.clone();
        // 2 回目は既に畳み込み済みなので変化しない。
        assert!(!prune_history(&mut msgs, 1, 0));
        assert_eq!(msgs, once);
    }

    #[test]
    fn short_results_are_kept() {
        let mut msgs = vec![tool_result("short output", false)];
        assert!(!prune_history(&mut msgs, 1, 0));
    }

    #[test]
    fn fold_respects_utf8_boundary() {
        let big = "あ".repeat(2000); // 3 bytes/char・KEEP_HEAD_BYTES 境界が文字途中
        let mut msgs = vec![tool_result(&big, false)];
        assert!(prune_history(&mut msgs, 1, 0));
        // パニックせず、頭は「あ」で始まる。
        if let Block::ToolResult { content, .. } = &msgs[0].content[0] {
            assert!(content.starts_with('あ'));
        }
    }

    #[test]
    fn prune_drops_old_reasoning_items_but_keeps_recent_ones() {
        let reasoning = || Block::Reasoning {
            provider: "openai_responses".into(),
            item: serde_json::json!({ "encrypted_content": "x".repeat(8_000) }),
        };
        let assistant = |blocks: Vec<Block>| Message {
            role: Role::Assistant,
            content: blocks,
        };
        let call = || Block::ToolUse {
            id: "c".into(),
            name: "t".into(),
            input: serde_json::json!({}),
        };
        let has_reasoning = |m: &Message| {
            m.content
                .iter()
                .any(|b| matches!(b, Block::Reasoning { .. }))
        };
        let fresh = || {
            vec![
                assistant(vec![reasoning(), call()]),
                assistant(vec![reasoning(), call()]),
            ]
        };

        let mut msgs = fresh();
        assert!(prune_history(&mut msgs, 100, 1));
        assert!(!has_reasoning(&msgs[0]));
        assert!(msgs[0]
            .content
            .iter()
            .any(|b| matches!(b, Block::ToolUse { .. })));
        // 直近（keep_recent）の推論は残す（続きのツールのターンで送り返す）。
        assert!(has_reasoning(&msgs[1]));

        // エージェントの剪定（ContextLimits）でも同じ（窓を知る・非常閾値を超える場合も直近は残す）。
        for window in [None, Some(3_000)] {
            let mut msgs = fresh();
            let l = ContextLimits::resolve(100, 1, window, None);
            assert!(l.apply(&mut msgs, &ContextGauge::default()));
            assert!(!has_reasoning(&msgs[0]), "window={window:?}");
            assert!(has_reasoning(&msgs[1]), "window={window:?}");
        }
    }

    fn limits(soft: usize, window: Option<u32>, max_out: Option<u32>) -> ContextLimits {
        ContextLimits::resolve(soft, 6, window, max_out)
    }

    #[test]
    fn window_derives_soft_and_hard_from_usable_window() {
        // 32k 窓・出力 4k → 有効 28k、soft 16.8k、hard 23.8k。
        assert_eq!(
            limits(24_000, Some(32_000), Some(4_000)).window,
            Some(WindowLimits {
                soft: 16_800,
                hard: 23_800
            })
        );
        // 窓が出力上限以下（設定ミス）は「知らない」扱い。
        assert_eq!(limits(24_000, Some(4_000), Some(4_000)).window, None);
        assert_eq!(limits(24_000, None, Some(4_000)).window, None);
    }

    #[test]
    fn large_window_never_delays_configured_pruning() {
        // 200k 窓でも、履歴が設定値（24k）を超えれば従来どおり畳む。
        let l = limits(24_000, Some(200_000), Some(4_000));
        let mut msgs: Vec<Message> = (0..8)
            .map(|_| tool_result(&"q".repeat(16_000), false))
            .collect();
        assert!(l.apply(&mut msgs, &ContextGauge::default()));
    }

    #[test]
    fn disabled_ignores_window() {
        // 0 は明示的な無効化。窓があっても剪定しない。
        let off = limits(0, Some(32_000), None);
        assert_eq!(off.window, None);
        let mut msgs = vec![
            tool_result(&"x".repeat(400_000), false),
            tool_result("y", false),
        ];
        assert!(!off.apply(&mut msgs, &ContextGauge::default()));
    }

    #[test]
    fn hard_limit_folds_recent_results_except_the_latest() {
        let big = "z".repeat(40_000); // ≒ 10k トークン
        let mut msgs: Vec<Message> = (0..4).map(|_| tool_result(&big, false)).collect();
        // 窓 30k（出力なし）→ soft 18k・hard 25.5k。keep_recent=6 では何も畳めず 40k のまま。
        let l = ContextLimits::resolve(24_000, 6, Some(30_000), None);
        assert!(l.apply(&mut msgs, &ContextGauge::default()));
        // 非常閾値で直前の 1 件以外を畳む。
        let folded: Vec<bool> = msgs
            .iter()
            .map(|m| matches!(&m.content[0], Block::ToolResult { content, .. } if is_folded(content)))
            .collect();
        assert_eq!(folded, vec![true, true, true, false]);
    }

    #[test]
    fn hard_limit_keeps_the_last_tool_result_before_a_user_follow_up() {
        // 末尾がユーザー発話（「まとめて」）でも、その根拠である直前のツール結果は畳まない。
        let big = "z".repeat(40_000);
        let mut msgs: Vec<Message> = (0..3).map(|_| tool_result(&big, false)).collect();
        msgs.push(Message::text(Role::User, "まとめて"));
        let l = ContextLimits::resolve(24_000, 6, Some(30_000), None);
        assert!(l.apply(&mut msgs, &ContextGauge::default()));
        let folded: Vec<bool> = msgs
            .iter()
            .map(|m| matches!(&m.content[0], Block::ToolResult { content, .. } if is_folded(content)))
            .collect();
        assert_eq!(folded, vec![true, true, false, false]);
    }

    #[test]
    fn hard_limit_folds_parallel_results_in_the_same_message() {
        // 並列 3 件の大きな結果が 1 つの Tool メッセージに入って溢れかけたら、最後の 1 件以外を畳む。
        let big = "p".repeat(40_000);
        let blocks = (0..3)
            .map(|i| Block::tool_result(format!("t{i}"), big.clone(), false))
            .collect();
        let mut msgs = vec![Message {
            role: Role::Tool,
            content: blocks,
        }];
        let l = ContextLimits::resolve(24_000, 6, Some(30_000), None);
        assert!(l.apply(&mut msgs, &ContextGauge::default()));
        let folded: Vec<bool> = msgs[0]
            .content
            .iter()
            .map(|b| matches!(b, Block::ToolResult { content, .. } if is_folded(content)))
            .collect();
        assert_eq!(folded, vec![true, true, false]);
    }

    #[test]
    fn last_result_survives_even_with_keep_recent_zero() {
        let big = "k".repeat(40_000);
        let mut msgs: Vec<Message> = (0..3).map(|_| tool_result(&big, false)).collect();
        let l = ContextLimits::resolve(24_000, 0, Some(30_000), None);
        assert!(l.apply(&mut msgs, &ContextGauge::default()));
        let folded: Vec<bool> = msgs
            .iter()
            .map(|m| matches!(&m.content[0], Block::ToolResult { content, .. } if is_folded(content)))
            .collect();
        assert_eq!(folded, vec![true, true, false]);
    }

    #[test]
    fn without_window_the_last_result_is_not_protected() {
        // 窓を知らないモデルは従来どおり（keep_recent = 0 なら最後の結果も畳む）。
        let big = "k".repeat(40_000);
        let mut msgs: Vec<Message> = (0..2).map(|_| tool_result(&big, false)).collect();
        let l = ContextLimits::resolve(1_000, 0, None, None);
        assert!(l.apply(&mut msgs, &ContextGauge::default()));
        assert!(msgs.iter().all(
            |m| matches!(&m.content[0], Block::ToolResult { content, .. } if is_folded(content))
        ));
    }

    #[test]
    fn newly_loaded_tools_raise_the_estimate_until_calibrated() {
        let msgs = vec![tool_result("x", false)];
        let mut g = ContextGauge::with_overhead(1_000).with_tool_tokens(300);
        let base = g.tokens(&msgs);
        g.observe_tools(300); // 変化なし
        assert_eq!(g.tokens(&msgs), base);
        g.observe_tools(800); // 遅延ツールを読み込んだ
        assert_eq!(g.tokens(&msgs), base + 500);
        // 実測で置き換わる（読み込んだ定義込みの実測）。
        g.calibrate(estimate_prompt_tokens(&msgs), 2_000);
        assert_eq!(g.overhead_tokens(), 2_000 - estimate_prompt_tokens(&msgs));
    }

    #[test]
    fn tools_loaded_in_the_last_step_survive_into_the_checkpoint() {
        // 送信時 300 → 実測で補正 → このステップの結果で 800 に増えた。保存する差には増分が入り、
        // 再開時に履歴から求めた基準（800）と食い違わない。
        let msgs = vec![tool_result("x", false)];
        let mut g = ContextGauge::with_overhead(0).with_tool_tokens(300);
        g.calibrate(
            estimate_prompt_tokens(&msgs),
            1_300 + estimate_prompt_tokens(&msgs) as u64,
        );
        g.observe_tools(800);
        let saved = g.overhead_tokens();
        assert_eq!(saved, 1_800);
        let mut resumed = ContextGauge::with_overhead(saved).with_tool_tokens(800);
        resumed.observe_tools(800);
        assert_eq!(resumed.overhead_tokens(), 1_800);
    }

    #[test]
    fn static_estimate_counts_system_and_tool_definitions() {
        // name "web_fetch" 9 bytes → 2 / description 4 字 → 4 / schema "{}" → 0
        let tools = vec![ToolDef::new("web_fetch", "取得する", serde_json::json!({}))];
        assert_eq!(estimate_static_tokens(Some(&"s".repeat(400)), &tools), 106);
        assert_eq!(estimate_static_tokens(None, &[]), 0);
    }

    #[test]
    fn prompt_estimate_counts_non_ascii_per_char() {
        // ASCII は bytes/4、日本語は 1 字 1 トークン（bytes/4 だと 0.75 で過小）。
        let ascii = vec![tool_result(&"a".repeat(4_000), false)];
        assert_eq!(estimate_prompt_tokens(&ascii), 1_000);
        let ja = vec![tool_result(&"あ".repeat(1_000), false)];
        assert_eq!(estimate_tokens(&ja), 750);
        assert_eq!(estimate_prompt_tokens(&ja), 1_000);
    }

    #[test]
    fn gauge_overhead_roundtrips_for_resume() {
        let mut g = ContextGauge::default();
        g.calibrate(1_000, 7_000);
        let restored = ContextGauge::with_overhead(g.overhead_tokens());
        let msgs = vec![tool_result(&"x".repeat(4_000), false)];
        assert_eq!(restored.tokens(&msgs), g.tokens(&msgs));
    }

    #[test]
    fn gauge_adds_reported_overhead() {
        let msgs = vec![tool_result(&"x".repeat(4_000), false)]; // 概算 1000
        let mut g = ContextGauge::default();
        assert_eq!(g.tokens(&msgs), 1_000);
        // 実測 6000 ＝ system/ツール定義ぶん 5000 が上乗せされている。
        g.calibrate(1_000, 6_000);
        assert_eq!(g.tokens(&msgs), 6_000);
        // 報告なし（0）は補正を変えない。
        g.calibrate(1_000, 0);
        assert_eq!(g.tokens(&msgs), 6_000);
        // 概算の方が大きいときは差 0（過大評価側には寄せない）。
        g.calibrate(9_000, 6_000);
        assert_eq!(g.tokens(&msgs), 1_000);
    }

    #[test]
    fn overhead_counts_only_against_window_limits() {
        // 履歴の概算 2k。ツール定義ぶん 10k を足すと prompt は 12k。
        let msgs = vec![
            tool_result(&"x".repeat(8_000), false),
            tool_result("y", false),
        ];
        let mut g = ContextGauge::default();
        g.calibrate(0, 10_000);
        // 窓を知らなければ設定値（5k）は履歴の概算とだけ比べる → 畳まない（従来挙動）。
        let no_window = ContextLimits::resolve(5_000, 1, None, None);
        assert!(!no_window.apply(&mut msgs.clone(), &g));
        // 窓 16k → soft 9.6k。prompt 全体 12k が超えるので畳む。
        let windowed = ContextLimits::resolve(5_000, 1, Some(16_000), None);
        assert!(windowed.apply(&mut msgs.clone(), &g));
    }
}
