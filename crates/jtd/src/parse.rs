//! `DocumentText` のユニット列 → [`JtdDocument`]（トラックJTD）。
//!
//! # レコード構造
//!
//! 本文ストリームは UTF-16BE のテキストと、自己記述型の可変長レコードが交互に並ぶ。
//! 構造は所有フォークに vendor 済みの
//! `vendor/openjtd/openjtd-spec/rfc/0009-document-text-paragraph-record.ja.md` が正本。
//!
//! ```text
//! w0        0x001C  レコードオープナー
//! w1        class   0x0000 / 0x0010 / 0x0020 / 0x0030
//! w2        len     総ワード数（w0 とフッタを含む）
//! w3…               ペイロード（len - 7 ワード）
//! w[len-4]  len エコー
//! w[len-3]  0x0000
//! w[len-2]  class エコー
//! w[len-1]  0x001F  ターミネータ ＝ テキストラン開始
//! ```
//!
//! **`len` と `class` のエコーが整合検査になる。** 長さを詐称した入力はここで弾けるので、
//! 「`0x001F` を探して読み飛ばす」ような当てずっぽうの走査をしないで済む。
//!
//! # 上流と違えている点
//!
//! 上流 `parse_document_text` は「最初の `0x001F` を見るまで本文を読まない」ため、
//! **ヘッダ直後・最初のレコードより前にある本文を落とす**。`f1.jtd` では 1 行目の
//! 「様式第１（第７条関係）」がまるごと消える（厚労省が同じ様式で配布している
//! text 版の 1 行目と一致するので、これは本文である）。ここではヘッダを読み飛ばした
//! 直後から本文として読む。
//!
//! # 本タスクで読むもの / 読まないもの
//!
//! `class=0x0010`（論理段落の開始）だけを段落境界として使い、他のクラスは
//! 構造として正しく消費したうえで意味は無視する。特に `class=0x0030` は
//! 表セルヘッダで `b0`/`b1` にセルの左右座標を持つが、**表の再構成は JTD.3 の範囲**。
//! ここで中途半端に扱うと、後から入る本物の表と二重になる。

use crate::model::{Block, JtdDocument, Paragraph, TextRun};

/// `DocumentText` ストリーム先頭のマジック。
const MAGIC: &[u8; 8] = b"SsmgV.01";
/// マジックの後に続くヘッダのワード数（ストリーム先頭から数えて 10 ワード − マジック 4 ワード）。
const HEADER_WORDS_AFTER_MAGIC: usize = 6;
/// セグメント数フィールドの位置（マジックの後ろから数えて）。
const SEGMENT_COUNT_INDEX: usize = 1;
/// 生テキストセグメント形式を表すセグメント数。
const RAW_TEXT_SEGMENT_COUNT: u16 = 0x0001;
/// 生テキストセグメント形式の名前。
const TEXT_SEGMENT_NAME: &[u16; 4] = &[0x5465, 0x7874, 0x562e, 0x3031]; // "TextV.01"

/// レコードオープナー。
const RECORD_OPEN: u16 = 0x001c;
/// レコードターミネータ。テキストランの開始でもある。
const RECORD_END: u16 = 0x001f;
/// ルビ等のインライン形式の開始（`class=0x0001`・ターミネータが別物）。
const INLINE_OPEN: u16 = 0x001d;
/// インライン形式の終了。
const INLINE_CLOSE: u16 = 0x001e;
/// 表の**行**区切り（RFC 0003 `TABLE_ROW_DELIMITER_CONTROL`・上流 `TEXT_ROW_DELIMITER`）。
///
/// **セル区切りではない。** セルはクラス `0x0030` のレコードが 1 セル 1 件で区切る
/// （RFC 0009）。実データでも `0x000E` の直後は必ず `0x001C class=0x0010` の行ヘッダで、
/// 行の末尾にしか現れない。タブとして出すと行末にゴミが残る。
const ROW_DELIMITER: u16 = 0x000e;
/// 段落内の改行。
const LINE_BREAK: u16 = 0x000a;
/// 改ページ（RFC 0003: 一太郎の COM エクスポートが `Chr(12)` を改ページ文字に使う）。
///
/// **テキストランを閉じない。** 制御コードとして扱ってしまうと、改ページの直後にある
/// 本文（`f1.jtd` では「作成上の留意事項」以下の記入要領 283 文字）が丸ごと落ちる。
const PAGE_BREAK: u16 = 0x000c;

