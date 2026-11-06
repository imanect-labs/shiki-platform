//! ⑧ 組み立て: チャンクの訳を順に連結し、プレースホルダを原文へ戻す（設計メモ §4.6）。
//!
//! - 表の続きのチャンクは、複製した見出し行の訳を捨てる（表の途中に見出し行を繰り返さない）。
//! - 翻訳できなかったチャンクは原文のまま残し、注記を**閉じた Markdown ブロックの後ろ**に置く
//!   （段落の途中で割ったチャンクや表の途中では、次の空行まで注記を持ち越す）。

use super::chunk::Chunk;
use super::protect::Protected;
use super::ChunkResult;

/// 翻訳できなかった節に付ける注記（こちらの固定文言）。
pub(super) const FAILED_NOTE: &str =
    "> ⚠ この部分は翻訳できなかったため、原文のまま表示しています。";

pub(super) struct Assembled {
    pub markdown: String,
    /// 翻訳できなかったチャンクの番号（0 始まり）。
    pub failed_chunks: Vec<usize>,
}

pub(super) fn assemble(chunks: &[Chunk], results: &[ChunkResult], p: &Protected) -> Assembled {
    let mut out = String::new();
    let mut failed_chunks = Vec::new();
    let mut pending_note = false;
    for (chunk, result) in chunks.iter().zip(results) {
        let mut text = result.text.as_str();
        if chunk.dup_header_lines > 0 {
            text = drop_lines(text, chunk.dup_header_lines);
        }
        let text = text.trim_end();
        // 前のチャンクの注記を持ち越していて、このチャンクの中で段落が切れるなら、そこに置く。
        match (pending_note, text.find("\n\n")) {
            (true, Some(at)) => {
                out.push_str(&text[..at]);
                out.push_str("\n\n");
                out.push_str(FAILED_NOTE);
                out.push_str(&text[at..]);
                pending_note = false;
            }
            _ => out.push_str(text),
        }
        if result.failed {
            failed_chunks.push(chunk.index);
            pending_note = true;
        }
        // 注記はブロックの切れ目（空行）か末尾でだけ出す。
        if pending_note && (chunk.gap_after.contains("\n\n") || chunk.gap_after.is_empty()) {
            out.push_str("\n\n");
            out.push_str(FAILED_NOTE);
            pending_note = false;
        }
        out.push_str(&chunk.gap_after);
    }
    if pending_note {
        out.push_str("\n\n");
        out.push_str(FAILED_NOTE);
    }
    Assembled {
        markdown: p.restore(out.trim_end()),
        failed_chunks,
    }
}

/// 先頭の `n` 行を捨てる。
fn drop_lines(s: &str, n: usize) -> &str {
    let mut rest = s;
    for _ in 0..n {
        match rest.find('\n') {
            Some(i) => rest = &rest[i + 1..],
            None => return "",
        }
    }
    rest
}
