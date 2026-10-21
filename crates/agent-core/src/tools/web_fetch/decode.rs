//! 取得バイト列の文字コード判定とデコード（#405）。
//!
//! 旧実装は `String::from_utf8_lossy` 固定だった。国内サイトに残る Shift_JIS / EUC-JP は
//! **全バイトが U+FFFD になる**ため、モデルは「読めない文字列」に 4,000 トークン払って終わる。
//! 正しさの問題であると同時にコンテキスト効率の問題でもある。
//!
//! 優先順位は HTML5 のエンコーディング sniffing に倣う:
//! 1. BOM（最も強い。宣言と食い違っても BOM が勝つ）
//! 2. `Content-Type: ...; charset=`
//! 3. `<meta charset>` / `<meta http-equiv="Content-Type">`
//! 4. バイト列が妥当な UTF-8 ならそれ
//! 5. `chardetng` の推定（encoding_rs と同作者・Firefox 実績）
//!
//! 5 まで落ちるのは「宣言が無く UTF-8 でもない」ページだけで、そこは推定しか手がない。

use encoding_rs::Encoding;

/// `<meta charset>` の探索範囲。HTML5 は「先頭 1024 バイト以内」を要求するが、
/// 長いライセンスコメントを先頭に置くサイトが実在するため少し広く見る。
const META_SCAN_BYTES: usize = 4096;

/// デコード結果。
pub(super) struct Decoded {
    pub text: String,
    /// 実際に使ったエンコーディング名（観測・デバッグ用）。
    pub encoding: &'static str,
}

/// 本文バイト列を文字列へ落とす。判定不能でも**必ず成功する**（推定にフォールバックする）。
pub(super) fn decode(body: &[u8], content_type: Option<&str>) -> Decoded {
    let declared = Encoding::for_bom(body)
        .map(|(enc, _)| enc)
        .or_else(|| from_content_type(content_type))
        .or_else(|| from_meta(body));
    // BOM 付きの一致は `decode` が自分で剥がす（宣言より BOM が優先されるのも仕様どおり）。
    let encoding = declared.unwrap_or_else(|| guess(body));
    let (text, actual, _had_errors) = encoding.decode(body);
    Decoded {
        text: text.into_owned(),
        encoding: actual.name(),
    }
}

/// `text/html; charset=shift_jis` の charset を引く。
///
/// **全パラメータを最後まで走査する**。charset より前に別パラメータを置くサーバ
/// （`text/html; version=1; Charset=Shift_JIS`）や、`Charset=` / `charset = ` のような
/// 綴りが実在する。先頭パラメータだけ見て打ち切ると宣言を取りこぼし、推定へ落ちて
/// 短いページが文字化けする。
fn from_content_type(content_type: Option<&str>) -> Option<&'static Encoding> {
    content_type?
        .split(';')
        .skip(1)
        .filter_map(|param| param.split_once('='))
        .filter(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
        .find_map(|(_, value)| Encoding::for_label(trim_label(value).as_bytes()))
}

/// 先頭 [`META_SCAN_BYTES`] から `<meta>` の charset 宣言を引く。
///
/// タグを正しくパースはしない（この時点ではまだ文字列にできていない）。ASCII 範囲だけを見て
/// `charset=` の値を拾う軽量スキャンで、誤検出しても次段（推定）が受け止める。
fn from_meta(body: &[u8]) -> Option<&'static Encoding> {
    let head = &body[..body.len().min(META_SCAN_BYTES)];
    let ascii: String = head
        .iter()
        .map(|&b| if b.is_ascii() { b as char } else { ' ' })
        .collect();
    let lower = ascii.to_ascii_lowercase();
    // `<meta` の中の charset だけを見る（本文中の "charset=" を拾わない）。
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find("<meta") {
        let start = from + rel;
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        if let Some(pos) = lower[start..end].find("charset") {
            // `charset` に `=` が続かない meta（`name="charset-note"` 等）は**読み飛ばす**。
            // ここで打ち切ると、後続の本物の宣言まで見えなくなる。
            let value = ascii[start + pos + "charset".len()..end]
                .trim_start()
                .strip_prefix('=');
            if let Some(enc) = value.and_then(|v| Encoding::for_label(trim_label(v).as_bytes())) {
                return Some(enc);
            }
        }
        from = end.max(start + 1);
    }
    None
}

/// charset ラベルから引用符・末尾のごみを落とす。
fn trim_label(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim()
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != ';' && *c != '"' && *c != '\'' && *c != '/')
        .collect()
}