/// 論理段落の開始を表すレコードクラス。
const CLASS_PARAGRAPH: u16 = 0x0010;
/// 表示テキストとして取り込むインラインセレクタ。
///
/// このセレクタを持つインラインセグメントは本文の一部（申請書の欄見出し「連絡先」
/// 「所属施設」などがこの形で入っている）。認識できないセレクタはルビ・テンプレート
/// 差込なので本文には混ぜない。
const INLINE_TEXT_SELECTORS: [u16; 3] = [0x0001, 0x0003, 0x0013];
/// インラインレコードの固定ヘッダ（`0x001C class=0x0001 len=0x0007 0x0000 0x0000`）。
///
/// **このクラスだけフッタを持たない。** 終端が `0x001D` で、以降 `0x001E` までが表示テキスト。
/// 標準レコードとして読もうとすると整合検査に落ち、そこで本文全体が途切れる
/// （実際 `f1.jtd` は 9,984 文字が 223 文字になっていた）。
const INLINE_HEADER: [u16; 5] = [0x001c, 0x0001, 0x0007, 0x0000, 0x0000];
/// インラインレコードの固定長（`0x001C 0x0001 0x0007 … 0x001D` の 7 ワード・RFC 0009）。
const INLINE_RECORD_WORDS: usize = 7;
/// 表示テキストの最大ユニット数。
///
/// **有界性のために要る。** 閉じ `0x001E` を末尾まで探すと、`0x001D` を敷き詰めた入力で
/// 走査が O(n²) になる（実測 400,000 ユニットで 56 秒・8 MiB なら 1.7 時間）。
/// 上流 `read_skipped_inline_segment` も同じ 256 ユニットで打ち切っている。
const INLINE_MAX_UNITS: usize = 256;

/// フッタの固定ワード（len エコーの次）。
const FOOTER_PAD: u16 = 0x0000;
/// フッタ（len エコー・0x0000・class エコー・0x001F）を含む最小のレコード長。
const MIN_RECORD_WORDS: usize = 7;

/// `DocumentText` の生バイト列を [`JtdDocument`] へ組み上げる。
///
/// マジックが違う場合は空文書を返す（入力の妥当性は呼び出し側の
/// [`crate::JtdFile`] が既に判定している）。
pub(crate) fn parse_document(raw: &[u8]) -> JtdDocument {
    let Some(body) = raw.strip_prefix(MAGIC) else {
        return JtdDocument::default();
    };
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();

    let start = text_start(&units);
    let end = text_end(&units, start);
    Builder::default().run(&units[..end], start)
}

/// 本文が始まるユニット位置を返す。
///
/// 通常形式はヘッダ 6 ワードの直後。生テキストセグメント形式（`TextV.01` が続くもの）は
/// さらに名前 4 ワードと長さ 2 ワードを飛ばす。
fn text_start(units: &[u16]) -> usize {
    let after_header = HEADER_WORDS_AFTER_MAGIC;
    let follows_text_segment = units
        .get(after_header..after_header + TEXT_SEGMENT_NAME.len())
        .is_some_and(|name| name == TEXT_SEGMENT_NAME);

    if follows_text_segment {
        after_header + TEXT_SEGMENT_NAME.len() + 2
    } else {
        after_header
    }
}

/// 本文の終端ユニット位置。
///
/// 生テキストセグメント形式（セグメント数 `0x0001` ＋ `TextV.01`）は宣言長を持つので、
/// そこで切る。宣言長を無視すると末尾のパディングを本文として取り込む。
/// 通常形式は宣言長を持たないのでストリーム末尾まで。
fn text_end(units: &[u16], start: usize) -> usize {
    let is_raw_text_segment = units.get(SEGMENT_COUNT_INDEX) == Some(&RAW_TEXT_SEGMENT_COUNT)
        && units
            .get(HEADER_WORDS_AFTER_MAGIC..HEADER_WORDS_AFTER_MAGIC + TEXT_SEGMENT_NAME.len())
            .is_some_and(|name| name == TEXT_SEGMENT_NAME);

    if !is_raw_text_segment {
        return units.len();
    }
    let declared = units.get(start - 1).copied().unwrap_or(0) as usize;
    start.saturating_add(declared).min(units.len())
}

