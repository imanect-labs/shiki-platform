//! 新しく保存する CSV の中身を、選んだ文字コードのバイト列にする（`csv.write`・`storage.write` の CSV・GAP-47・D-120）。
//!
//! 日本語の Windows の Excel は、BOM の無い UTF-8 の CSV を Shift_JIS として開いて文字化けする。保存の欄「文字コード」で
//! UTF-8（BOM 付き）・UTF-8・Shift_JIS を選ぶ。書き方は CSV の編集の書き戻しと同じ `textenc::encode_like`
//! （Shift_JIS に無い文字は書かずに断る・〜 などは見た目の同じ全角の文字にする）。

use serde_json::Value;
use textenc::encoding_rs::{SHIFT_JIS, UTF_8};

use super::csv_compose::csv_text;
use super::ports::PortError;
use super::resolver::as_base64_bytes;
use crate::ir::params::CsvEncoding;

/// 種類（Content-Type）が CSV か（パラメーター〔`; charset=…`〕は見ない）。
#[must_use]
pub(super) fn is_csv_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|base| base.trim().eq_ignore_ascii_case("text/csv"))
}

/// 内容（解決した値）を、選んだ文字コードの CSV のバイト列にする。`label` は内容の欄の見出し（誤りの文で名指しする）。
///
/// 文章（文字列と、表や項目のまとまりのリストを組み立てた CSV・[`csv_text`]）だけを書き直す。`{ "base64": … }` の
/// バイト列は、作成者が決めたバイト列なのでそのまま保存する。BOM を付けるときは、内容の先頭に BOM の文字（U+FEFF）が
/// あれば重ねない。CSV にできない値（数・表でない項目のまとまりなど）は `invalid` で断る（BUG-178）。
pub(super) fn csv_bytes(
    content: &Value,
    encoding: CsvEncoding,
    label: &str,
) -> Result<Vec<u8>, PortError> {
    if let Some(bytes) = as_base64_bytes(content) {
        return Ok(bytes);
    }
    let text = csv_text(content, label)?;
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

    const L: &str = "CSV の内容";

    #[test]
    fn encodes_csv_text_in_the_chosen_encoding() {
        let csv = json!("品目,金額\n交通費,1200\n");
        assert_eq!(
            csv_bytes(&csv, CsvEncoding::Utf8, L).unwrap(),
            "品目,金額\n交通費,1200\n".as_bytes()
        );
        let bom = csv_bytes(&csv, CsvEncoding::Utf8Bom, L).unwrap();
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
            csv_bytes(&csv, CsvEncoding::ShiftJis, L).unwrap(),
            sjis.as_ref()
        );
        // 内容の先頭に BOM の文字があっても重ねない。
        assert_eq!(
            csv_bytes(&json!("\u{FEFF}a,b\n"), CsvEncoding::Utf8Bom, L).unwrap(),
            b"\xEF\xBB\xBFa,b\n"
        );
        // 〜（Mac の入力）は Shift_JIS の ～ にする（CSV の編集の書き戻しと同じ）。
        let (wave, _, _) = SHIFT_JIS.encode("期間\n4\u{FF5E}6月\n");
        assert_eq!(
            csv_bytes(&json!("期間\n4\u{301C}6月\n"), CsvEncoding::ShiftJis, L).unwrap(),
            wave.as_ref()
        );
    }

    #[test]
    fn keeps_base64_bytes_and_refuses_unmappable_characters() {
        // base64 のバイト列は作成者が決めたものなので書き直さない。
        assert_eq!(
            csv_bytes(&json!({ "base64": "YSxiCg==" }), CsvEncoding::ShiftJis, L).unwrap(),
            b"a,b\n"
        );
        // 表と項目のまとまりのリストは CSV に組み立ててから、選んだ文字コードで書く（JSON の文字にしない・BUG-178）。
        let table = json!({ "columns": ["品目", "金額"], "rows": [["交通費", "1200"]] });
        let (sjis, _, _) = SHIFT_JIS.encode("品目,金額\r\n交通費,1200\r\n");
        assert_eq!(
            csv_bytes(&table, CsvEncoding::ShiftJis, L).unwrap(),
            sjis.as_ref()
        );
        assert_eq!(
            csv_bytes(&json!([{ "a": 1 }]), CsvEncoding::Utf8Bom, L).unwrap(),
            b"\xEF\xBB\xBFa\r\n1\r\n"
        );
        // CSV にできない値は JSON の文字のまま保存せずに断る。
        let err = csv_bytes(&json!(["a"]), CsvEncoding::Utf8Bom, L).unwrap_err();
        assert_eq!(err.code, "invalid");
        assert!(
            err.message.starts_with("「CSV の内容」のリストの 1 件目が"),
            "{}",
            err.message
        );
        let err = csv_bytes(&json!("名前\n😀\n"), CsvEncoding::ShiftJis, L).unwrap_err();
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
