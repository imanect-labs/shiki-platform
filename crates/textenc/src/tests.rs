#![allow(clippy::unwrap_used)]

use encoding_rs::{BIG5, EUC_JP, EUC_KR, GBK, SHIFT_JIS, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252};

use super::*;

/// 経費の CSV（日本語の Windows の Excel が既定で保存する形・Shift_JIS）。
const EXPENSES: &str = "経費,金額\n交通費,1200\n宿泊費,8000\n";

fn encoded(encoding: &'static Encoding, text: &str) -> Vec<u8> {
    let (bytes, _, unmappable) = encoding.encode(text);
    assert!(!unmappable, "{text}");
    bytes.into_owned()
}

fn utf16(text: &str, le: bool, with_bom: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if with_bom {
        out.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
    }
    for u in text.encode_utf16() {
        out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
    }
    out
}

#[test]
fn reads_shift_jis_csv_from_excel() {
    let bytes = encoded(SHIFT_JIS, EXPENSES);
    // 以前の読み方（from_utf8_lossy）では置換文字だらけになる。
    assert!(String::from_utf8_lossy(&bytes).contains('\u{FFFD}'));
    let d = decode_text(&bytes).unwrap();
    assert_eq!(d.text, EXPENSES);
    assert_eq!(d.encoding.name(), "Shift_JIS");
    assert!(!d.bom);
    // 見出しの行だけの短い CSV も読める。
    assert_eq!(
        decode_text(&encoded(SHIFT_JIS, "経費,金額\n"))
            .unwrap()
            .text,
        "経費,金額\n"
    );
}

/// CP932 の拡張（NEC の特殊文字・IBM の拡張漢字）と半角カナも読める（Excel・業務の古い仕組みが書く）。
#[test]
fn reads_cp932_extensions_and_halfwidth_kana() {
    for text in [
        "①②③,㈱\n",
        "髙橋,﨑\n",
        "ｶ)ｼｷ,ﾌﾘｺﾐ\n",
        "申請者,部署,金額\n山田太郎,営業部,12000\n",
    ] {
        let d = decode_text(&encoded(SHIFT_JIS, text)).unwrap();
        assert_eq!((d.text.as_str(), d.encoding), (text, SHIFT_JIS));
    }
}

#[test]
fn reads_bom_utf8_and_utf16() {
    let mut bom8 = b"\xEF\xBB\xBF".to_vec();
    bom8.extend_from_slice(EXPENSES.as_bytes());
    let d = decode_text(&bom8).unwrap();
    // BOM は文章に含めない（以前は先頭に U+FEFF が残っていた）。
    assert_eq!(
        (d.text.as_str(), d.encoding, d.bom),
        (EXPENSES, UTF_8, true)
    );

    let d = decode_text(&utf16(EXPENSES, true, true)).unwrap();
    assert_eq!(
        (d.text.as_str(), d.encoding, d.bom),
        (EXPENSES, UTF_16LE, true)
    );
    let d = decode_text(&utf16(EXPENSES, false, true)).unwrap();
    assert_eq!(
        (d.text.as_str(), d.encoding, d.bom),
        (EXPENSES, UTF_16BE, true)
    );
}

#[test]
fn reads_plain_utf8_as_before() {
    let d = decode_text(EXPENSES.as_bytes()).unwrap();
    assert_eq!(
        (d.text.as_str(), d.encoding, d.bom),
        (EXPENSES, UTF_8, false)
    );
    // UTF-8 は NUL だけで断る（ログの色の指定などの制御文字は文章として読む・以前と同じ）。
    assert_eq!(
        decode_text(b"\x1b[31merror\x1b[0m\n").unwrap().encoding,
        UTF_8
    );
    assert_eq!(decode_text(b"a\0b"), Err(NotText::Binary));
    assert_eq!(decode_text(b"").unwrap().text, "");
}

/// 日本語が大きな ASCII の後から始まっても、推定の窓は最初の ASCII でないバイトから取る。
#[test]
fn finds_japanese_after_a_long_ascii_prefix() {
    use std::fmt::Write as _;
    let mut text = "id,amount\n".to_string();
    for i in 0..200_000 {
        let _ = writeln!(text, "{i},100");
    }
    text.push_str("合計,20000000\n");
    let bytes = encoded(SHIFT_JIS, &text);
    assert!(bytes.len() > GUESS_WINDOW_BYTES);
    let d = decode_text(&bytes).unwrap();
    assert_eq!(d.encoding, SHIFT_JIS);
    assert!(d.text.ends_with("合計,20000000\n"));
}

