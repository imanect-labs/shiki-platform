//! コンテキスト管理・圧縮（Task 5.3）。
//!
//! 長ホライズンでコンテキスト長を超えないよう、**古いツール結果を剪定**する。直近 `keep_recent`
//! ステップの観測は無傷で残し、それ以前の**大きな `tool_result`**だけを短い要約プレースホルダに
//! 畳み込む。重要事実・計画・成果物参照は agent-core 外（外部メモリ＝ワークスペースの `plan.md`/
//! `notes.md`・W2）に逃がす前提で、ここでは**冗長なツール出力の畳み込み**に徹する。
//!
//! 閾値はモデルのコンテキスト窓が分かればそれに合わせる（[`ContextLimits`]・#529）。窓に対する
//! 比率（省略 0.6・非常 0.85）は arXiv:2609.20804 の段階的管理に倣う。同論文では管理の効果の
//! 大半が**窓あふれ失敗の防止**で、窓が狭いほど効く。トークン数は bytes/4 の概算を、プロバイダが
//! 報告した prompt トークンで補正する（[`ContextGauge`]・system/ツール定義の分を拾う）。
//!
//! 剪定は**純粋関数**（[`prune_history`]）。tool_use / text / thinking は保持し（呼び出しと結果の
//! 対応を壊さない）、`tool_result` の本文のみ縮める。引用は別イベントで既に外部化済みのため
//! 出所追跡は壊れない（Task 2.7 整合・PIT-5）。

use llm_gateway::{Block, Message, Role};

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

fn block_bytes(b: &Block) -> usize {
    match b {
        Block::Text { text } | Block::Thinking { text } => text.len(),
        Block::ToolResult { content, .. } => content.len(),
        Block::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
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
    let mut pruned = false;
    for msg in messages.iter_mut().take(cutoff) {
        if msg.role != Role::Tool {
            continue;
        }
        for block in &mut msg.content {
            if let Block::ToolResult {
                content, is_error, ..
            } = block
            {
                if content.len() < MIN_FOLD_BYTES || is_folded(content) {
                    continue;
                }
                *content = fold(content, *is_error);
                pruned = true;
            }
        }
    }
    pruned
}

/// 畳み込みマーカー（再短縮の冪等判定に使う）。
const FOLD_MARK: &str = "…［古い出力を要約のため省略";

fn is_folded(s: &str) -> bool {
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
/// 非常時の閾値（千分率）。超えたら直近の保護を最小にして畳む。
const HARD_PERMILLE: usize = 850;
/// 非常時にも無傷で残す直近メッセージ数（＝直前のツール結果。次の一手の根拠は消さない）。
const EMERGENCY_KEEP_RECENT: usize = 1;

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
    /// 超えたら直近の保護を最小にして畳む（有効窓の 0.85・溢れ直前の非常手段）。
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
    /// 直近の保護を最小にしてもう一度畳む。戻り値は「畳んだか」。
    pub fn apply(&self, messages: &mut [Message], gauge: &ContextGauge) -> bool {
        if self.history_soft == 0 {
            return false;
        }
        let over_history = estimate_tokens(messages) > self.history_soft;
        let over_window = self.window.is_some_and(|w| gauge.tokens(messages) > w.soft);
        if !over_history && !over_window {
            return false;
        }
        let mut pruned = fold_older_than(messages, self.keep_recent);
        if self.window.is_some_and(|w| gauge.tokens(messages) > w.hard) {
            pruned |= fold_older_than(messages, EMERGENCY_KEEP_RECENT);
        }
        pruned
    }
}

/// 履歴の実トークン数の見積り。bytes/4 の概算に、プロバイダ実測との差（system プロンプト・
/// ツール定義・トークナイザ差）を足す。差は直前ステップの実測で更新する。
#[derive(Debug, Clone, Copy, Default)]
pub struct ContextGauge {
    overhead_tokens: usize,
}

impl ContextGauge {
    /// 現在の履歴を送ったときの prompt トークンの見積り。
    #[must_use]
    pub fn tokens(&self, messages: &[Message]) -> usize {
        estimate_tokens(messages).saturating_add(self.overhead_tokens)
    }

    /// 送った履歴の概算（`sent_estimate`）と、プロバイダが報告した prompt トークンで差を更新する。
    /// 報告が無い（0）プロバイダでは補正しない（概算のみ＝従来どおり）。
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
            content: vec![Block::ToolResult {
                tool_use_id: "t1".into(),
                content: content.into(),
                is_error,
            }],
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
