//! 文章のバイト列の文字コードを決めて読む・書く（GAP-45・#405）。
//!
//! 使うところは、ワークフローの「ファイルを読む」と AI エージェントの窓読み（api）、CSV の集計と編集（tabular）、
//! データの取り込み（data）、AI のウェブ取得（agent-core）。判定はここ 1 か所に置き、2 つ持たない。
//!
//! ファイルの文章（[`decode_text`]）は、次の順で文字コードを決める。
//!
//! 1. BOM（UTF-8・UTF-16LE・UTF-16BE）。Excel の「CSV UTF-8」と「Unicode テキスト」はここで決まる。
//! 2. BOM が無く、全体が妥当な UTF-8 なら UTF-8。
//! 3. 推定（chardetng）。日本の文字コード（Shift_JIS〔CP932〕・EUC-JP）だけを採り、確からしさが低ければ断る
//!    （[`guess_japanese`]）。日本語の Windows の Excel が保存する CSV の既定は Shift_JIS。
//!
//! 推定で断るのは、文字化けしたまま読んだことにしないため（以前の `from_utf8_lossy` は、Shift_JIS の CSV を
//! 置換文字だらけにして成功させていた）。
//!
//! ウェブのページ（[`guess_any`]）は、宣言（Content-Type・`<meta charset>`）を agent-core が先に見て、宣言が
//! 無いときにここで決める。ページは読めなくても何かを見せる用途なので、推定の答えをそのまま使う（断らない）。

/// 文字コードの定数（`textenc::encoding_rs::SHIFT_JIS` など）を使う側が別に依存しなくて済むように出す。
pub use encoding_rs;
use encoding_rs::{
    Encoding, BIG5, EUC_JP, EUC_KR, GB18030, GBK, ISO_2022_JP, SHIFT_JIS, UTF_16BE, UTF_16LE, UTF_8,
};

/// 推定に渡す最大のバイト数（最初の ASCII でないバイトから数える）。大きなファイルでも推定の手間を抑える。
/// 中身の全体は、決めた文字コードで最後まで誤りなく読めるかを別に確かめる（[`decode_text`]）。
const GUESS_WINDOW_BYTES: usize = 1 << 20;

/// 推定の窓の前に含める ASCII のバイト数（推定器は直前の文字も見る）。
const GUESS_CONTEXT_BYTES: usize = 16;

/// 読んだ文章と、読んだ文字コード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// 中身の文章（BOM は含まない）。
    pub text: String,
    /// 読んだ文字コード（名前は [`Encoding::name`]。例: UTF-8、Shift_JIS、UTF-16LE）。
    pub encoding: &'static Encoding,
    /// 先頭に BOM があったか（同じ形で書き戻すときに付け直す・[`encode_like`]）。
    pub bom: bool,
}

/// 文章として読めない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotText {
    /// 文章でない中身（NUL を含む。BOM で始まる UTF-16 は、改行・タブ以外の制御文字も）。
    Binary,
    /// 文字コードを決められない（UTF-8 でも日本の文字コードでもない、推定の確からしさが低い、
    /// BOM の文字コードとして壊れている）。
    UnknownEncoding,
}

/// その文字コードに無いため書けない文字（[`encode_like`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unmappable(pub char);

/// BOM が示す文字コードと BOM のバイト数（UTF-8・UTF-16LE・UTF-16BE）。
#[must_use]
pub fn bom(bytes: &[u8]) -> Option<(&'static Encoding, usize)> {
    Encoding::for_bom(bytes)
}

/// 全体が妥当な UTF-8 か。`cut_tail` なら、末尾で切れた多バイト文字だけは許す（大きさの上限で途中まで
/// 取った本文を、打ち切りのせいで UTF-8 でないと決めない）。
#[must_use]
pub fn is_utf8(bytes: &[u8], cut_tail: bool) -> bool {
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        // error_len() が None なのは「入力の末尾で切れた」とき。
        Err(e) => cut_tail && e.error_len().is_none(),
    }
}

