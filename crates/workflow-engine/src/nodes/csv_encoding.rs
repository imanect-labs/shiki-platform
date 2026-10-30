//! 新しく保存する CSV の中身を、選んだ文字コードのバイト列にする（`csv.write`・`storage.write` の CSV・GAP-47・D-120）。
//!
//! 日本語の Windows の Excel は、BOM の無い UTF-8 の CSV を Shift_JIS として開いて文字化けする。保存の欄「文字コード」で
//! UTF-8（BOM 付き）・UTF-8・Shift_JIS を選ぶ。書き方は CSV の編集の書き戻しと同じ `textenc::encode_like`
//! （Shift_JIS に無い文字は書かずに断る・〜 などは見た目の同じ全角の文字にする）。

use serde_json::Value;
use textenc::encoding_rs::{SHIFT_JIS, UTF_8};

use super::ports::PortError;
use super::resolver::{as_base64_bytes, as_text};
use crate::ir::params::CsvEncoding;

/// 種類（Content-Type）が CSV か（パラメーター〔`; charset=…`〕は見ない）。
#[must_use]
pub(super) fn is_csv_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|base| base.trim().eq_ignore_ascii_case("text/csv"))
}

/// 内容（解決した値）を、選んだ文字コードの CSV のバイト列にする。
///
/// 文章（文字列と、リストや項目のまとまりを直列化した JSON の文字）だけを書き直す。`{ "base64": … }` のバイト列は、
/// 作成者が決めたバイト列なのでそのまま保存する。BOM を付けるときは、内容の先頭に BOM の文字（U+FEFF）があれば
/// 重ねない。
pub(super) fn csv_bytes(content: &Value, encoding: CsvEncoding) -> Result<Vec<u8>, PortError> {
    if let Some(bytes) = as_base64_bytes(content) {
        return Ok(bytes);
    }
    let text = as_text(content);
    let (target, bom) = match encoding {
        CsvEncoding::Utf8 => return Ok(text.into_bytes()),
        CsvEncoding::Utf8Bom => (UTF_8, true),
        CsvEncoding::ShiftJis => (SHIFT_JIS, false),
    };
    let body = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    textenc::encode_like(body, target, bom).map_err(|textenc::Unmappable(c)| {
        PortError::invalid(format!(
            "「{c}」は Shift_JIS で書けないため、CSV を保存できませんでした。「文字コード」を「UTF-8（BOM 付き）」にするか、\
             別の文字に置き換えてください。"
        ))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn encodes_csv_text_in_the_chosen_encoding() {
        let csv = json!("品目,金額\n交通費,1200\n");
        assert_eq!(
            csv_bytes(&csv, CsvEncoding::Utf8).unwrap(),
            "品目,金額\n交通費,1200\n".as_bytes()
        );
        let bom = csv_bytes(&csv, CsvEncoding::Utf8Bom).unwrap();
        assert_eq!(
            bom,
            [
                b"\xEF\xBB\xBF".as_slice(),
                "品目,金額\n交通費,1200\n".as_bytes()
            ]
            .concat()
        );
        let (sjis, _, _) = SHIFT_JIS.encode("品目,金額\n交通費,1200\n");
        assert_eq!(
            csv_bytes(&csv, CsvEncoding::ShiftJis).unwrap(),
            sjis.as_ref()
        );
        // 内容の先頭に BOM の文字があっても重ねない。
        assert_eq!(
            csv_bytes(&json!("\u{FEFF}a,b\n"), CsvEncoding::Utf8Bom).unwrap(),
            b"\xEF\xBB\xBFa,b\n"
        );
        // 〜（Mac の入力）は Shift_JIS の ～ にする（CSV の編集の書き戻しと同じ）。
        let (wave, _, _) = SHIFT_JIS.encode("期間\n4\u{FF5E}6月\n");
        assert_eq!(
            csv_bytes(&json!("期間\n4\u{301C}6月\n"), CsvEncoding::ShiftJis).unwrap(),
            wave.as_ref()
        );
    }

    #[test]
    fn keeps_base64_bytes_and_refuses_unmappable_characters() {
        // base64 のバイト列は作成者が決めたものなので書き直さない。
        assert_eq!(
            csv_bytes(&json!({ "base64": "YSxiCg==" }), CsvEncoding::ShiftJis).unwrap(),
            b"a,b\n"
        );
        // リストや項目のまとまりは JSON の文字として書く（以前と同じ）。
        assert_eq!(
            csv_bytes(&json!(["a"]), CsvEncoding::Utf8Bom).unwrap(),
            b"\xEF\xBB\xBF[\"a\"]"
        );
        let err = csv_bytes(&json!("名前\n😀\n"), CsvEncoding::ShiftJis).unwrap_err();
        assert_eq!(err.code, "invalid");
        assert!(!err.retryable);
        assert!(
            err.message.starts_with("「😀」は Shift_JIS で書けない"),
            "{}",
            err.message
        );
        assert!(is_csv_type("text/csv; charset=utf-8") && is_csv_type("TEXT/CSV"));
        assert!(!is_csv_type("text/plain") && !is_csv_type("application/vnd.ms-excel"));
    }
}