/// レコードの読み取り結果。
struct Record {
    class: u16,
    /// レコード全体が占めるワード数。
    words: usize,
}

/// ユニット列を舐めて段落を組み立てる。
#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    /// 現在の段落に積まれた文字。
    current: String,
    /// テキストランの内側にいるか。
    reading_text: bool,
}

impl Builder {
    fn run(mut self, units: &[u16], start: usize) -> JtdDocument {
        // ヘッダ直後は本文。上流のように「最初の 0x001F を待つ」ことはしない。
        self.reading_text = true;

        let mut index = start;
        while index < units.len() {
            let unit = units[index];

            if unit == RECORD_OPEN {
                if let Some(open) = inline_record_open(units, index) {
                    index = self.read_inline_record(units, open);
                    continue;
                }
                let Some(record) = read_record(units, index) else {
                    // 自己記述が整合しない。**ここで文書を打ち切らない。**
                    // `0x001C` は埋め込みオブジェクト等の非テキスト領域にも偶然現れ、
                    // `f1.jtd` では 883 件の表セルレコードのうち 1 件がこの偽陽性だった。
                    // 打ち切ると本文の後半 5,000 文字超がまるごと消える。
                    // 制御コードとして 1 ユニットだけ進み、次の正しいレコードで再同期する。
                    self.reading_text = false;
                    index += 1;
                    continue;
                };
                if record.class == CLASS_PARAGRAPH {
                    self.break_paragraph();
                }
                index += record.words;
                // レコードのターミネータ 0x001F がテキストランを開く。
                self.reading_text = true;
                continue;
            }

            if unit == INLINE_OPEN {
                // ルビ・テンプレート差込。表示テキストの取り込みは JTD.5 の範囲。
                index = skip_inline(units, index);
                continue;
            }

            // テキストランマーカーは `reading_text` の状態に関わらず本文を開き直す。
            // レコードのフッタ（`0x0005 0x0000 0x0001 0x001F` 等）がこの形で現れるので、
            // ここを条件付きにすると直後の本文が落ちる。
            if unit == RECORD_END {
                self.reading_text = true;
                index += 1;
                continue;
            }

            if !self.reading_text {
                index += 1;
                continue;
            }

            index += self.push_text(units, index);
        }

        self.break_paragraph();
        JtdDocument::new(self.blocks)
    }

    /// テキストランの中の 1 文字を処理し、消費したユニット数を返す。
    fn push_text(&mut self, units: &[u16], index: usize) -> usize {
        let unit = units[index];
        match unit {
            // 改行・改ページ・表の行区切りはどれも行を割るが、本文は続く。
            LINE_BREAK | PAGE_BREAK | ROW_DELIMITER => {
                self.current.push('\n');
                return 1;
            }
            0x0000..=0x001f => {
                self.reading_text = false;
                return 1;
            }
            _ => {}
        }

        // 上位面の文字はサロゲート対で来る。片方だけを文字にすると
        // 「𠮷田」が「田」になるので、対で組み直す。
        if (0xd800..=0xdbff).contains(&unit) {
            let low = units.get(index + 1).copied().unwrap_or(0);
            if (0xdc00..=0xdfff).contains(&low) {
                let scalar = 0x1_0000 + (u32::from(unit - 0xd800) << 10) + u32::from(low - 0xdc00);
                if let Some(character) = char::from_u32(scalar) {
                    self.current.push(character);
                    return 2;
                }
            }
            // 対になっていない孤立サロゲートは文字にできない。捨てる。
            return 1;
        }

        // 非文字（U+FFFE / U+FFFF）は本文ではない。レコードのパディングに現れる。
        if unit == 0xfffe || unit == 0xffff {
            return 1;
        }

        if let Some(character) = char::from_u32(u32::from(unit)) {
            self.current.push(character);
        }
        1
    }