/// 宣言の無いバイト列の文字コード（ウェブのページ）。妥当な UTF-8（末尾で切れた文字は許す）なら UTF-8、
/// そうでなければ推定の答え。**必ず何かを返す**（断らない）。
#[must_use]
pub fn guess_any(bytes: &[u8]) -> &'static Encoding {
    if is_utf8(bytes, true) {
        return UTF_8;
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(bytes, true);
    detector.guess(None, true)
}

/// ファイルの中身を文章として読む（判定の順はモジュールの説明）。文字化けしたまま読んだことにはしない。
pub fn decode_text(bytes: &[u8]) -> Result<Decoded, NotText> {
    if let Some((encoding, len)) = bom(bytes) {
        let text = encoding
            .decode_without_bom_handling_and_without_replacement(&bytes[len..])
            .ok_or(NotText::UnknownEncoding)?;
        // UTF-16 は、BOM で始まる文章でないファイルも誤りなく読めてしまうので、制御文字でも確かめる。
        let binary = if encoding == UTF_8 {
            text.contains('\0')
        } else {
            text.chars().any(is_binary_control)
        };
        if binary {
            return Err(NotText::Binary);
        }
        return Ok(Decoded {
            text: text.into_owned(),
            encoding,
            bom: true,
        });
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        // UTF-8 は NUL だけで断る（以前の判定と同じ。ログの色の指定〔ESC〕などは文章として読む）。
        if text.contains('\0') {
            return Err(NotText::Binary);
        }
        return Ok(Decoded {
            text: text.to_owned(),
            encoding: UTF_8,
            bom: false,
        });
    }
    let encoding = guess_japanese(bytes).ok_or(NotText::UnknownEncoding)?;
    let text = encoding
        .decode_without_bom_handling_and_without_replacement(bytes)
        .ok_or(NotText::UnknownEncoding)?;
    // Shift_JIS・EUC-JP も UTF-8 と同じく NUL だけで断る（以前の画面の判定と同じ）。MS-DOS の頃の帳票の末尾の
    // EOF の印（0x1A）や、ログの色の指定（ESC）は文章として読む。誤りなく読めることと推定で、文章でない中身は
    // ほぼ落ちる。
    if text.contains('\0') {
        return Err(NotText::Binary);
    }
    Ok(Decoded {
        text: text.into_owned(),
        encoding,
        bom: false,
    })
}

/// UTF-8 でないバイト列を、日本の文字コード（Shift_JIS・EUC-JP）として推定する。確からしいときだけ返す。
///
/// 推定器（chardetng）に、日本のサイト（`jp`）という手がかりを付けたときと付けないときの 2 回聞く。
///
/// - 手がかりを付けて、日本の文字コードと答えなければ断る（西欧の文字コードなど）。
/// - 手がかりを付けないと、別の東アジアの多バイトの文字コード（中国語の GBK・Big5、韓国語の EUC-KR、もう一方の
///   日本の文字コード）と答えるなら、バイト列がどちらとも読めるということなので、確からしさが低いとして断る。
///   中国語や韓国語の文章が、手がかりのせいで日本語の文字化けとして読まれるのを防ぐ。
///
/// 手がかりを付けないときの答えが 1 バイトの文字コード（西欧・キリル文字など）なのは、短い日本語で手がかりが
/// 足りないときによく起きる。そのときは手がかりを付けた答えを採る（西欧の文章は、Shift_JIS や EUC-JP として
/// 最後まで誤りなく読めることがまず無く、[`decode_text`] がそこで断る）。
fn guess_japanese(bytes: &[u8]) -> Option<&'static Encoding> {
    let first = Encoding::ascii_valid_up_to(bytes);
    // 窓の前は ASCII だけなので、どこで切っても文字の途中にはならない。
    let from = first.saturating_sub(GUESS_CONTEXT_BYTES);
    let to = first.saturating_add(GUESS_WINDOW_BYTES).min(bytes.len());
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(&bytes[from..to], to == bytes.len());
    let japanese = detector.guess(Some(b"jp"), false);
    if japanese != SHIFT_JIS && japanese != EUC_JP {
        return None;
    }
    let plain = detector.guess(None, false);
    if plain != japanese && is_east_asian_multibyte(plain) {
        return None;
    }
    Some(japanese)
}

