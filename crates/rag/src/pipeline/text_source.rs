//! 文章のファイルを worker へ渡す前に、文字コードを決めて UTF-8 にする（GAP-46・D-118）。
//!
//! worker（Docling・text/plain の軽量経路）は、渡されたバイト列を UTF-8 として読む。Shift_JIS のテキストは文字化け
//! したまま索引に入り、Shift_JIS の CSV・Markdown は `parse_failed` になり、BOM 付きの UTF-8 は先頭の U+FEFF が
//! 残っていた。判定を worker に持たせず（2 か所に持たない）、ワークフローの「ファイルを読む」・CSV の集計と同じ
//! `textenc` の判定で決めて、UTF-8 のバイト列（`ParseSource::Bytes`）で渡す。
//!
//! HTML だけは扱いが違う。HTML の読み手（BeautifulSoup）は、BOM が無ければ `<meta charset>` の宣言を先に使うので、
//! Shift_JIS と宣言したページを UTF-8 に直して渡すと、宣言のほうで読まれて文字化けする。UTF-8 に直したときは
//! 先頭に BOM を付けて渡す（BOM は宣言より先に見る）。文字コードを決められない HTML は、これまでどおり原本を
//! 渡して宣言で読ませる（西欧の文字コードで宣言したページなど。`textenc` は日本の文字コードだけを推定する）。

use crate::error::RagError;

/// worker へ渡す本文（文字コードを決めて UTF-8 にしたもの）。
pub(super) enum TextForWorker {
    /// UTF-8 にした本文（BOM は含めない。HTML だけは先頭に BOM を付ける）。
    Utf8(Vec<u8>),
    /// 原本をそのまま渡す（文字コードを決められない HTML。宣言は HTML の読み手が読む）。
    Original,
}

/// 文字コードを決めて worker へ渡す本文にする種類か（`text/plain`・`text/csv`・`text/markdown`・`text/html`）。
pub(super) fn is_text_type(base_type: &str) -> bool {
    base_type.starts_with("text/")
}

