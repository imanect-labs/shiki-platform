//! コンテキスト管理・圧縮（Task 5.3）。
//!
//! 長ホライズンでコンテキスト長を超えないよう、**古いツール結果を剪定**する。直近 `keep_recent`
//! ステップの観測は無傷で残し、それ以前の**大きな `tool_result`**だけを短い要約プレースホルダに
//! 畳み込む。重要事実・計画・成果物参照は agent-core 外（外部メモリ＝ワークスペースの `plan.md`/
//! `notes.md`・W2）に逃がす前提で、ここでは**冗長なツール出力の畳み込み**に徹する。
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
}