/// 宣言が無いときの推定。妥当な UTF-8 を最優先し、それ以外は `chardetng` に委ねる。
fn guess(body: &[u8]) -> &'static Encoding {
    if is_utf8_ignoring_truncation(body) {
        return encoding_rs::UTF_8;
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(body, true);
    detector.guess(None, true)
}

/// 「末尾で切れた多バイト文字」を除けば妥当な UTF-8 か。
///
/// 本文はサイズ上限で**途中で打ち切られる**ため、末尾の不完全なシーケンスだけで
/// 「UTF-8 ではない」と判定してはいけない（打ち切りが推定を狂わせる）。
fn is_utf8_ignoring_truncation(body: &[u8]) -> bool {
    match std::str::from_utf8(body) {
        Ok(_) => true,
        // error_len() == None は「入力の末尾で切れた」＝打ち切りによるもの。
        Err(e) => e.error_len().is_none(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn prefers_bom_over_declaration() {
        let mut body = vec![0xEF, 0xBB, 0xBF];
        body.extend_from_slice("あ".as_bytes());
        let d = decode(&body, Some("text/html; charset=shift_jis"));
        assert_eq!(d.text, "あ");
        assert_eq!(d.encoding, "UTF-8");
    }

    #[test]
    fn decodes_shift_jis_from_content_type() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("日本語のページ");
        let d = decode(&bytes, Some("text/html; charset=Shift_JIS"));
        assert_eq!(d.text, "日本語のページ");
        assert_eq!(d.encoding, "Shift_JIS");
        // 旧実装（from_utf8_lossy）だと置換文字だらけになることを対比で示す。
        assert!(String::from_utf8_lossy(&bytes).contains('\u{FFFD}'));
    }

    #[test]
    fn decodes_euc_jp_from_meta_tag() {
        let (body, _, _) = encoding_rs::EUC_JP.encode(
            "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=EUC-JP\">\
             </head><body>本文です</body></html>",
        );
        let d = decode(&body, Some("text/html"));
        assert!(d.text.contains("本文です"), "{}", d.text);
        assert_eq!(d.encoding, "EUC-JP");
    }

    #[test]
    fn falls_back_to_detection_without_declaration() {
        let (body, _, _) = encoding_rs::SHIFT_JIS.encode(
            "これは日本語の文章です。宣言が無くても読めるようにしたい。日本語の文字が続きます。",
        );
        let d = decode(&body, None);
        assert!(d.text.contains("日本語"), "{} / {}", d.encoding, d.text);
    }

    #[test]
    fn truncated_utf8_stays_utf8() {
        // 末尾で多バイト文字が切れていても UTF-8 と判定する（推定に流さない）。
        let full = "日本語テキストの本文がここにあります".as_bytes().to_vec();
        let cut = &full[..full.len() - 1];
        let d = decode(cut, None);
        assert_eq!(d.encoding, "UTF-8");
        assert!(d.text.starts_with("日本語テキスト"));
    }

    #[test]
    fn ignores_charset_like_text_outside_meta() {
        let body = "<html><body>charset=shift_jis という文字列は本文です</body></html>";
        assert_eq!(decode(body.as_bytes(), None).encoding, "UTF-8");
    }

    /// charset より前に別パラメータがあり、綴りが大文字でも宣言を見つける。
    #[test]
    fn scans_every_content_type_parameter() {
        let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("日本語のページ");
        let d = decode(&bytes, Some("text/html; version=1; Charset=Shift_JIS"));
        assert_eq!(d.encoding, "Shift_JIS");
        assert_eq!(d.text, "日本語のページ");
    }

    /// 空白を挟む緩い記法（`charset = "euc-jp"`）も拾う。
    #[test]
    fn accepts_loose_charset_spelling() {
        let (bytes, _, _) = encoding_rs::EUC_JP.encode("本文です");
        let d = decode(&bytes, Some("text/html; charset = \"euc-jp\""));
        assert_eq!(d.encoding, "EUC-JP");
        assert_eq!(d.text, "本文です");
    }

    /// `charset` を含むが宣言ではない meta が**先に**あっても、後続の本物を見つける。
    #[test]
    fn keeps_scanning_past_a_meta_without_charset_value() {
        let (body, _, _) = encoding_rs::SHIFT_JIS.encode(
            "<html><head><meta name=\"charset-note\" content=\"none\">\
             <meta charset=\"Shift_JIS\"></head><body>本文です</body></html>",
        );
        let d = decode(&body, Some("text/html"));
        assert_eq!(d.encoding, "Shift_JIS");
        assert!(d.text.contains("本文です"), "{}", d.text);
    }
}