/// 東アジアの多バイトの文字コードか（日本語・中国語・韓国語）。
fn is_east_asian_multibyte(encoding: &'static Encoding) -> bool {
    [SHIFT_JIS, EUC_JP, ISO_2022_JP, GBK, GB18030, BIG5, EUC_KR].contains(&encoding)
}

/// 文章に入らない制御文字か（タブ・改行・改ページは文章に入る）。
fn is_binary_control(c: char) -> bool {
    (c.is_control() && !matches!(c, '\t' | '\n' | '\r' | '\u{0C}')) || c == '\u{FFFE}'
}

/// Shift_JIS・EUC-JP（encoding_rs は WHATWG の表）で書けない記号のうち、Windows（CP932）が見た目の同じ全角の文字で
/// 持つもの。Mac の入力でよく出る（〜 など）。読むと 0x8160 は ～ に見えるので、書き戻すときはこちらに置き換える。
const CP932_LOOK_ALIKES: [(char, char); 6] = [
    ('\u{301C}', '\u{FF5E}'), // 〜 → ～
    ('\u{2014}', '\u{2015}'), // — → ―
    ('\u{2016}', '\u{2225}'), // ‖ → ∥
    ('\u{00A2}', '\u{FFE0}'), // ¢ → ￠
    ('\u{00A3}', '\u{FFE1}'), // £ → ￡
    ('\u{00AC}', '\u{FFE2}'), // ¬ → ￢
];

/// [`CP932_LOOK_ALIKES`] の置き換え（無ければそのまま）。
fn cp932_look_alike(c: char) -> char {
    CP932_LOOK_ALIKES
        .iter()
        .find_map(|(from, to)| (*from == c).then_some(*to))
        .unwrap_or(c)
}

/// 文章を、読んだときと同じ文字コードと BOM で書く（CSV を編集した後も、元の文字コードのまま保存する）。
///
/// その文字コードに無い文字があれば、その文字を返して書かない（`&#…;` に置き換えて黙って中身を変えない）。
/// Shift_JIS・EUC-JP では、Windows が見た目の同じ全角の文字で持つ記号（〜・— など・[`CP932_LOOK_ALIKES`]）を、
/// その文字に置き換えて書く（読み直すと全角の文字になる）。
pub fn encode_like(
    text: &str,
    encoding: &'static Encoding,
    bom: bool,
) -> Result<Vec<u8>, Unmappable> {
    if encoding == UTF_8 {
        let mut out = Vec::with_capacity(text.len() + 3);
        if bom {
            out.extend_from_slice(b"\xEF\xBB\xBF");
        }
        out.extend_from_slice(text.as_bytes());
        return Ok(out);
    }
    if encoding == UTF_16LE || encoding == UTF_16BE {
        // encoding_rs は UTF-16 へ書かない（Encoding Standard の決まり）ので、ここで並べる。
        let le = encoding == UTF_16LE;
        let mut out = Vec::with_capacity(text.len() * 2 + 2);
        if bom {
            out.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
        }
        for unit in text.encode_utf16() {
            out.extend_from_slice(&if le {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        return Ok(out);
    }
    let text: std::borrow::Cow<'_, str> = if (encoding == SHIFT_JIS || encoding == EUC_JP)
        && text.chars().any(|c| cp932_look_alike(c) != c)
    {
        std::borrow::Cow::Owned(text.chars().map(cp932_look_alike).collect())
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    let text = text.as_ref();
    let mut encoder = encoding.new_encoder();
    let capacity = encoder
        .max_buffer_length_from_utf8_without_replacement(text.len())
        .unwrap_or(text.len().saturating_mul(4));
    let mut out = Vec::with_capacity(capacity);
    let mut rest = text;
    loop {
        let (result, read) =
            encoder.encode_from_utf8_to_vec_without_replacement(rest, &mut out, true);
        rest = &rest[read..];
        match result {
            encoding_rs::EncoderResult::InputEmpty => return Ok(out),
            encoding_rs::EncoderResult::Unmappable(c) => return Err(Unmappable(c)),
            encoding_rs::EncoderResult::OutputFull => {
                out.reserve(rest.len().saturating_mul(4) + 16);
            }
        }
    }
}

#[cfg(test)]
mod tests;