#[test]
fn reads_euc_jp_when_the_guess_is_clear() {
    let text = "申請者,部署,金額,日付\n山田太郎,営業部,12000,2026-10-01\n佐藤花子,経理部,3400,2026-10-02\n";
    let d = decode_text(&encoded(EUC_JP, text)).unwrap();
    assert_eq!((d.text.as_str(), d.encoding), (text, EUC_JP));
}

/// 推定の確からしさが低いとき（手がかり無しでは別の東アジアの文字コードと答える）は断る。
/// 短い EUC-JP は EUC-KR とも読め、中国語や韓国語の文章は手がかりを付けると日本語の文字化けとして読めてしまう。
#[test]
fn refuses_when_the_guess_is_not_clear() {
    for bytes in [
        encoded(EUC_JP, EXPENSES),
        encoded(GBK, "你好世界，这是中文。"),
        encoded(GBK, "中文,金额\n交通费,1200\n"),
        encoded(EUC_KR, "한국어,금액\n교통비,1200\n"),
        encoded(BIG5, "中文,金額\n交通費,1200\n"),
    ] {
        assert_eq!(
            decode_text(&bytes),
            Err(NotText::UnknownEncoding),
            "{bytes:?}"
        );
    }
}

/// 日本の文字コードでないもの（西欧の文字コード）は読まない（「UTF-8 と Shift_JIS」と言っている）。
#[test]
fn refuses_non_japanese_legacy_encodings() {
    for text in ["Müller,Straße\nCafé,Crème brûlée\n", "café", "Ærø,Øl\n"] {
        assert_eq!(
            decode_text(&encoded(WINDOWS_1252, text)),
            Err(NotText::UnknownEncoding),
            "{text}"
        );
    }
}

#[test]
fn refuses_binaries() {
    let mut random = Vec::new();
    let mut x: u32 = 12345;
    for _ in 0..512 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
        random.push((x >> 16) as u8);
    }
    for bytes in [
        b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj\n".to_vec(),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec(),
        b"PK\x03\x04\x14\x00\x06\x00".to_vec(),
        random,
    ] {
        assert!(decode_text(&bytes).is_err(), "{bytes:?}");
    }
    // Shift_JIS として読めても、NUL を含むものは文章にしない。
    let mut sjis = encoded(SHIFT_JIS, "経費");
    sjis.extend_from_slice(b"\x00\x02");
    assert_eq!(decode_text(&sjis), Err(NotText::Binary));
    // BOM で始まっても、UTF-16 として壊れているもの・制御文字だらけのものは断る。
    assert_eq!(decode_text(b"\xFF\xFEa"), Err(NotText::UnknownEncoding));
    assert_eq!(
        decode_text(b"\xFF\xFE\x00\x00\x01\x00"),
        Err(NotText::Binary)
    );
}

/// MS-DOS の頃の帳票の末尾の EOF の印（0x1A）や、ログの色の指定（ESC）を含む Shift_JIS・EUC-JP の文章は読める
/// （UTF-8 と同じく NUL だけで断る・以前の画面でも見られた）。書き戻しても同じバイト列になる。
#[test]
fn reads_shift_jis_with_dos_eof_and_escape() {
    for (encoding, text) in [
        (SHIFT_JIS, "売上日報\r\n合計,12000\r\n\u{1A}"),
        (
            SHIFT_JIS,
            "\u{1B}[31m異常\u{1B}[0m 在庫が発注点を下回りました。倉庫の在庫を確かめてください。\n",
        ),
        (
            EUC_JP,
            "議事録の本文です。次の会議までに資料を用意します。担当者は営業部です。\n\u{1A}",
        ),
    ] {
        let bytes = encoded(encoding, text);
        let d = decode_text(&bytes);
        assert!(d.is_ok(), "{text:?}: {d:?}");
        let d = d.unwrap();
        assert_eq!((d.text.as_str(), d.encoding), (text, encoding));
        assert_eq!(encode_like(&d.text, d.encoding, d.bom).unwrap(), bytes);
    }
}

