//! 本文に埋め込まれた `<think>…</think>` を思考として切り出す（#584）。
//!
//! 一部のモデル（MiniMax M3 等）は思考を `reasoning_content` ではなく本文（`content`）の
//! 先頭に `<think>…</think>` で入れて返す。そのまま流すと回答に思考が出るので、本文の
//! **先頭**（前置きの空白は許容）にあるものだけを思考へ振り分け、残りを本文にする。
//! 先頭以外の `<think>` はコード例などの本文として扱い、触らない。
//!
//! ストリームではタグがチャンクの境目で分かれる（`<thi` + `nk>`）ので、判定がつくまで
//! 末尾を保留する状態機械にしている。

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// 切り出した断片。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Piece {
    Text(String),
    Thinking(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// 本文の先頭。`<think>` で始まるかまだ判定できない。
    Start,
    /// `<think>` の中。`</think>` を探している。
    InThink,
    /// `</think>` の直後。回答の前の空白（改行）を読み飛ばす。
    AfterThink,
    /// 判定済み。以降はそのまま本文。
    Text,
}

/// 本文デルタを順に与え、思考と本文の断片へ分ける。
#[derive(Debug)]
pub(super) struct ThinkSplitter {
    state: State,
    /// 判定を保留している末尾（タグの途中かもしれない部分）。
    pending: String,
}

impl Default for ThinkSplitter {
    fn default() -> Self {
        ThinkSplitter {
            state: State::Start,
            pending: String::new(),
        }
    }
}

impl ThinkSplitter {
    /// 本文デルタを 1 つ与える。確定した断片を返す（空文字の断片は返さない）。
    pub(super) fn push(&mut self, chunk: &str) -> Vec<Piece> {
        let mut out = Vec::new();
        self.pending.push_str(chunk);
        loop {
            match self.state {
                State::Text => {
                    emit(&mut out, Piece::Text(std::mem::take(&mut self.pending)));
                    return out;
                }
                State::Start => {
                    let trimmed = self.pending.trim_start();
                    if let Some(rest) = trimmed.strip_prefix(OPEN) {
                        self.pending = rest.to_string();
                        self.state = State::InThink;
                    } else if OPEN.starts_with(trimmed) {
                        // 空白だけ、または `<thi` のようにタグの途中。続きを待つ。
                        return out;
                    } else {
                        self.state = State::Text;
                    }
                }
                State::InThink => {
                    if let Some(pos) = self.pending.find(CLOSE) {
                        let thinking = self.pending[..pos].to_string();
                        self.pending = self.pending[pos + CLOSE.len()..].to_string();
                        emit(&mut out, Piece::Thinking(thinking));
                        self.state = State::AfterThink;
                    } else {
                        // 末尾が `</th` のように閉じタグの途中なら、その分だけ残す。
                        let keep = partial_suffix(&self.pending, CLOSE);
                        let cut = self.pending.len() - keep;
                        let thinking = self.pending[..cut].to_string();
                        self.pending.drain(..cut);
                        emit(&mut out, Piece::Thinking(thinking));
                        return out;
                    }
                }
                State::AfterThink => {
                    let trimmed = self.pending.trim_start();
                    if trimmed.is_empty() {
                        self.pending.clear();
                        return out;
                    }
                    self.pending = trimmed.to_string();
                    self.state = State::Text;
                }
            }
        }
    }

    /// ストリームの終わり。保留していた分を確定させる。
    ///
    /// `<think>` が閉じないまま終わったら（出力上限で切れた等）、中身は思考として扱う
    /// （モデル自身が思考と印を付けたものを回答として見せない）。
    pub(super) fn finish(&mut self) -> Vec<Piece> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.pending);
        match self.state {
            State::Start | State::Text => emit(&mut out, Piece::Text(rest)),
            State::InThink => emit(&mut out, Piece::Thinking(rest)),
            State::AfterThink => {}
        }
        self.state = State::Text;
        out
    }
}

fn emit(out: &mut Vec<Piece>, piece: Piece) {
    let empty = match &piece {
        Piece::Text(t) | Piece::Thinking(t) => t.is_empty(),
    };
    if !empty {
        out.push(piece);
    }
}

/// `s` の末尾が `tag` の先頭部分（全体ではない）と一致する最長の長さ（バイト）。
///
/// `tag` は ASCII なので、一致した末尾の開始位置は必ず文字境界になる。
fn partial_suffix(s: &str, tag: &str) -> usize {
    (1..tag.len())
        .rev()
        .find(|&k| s.ends_with(&tag[..k]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// チャンク列を流して、隣り合う同種の断片をつなげた結果を返す。
    fn run(chunks: &[&str]) -> Vec<Piece> {
        let mut s = ThinkSplitter::default();
        let mut pieces: Vec<Piece> = chunks.iter().flat_map(|c| s.push(c)).collect();
        pieces.extend(s.finish());
        let mut merged: Vec<Piece> = Vec::new();
        for p in pieces {
            match (merged.last_mut(), p) {
                (Some(Piece::Text(a)), Piece::Text(b))
                | (Some(Piece::Thinking(a)), Piece::Thinking(b)) => a.push_str(&b),
                (_, p) => merged.push(p),
            }
        }
        merged
    }

    fn text(s: &str) -> Piece {
        Piece::Text(s.into())
    }

    fn think(s: &str) -> Piece {
        Piece::Thinking(s.into())
    }

    #[test]
    fn leading_think_block_becomes_thinking() {
        assert_eq!(
            run(&["<think>考える</think>\n\n答え"]),
            [think("考える"), text("答え")]
        );
    }

    #[test]
    fn tags_split_across_chunks() {
        assert_eq!(
            run(&["<thi", "nk>考", "える</th", "ink>", "\n", "答え"]),
            [think("考える"), text("答え")]
        );
        // 1 文字ずつ流しても同じ。
        let all = "<think>abc</think>xyz";
        let chars: Vec<String> = all.chars().map(String::from).collect();
        let refs: Vec<&str> = chars.iter().map(String::as_str).collect();
        assert_eq!(run(&refs), [think("abc"), text("xyz")]);
    }

    #[test]
    fn leading_whitespace_before_tag_is_allowed() {
        assert_eq!(run(&["\n  ", "<think>x</think>y"]), [think("x"), text("y")]);
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        assert_eq!(run(&["  こんにちは", "世界"]), [text("  こんにちは世界")]);
        // `<` で始まるが `<think>` ではない本文。
        assert_eq!(run(&["<", "b>太字</b>"]), [text("<b>太字</b>")]);
    }

    #[test]
    fn think_tag_not_at_start_stays_in_text() {
        assert_eq!(
            run(&["例: ", "<think>x</think> と書く"]),
            [text("例: <think>x</think> と書く")]
        );
    }

    #[test]
    fn unclosed_think_is_thinking() {
        assert_eq!(
            run(&["<think>途中で", "切れた</th"]),
            [think("途中で切れた</th")]
        );
    }

    #[test]
    fn only_think_without_answer_yields_no_text() {
        assert_eq!(run(&["<think>x</think>", "\n\n"]), [think("x")]);
    }

    #[test]
    fn stream_ending_mid_open_tag_is_text() {
        assert_eq!(run(&["<thi"]), [text("<thi")]);
    }

    #[test]
    fn multibyte_text_next_to_partial_close_tag() {
        assert_eq!(
            run(&["<think>日本語</", "think>答え"]),
            [think("日本語"), text("答え")]
        );
    }
}