    /// インラインレコードの表示テキストを処理し、次の位置を返す。
    ///
    /// `open` は終端 `0x001D` の位置。そこから `0x001E` までが表示テキストで、
    /// セレクタが [`INLINE_TEXT_SELECTORS`] のものは本文に取り込み、それ以外
    /// （ルビ・テンプレート差込）は捨てる。
    fn read_inline_record(&mut self, units: &[u16], open: usize) -> usize {
        let Some(close) = find_inline_close(units, open) else {
            // 閉じが無い（＝インラインではなかった）。`0x001D` を制御コードとして
            // 1 ユニットだけ進める。ここで末尾まで走査すると O(n²) になる。
            self.reading_text = false;
            return open + 1;
        };

        if is_display_text(units, open) {
            let mut index = open + 1;
            while index < close {
                index += self.push_text(units, index);
            }
        }
        // 表示テキストの直後はレコードのフッタ。単独の `0x001F` が
        // テキストランを開き直すので、そのまま本文の続きへ戻れる。
        close + 1
    }

    /// 段落を確定する。
    ///
    /// 末尾の改行は **1 つだけ** 落とす。JTD は「本文…改行、次の段落レコード」という
    /// 並びなので 1 つ残ると全段落の末尾に空行が付くが、全部落とすと原本にある
    /// 空行まで消えて詰まって見える。段落内部の改行は当然残す。
    fn break_paragraph(&mut self) {
        let text = std::mem::take(&mut self.current);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        if text.is_empty() {
            return;
        }
        self.blocks
            .push(Block::Paragraph(Paragraph::new(vec![TextRun::new(text)])));
    }
}

/// `0x001C` から始まるレコードを読む。自己記述が整合しなければ `None`。
fn read_record(units: &[u16], start: usize) -> Option<Record> {
    let class = *units.get(start + 1)?;
    let len = *units.get(start + 2)? as usize;
    if len < MIN_RECORD_WORDS {
        return None;
    }
    let end = start.checked_add(len)?;
    if end > units.len() {
        return None;
    }

    // フッタ: [len エコー][0x0000][class エコー][0x001F]
    if units[end - 4] as usize != len
        || units[end - 3] != FOOTER_PAD
        || units[end - 2] != class
        || units[end - 1] != RECORD_END
    {
        return None;
    }

    Some(Record { class, words: len })
}

/// `0x001C` がインラインレコードの開始なら、その終端 `0x001D` の位置を返す。
///
/// **形が完全に一致するときだけ受ける。** 標準レコードには len/class のエコー検査が
/// あるのに、こちらは `0x001C 0x0001` の 2 ワードが並ぶだけで入っていた。
/// `0x001C` は非テキスト領域にも偶然現れるので、同じ厳しさで見ないと
/// 偽陽性 1 件で本文が丸ごと消える（標準レコード側は再同期できるのに、
/// こちらだけ壊れ方が非対称だった）。
fn inline_record_open(units: &[u16], start: usize) -> Option<usize> {
    let open = start + INLINE_RECORD_WORDS - 1;
    let header = units.get(start..=open)?;
    (header[..INLINE_HEADER.len()] == INLINE_HEADER
        && header[INLINE_RECORD_WORDS - 1] == INLINE_OPEN)
        .then_some(open)
}

/// 表示テキストの閉じ `0x001E` を、`INLINE_MAX_UNITS` の窓の中だけ探す。
fn find_inline_close(units: &[u16], open: usize) -> Option<usize> {
    let end = units.len().min(open + 1 + INLINE_MAX_UNITS);
    units
        .get(open + 1..end)?
        .iter()
        .position(|unit| *unit == INLINE_CLOSE)
        .map(|offset| open + 1 + offset)
}

/// `0x001D` の直前のヘッダを見て、続く表示テキストを本文に取り込むべきか判定する。
fn is_display_text(units: &[u16], open: usize) -> bool {
    let Some(context) = open.checked_sub(6).and_then(|from| units.get(from..open)) else {
        return false;
    };
    context[..INLINE_HEADER.len()] == INLINE_HEADER
        && INLINE_TEXT_SELECTORS.contains(&context[INLINE_HEADER.len()])
}

/// 単独で現れた `0x001D`…`0x001E` を読み飛ばし、次の位置を返す。
///
/// 閉じが窓の中に無ければ 1 ユニットだけ進める。末尾まで探すと、`0x001D` を
/// 敷き詰めた入力で走査が O(n²) になる。
fn skip_inline(units: &[u16], start: usize) -> usize {
    find_inline_close(units, start).map_or(start + 1, |close| close + 1)
}

#[cfg(test)]
mod tests;