#[test]
fn utf8_check_can_allow_a_cut_tail() {
    let full = "日本語".as_bytes();
    let cut = &full[..full.len() - 1];
    assert!(is_utf8(full, false));
    assert!(!is_utf8(cut, false));
    assert!(is_utf8(cut, true));
    assert!(!is_utf8(b"\xff\xfe", true));
}

/// ウェブのページの推定は、読めなくても必ず何かを返す（断らない）。
#[test]
fn guess_any_always_answers() {
    assert_eq!(guess_any("日本語".as_bytes()), UTF_8);
    let sjis = encoded(
        SHIFT_JIS,
        "これは日本語の文章です。宣言が無くても読めるようにしたい。日本語の文字が続きます。",
    );
    assert_eq!(guess_any(&sjis), SHIFT_JIS);
    assert!(!guess_any(b"\x89PNG\r\n\x1a\n").name().is_empty());
}

#[test]
fn writes_back_in_the_same_encoding() {
    let edited = "経費,金額\n交通費,1500\n";
    for (encoding, with_bom) in [
        (SHIFT_JIS, false),
        (UTF_8, false),
        (UTF_8, true),
        (UTF_16LE, true),
        (UTF_16BE, true),
    ] {
        let bytes = encode_like(edited, encoding, with_bom).unwrap();
        let d = decode_text(&bytes).unwrap();
        assert_eq!(
            (d.text.as_str(), d.encoding, d.bom),
            (edited, encoding, with_bom),
            "{}",
            encoding.name()
        );
    }
    // 日本の文字コードは encoding_rs と同じバイト列になる（短い EUC-JP は推定で断るので、バイト列で比べる）。
    assert_eq!(
        encode_like(edited, SHIFT_JIS, false).unwrap(),
        encoded(SHIFT_JIS, edited)
    );
    assert_eq!(
        encode_like(edited, EUC_JP, false).unwrap(),
        encoded(EUC_JP, edited)
    );
    assert_eq!(
        encode_like("A", UTF_16LE, false).unwrap(),
        vec![0x41, 0x00],
        "BOM が無ければ付けない"
    );
}

/// その文字コードに無い文字は書かない（`&#…;` に置き換えて中身を変えない）。
#[test]
fn refuses_characters_the_encoding_cannot_hold() {
    assert_eq!(
        encode_like("経費,😀\n", SHIFT_JIS, false),
        Err(Unmappable('😀'))
    );
    assert_eq!(encode_like("한국어", EUC_JP, false), Err(Unmappable('한')));
    // UTF-8・UTF-16 はどの文字も書ける。
    assert!(encode_like("😀", UTF_8, false).is_ok());
    assert!(encode_like("😀", UTF_16LE, true).is_ok());
}

/// Mac の入力でよく出る、Shift_JIS（CP932）では全角の別の文字で持つ記号は、見た目の同じ文字に置き換えて書く
/// （〜 → ～ など）。読むと 0x8160 は ～ に見えるので、利用者には同じ文字なのに書けないと言わない。
#[test]
fn writes_mac_style_symbols_as_their_cp932_look_alikes() {
    let pairs = [
        ('\u{301C}', '\u{FF5E}'),
        ('\u{2014}', '\u{2015}'),
        ('\u{2016}', '\u{2225}'),
        ('\u{00A2}', '\u{FFE0}'),
        ('\u{00A3}', '\u{FFE1}'),
        ('\u{00AC}', '\u{FFE2}'),
    ];
    for encoding in [SHIFT_JIS, EUC_JP] {
        for (mac, cp932) in pairs {
            let text = format!("10時{mac}12時");
            let bytes = encode_like(&text, encoding, false);
            assert_eq!(
                bytes,
                Ok(encoded(encoding, &format!("10時{cp932}12時"))),
                "{} {mac:?}",
                encoding.name()
            );
        }
    }
    // UTF-8・UTF-16 は置き換えない（どの文字も書ける）。
    assert_eq!(
        encode_like("\u{301C}", UTF_8, false).unwrap(),
        "\u{301C}".as_bytes()
    );
}