/// UTF-8 の BOM。
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// 文章のファイルの中身を、worker へ渡す本文にする。
///
/// 文章として読めない中身（制御文字を含む）と、文字コードを決められない中身（HTML を除く）は、恒久的なパースの
/// 失敗（[`RagError::Parse`]）にする。文字化けしたまま索引に入れない。UTF-8 にすると大きくなるので（Shift_JIS の
/// 2 バイトの文字は 3 バイトになる）、`max_bytes` を超えたら worker の上限と同じ理由で断る。
pub(super) fn text_for_worker(
    bytes: &[u8],
    base_type: &str,
    max_bytes: i64,
) -> Result<TextForWorker, RagError> {
    let html = base_type == "text/html";
    let decoded = match textenc::decode_text(bytes) {
        Ok(decoded) => decoded,
        Err(textenc::NotText::UnknownEncoding) if html => return Ok(TextForWorker::Original),
        Err(textenc::NotText::UnknownEncoding) => {
            return Err(RagError::Parse {
                code: "unknown_encoding".into(),
                detail: "文字コードを決められません（UTF-8・Shift_JIS・EUC-JP・UTF-16 のどれとしても読めません）"
                    .into(),
            })
        }
        Err(textenc::NotText::Binary) => {
            return Err(RagError::Parse {
                code: "not_text".into(),
                detail: "文章として読めない中身です（文章に入らない制御文字を含みます）".into(),
            })
        }
    };
    let mut body = Vec::with_capacity(decoded.text.len() + if html { UTF8_BOM.len() } else { 0 });
    if html {
        body.extend_from_slice(UTF8_BOM);
    }
    body.extend_from_slice(decoded.text.as_bytes());
    if i64::try_from(body.len()).unwrap_or(i64::MAX) > max_bytes {
        return Err(RagError::Parse {
            code: "source_too_large".into(),
            detail: format!(
                "UTF-8 に直すと取り込みの上限（{max_bytes} バイト）を超えます（元の文字コード: {}）",
                decoded.encoding.name()
            ),
        });
    }
    Ok(TextForWorker::Utf8(body))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use textenc::encoding_rs::{EUC_JP, SHIFT_JIS};

    fn utf8(result: Result<TextForWorker, RagError>) -> String {
        match result.unwrap() {
            TextForWorker::Utf8(body) => String::from_utf8(body).unwrap(),
            TextForWorker::Original => panic!("原本をそのまま渡した"),
        }
    }

    const MAX: i64 = 1 << 20;

    /// Excel や Windows のメモ帳が保存する文字コードの文章を、BOM の無い UTF-8 にして渡す。
    #[test]
    fn japanese_encodings_and_bom_become_plain_utf8() {
        let csv = "経費,金額\n交通費,1200\n";
        let (sjis, _, _) = SHIFT_JIS.encode(csv);
        assert_eq!(utf8(text_for_worker(&sjis, "text/csv", MAX)), csv);
        let (euc, _, _) = EUC_JP.encode("議事録の本文です。\n");
        assert_eq!(
            utf8(text_for_worker(&euc, "text/plain", MAX)),
            "議事録の本文です。\n"
        );
        // MS-DOS の頃の帳票（末尾に EOF の印の 0x1A）も取り込む（恒久的な失敗にしない）。
        let (dos, _, _) = SHIFT_JIS.encode("売上日報\r\n合計,12000\r\n\u{1A}");
        assert_eq!(
            utf8(text_for_worker(&dos, "text/plain", MAX)),
            "売上日報\r\n合計,12000\r\n\u{1A}"
        );
        let bom = [UTF8_BOM, csv.as_bytes()].concat();
        let read = utf8(text_for_worker(&bom, "text/csv", MAX));
        assert_eq!(read, csv);
        assert!(!read.starts_with('\u{FEFF}'));
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("# 見出し\n".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            utf8(text_for_worker(&utf16, "text/markdown", MAX)),
            "# 見出し\n"
        );
    }

    /// HTML は、UTF-8 にしたら BOM を付けて渡す（`<meta charset>` の宣言より BOM を先に見させる）。
    /// 文字コードを決められない HTML は原本を渡し、宣言で読ませる。
    #[test]
    fn html_gets_a_bom_or_is_left_to_its_declaration() {
        let page =
            "<html><head><meta charset=\"Shift_JIS\"></head><body>社内規程の案内</body></html>";
        let (sjis, _, _) = SHIFT_JIS.encode(page);
        let read = utf8(text_for_worker(&sjis, "text/html", MAX));
        assert_eq!(read, format!("\u{FEFF}{page}"));
        let latin =
            b"<html><head><meta charset=\"windows-1252\"></head><body>caf\xe9</body></html>";
        assert!(matches!(
            text_for_worker(latin, "text/html", MAX).unwrap(),
            TextForWorker::Original
        ));
    }

    /// 読めない中身は、文字化けしたまま渡さずに恒久的な失敗にする。
    #[test]
    fn unreadable_text_is_a_permanent_parse_failure() {
        let code = |r: Result<TextForWorker, RagError>| match r {
            Err(RagError::Parse { code, .. }) => code,
            _ => panic!("恒久的な失敗にならなかった"),
        };
        assert_eq!(
            code(text_for_worker(b"caf\xe9\n", "text/plain", MAX)),
            "unknown_encoding"
        );
        assert_eq!(
            code(text_for_worker(b"abc\x00def", "text/csv", MAX)),
            "not_text"
        );
        // UTF-8 にすると上限を超える（Shift_JIS の 2 バイトの文字は 3 バイトになる）。
        let (sjis, _, _) = SHIFT_JIS.encode("あいうえお");
        assert_eq!(sjis.len(), 10);
        assert_eq!(
            code(text_for_worker(&sjis, "text/plain", 12)),
            "source_too_large"
        );
        assert!(matches!(
            text_for_worker(&sjis, "text/plain", 15).unwrap(),
            TextForWorker::Utf8(_)
        ));
        assert!(is_text_type("text/csv") && !is_text_type("application/pdf"));
    }
}
