//! トークナイザの単体テスト。
//!
//! 合成した `DocumentText` ストリームでレコード構造を突く。実物に対する検証は
//! `tests/fidelity_it.rs` のゴールデン比較が担う。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::parse_document;
use crate::model::Block;

/// ユニット列を `DocumentText` のバイト列（マジック付き）にする。
fn stream(units: &[u16]) -> Vec<u8> {
    let mut bytes = b"SsmgV.01".to_vec();
    for unit in units {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

/// ヘッダ 6 ワード（内容は本文に影響しない）。
fn header() -> Vec<u16> {
    vec![0x0000, 0x0003, 0x0000, 0x0100, 0x0000, 0x011c]
}

/// `class` の段落レコードを `payload` 付きで組む。
fn record(class: u16, payload: &[u16]) -> Vec<u16> {
    let len = (payload.len() + super::MIN_RECORD_WORDS) as u16;
    let mut out = vec![0x001c, class, len];
    out.extend_from_slice(payload);
    out.extend_from_slice(&[len, 0x0000, class, 0x001f]);
    out
}

/// 文字列を UTF-16BE のユニット列にする。
fn text(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

fn paragraphs(raw: &[u8]) -> Vec<String> {
    parse_document(raw)
        .blocks()
        .iter()
        .map(|block| {
            let Block::Paragraph(paragraph) = block;
            paragraph.text()
        })
        .collect()
}

#[test]
fn reads_text_before_the_first_record() {
    // 上流はここを落とす。`f1.jtd` の 1 行目「様式第１（第７条関係）」がこの位置にある。
    let mut units = header();
    units.extend(text("様式第１（第７条関係）"));
    units.push(0x000a);
    units.extend(record(
        0x0010,
        &[0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000],
    ));
    units.extend(text("本文"));

    assert_eq!(
        paragraphs(&stream(&units)),
        vec!["様式第１（第７条関係）", "本文"]
    );
}

#[test]
fn paragraph_record_starts_a_new_paragraph() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("いち"));
    units.push(0x000a);
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("に"));

    assert_eq!(paragraphs(&stream(&units)), vec!["いち", "に"]);
}

#[test]
fn non_paragraph_records_do_not_split_paragraphs() {
    // 0x0000（インライン文脈）と 0x0030（表セルヘッダ）は段落を割らない。
    // 表としての再構成は JTD.3 の範囲で、ここでは構造として消費するだけ。
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("あ"));
    units.extend(record(0x0030, &[0x0000, 0x0006, 0x0082, 0x00ff, 0x0000]));
    units.extend(text("い"));
    units.extend(record(0x0000, &[0x0000, 0x0007, 0x00dc, 0x020d, 0x0000]));
    units.extend(text("う"));

    assert_eq!(paragraphs(&stream(&units)), vec!["あいう"]);
}

#[test]
fn cell_delimiter_separates_text_without_splitting_the_paragraph() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("左"));
    units.push(0x000e);
    units.extend(text("右"));

    assert_eq!(paragraphs(&stream(&units)), vec!["左\t右"]);
}

#[test]
fn keeps_ideographic_space_padding() {
    // 申請書の記入欄は全角スペースで組まれている。詰めると升目が消える。
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("氏　　名　　　　　"));

    assert_eq!(paragraphs(&stream(&units)), vec!["氏　　名　　　　　"]);
}

#[test]
fn joins_surrogate_pairs() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("𠮷田と𩸽"));

    assert_eq!(paragraphs(&stream(&units)), vec!["𠮷田と𩸽"]);
}

#[test]
fn drops_lone_surrogates() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("前"));
    units.push(0xd83d); // 対にならない high surrogate
    units.extend(text("後"));

    assert_eq!(paragraphs(&stream(&units)), vec!["前後"]);
}

#[test]
fn skips_inline_segments() {
    // ルビ・テンプレート差込は本文に混ぜない（取り込みは JTD.5）。
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("前"));
    units.push(0x001d);
    units.extend(text("ルビ"));
    units.push(0x001e);
    units.extend(text("後"));

    assert_eq!(paragraphs(&stream(&units)), vec!["前後"]);
}

#[test]
fn unterminated_inline_segment_does_not_swallow_the_document() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("前"));
    units.push(0x001d);
    units.extend(text("閉じない"));

    // 閉じが無いので 1 ユニットだけ進む＝以降が本文として読まれる。
    // 大事なのは「全部消えない」こと。
    let result = paragraphs(&stream(&units));
    assert_eq!(result.len(), 1);
    assert!(result[0].starts_with('前'), "先頭の本文が残ること");
}

#[test]
fn resynchronises_after_a_record_with_an_inconsistent_length_echo() {
    // 長さを詐称したレコード。**ここで文書を打ち切らない。**
    // `0x001C` は埋め込みオブジェクト等の非テキスト領域にも偶然現れ、`f1.jtd` では
    // 883 件の表セルレコードのうち 1 件がこの偽陽性だった。打ち切ると本文の後半が消える。
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("前"));
    units.extend_from_slice(&[
        0x001c, 0x0010, 0x000a, 0, 0, 0, 0xffff, 0x0000, 0x0010, 0x001f,
    ]);
    units.extend(text("後"));

    let joined = paragraphs(&stream(&units)).join("");

    assert!(joined.contains('前'), "壊れたレコードの前が残ること");
    assert!(joined.contains('後'), "壊れたレコードの後も読むこと");
}

#[test]
fn resynchronises_after_a_record_with_an_inconsistent_class_echo() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("前"));
    units.extend_from_slice(&[
        0x001c, 0x0010, 0x0008, 0x0000, 0x0008, 0x0000, 0x0030, 0x001f,
    ]);
    units.extend(text("後"));

    let joined = paragraphs(&stream(&units)).join("");

    assert!(joined.contains('前') && joined.contains('後'));
}

#[test]
fn record_longer_than_the_stream_does_not_truncate_the_document() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("前"));
    units.extend_from_slice(&[0x001c, 0x0010, 0xffff]);
    // 壊れたレコードの後は、次の正しいレコードで再同期する（実物もこの形）。
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("後"));

    let joined = paragraphs(&stream(&units)).join("");

    assert!(joined.contains('前') && joined.contains('後'));
}

#[test]
fn record_shorter_than_its_footer_does_not_truncate_the_document() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("前"));
    units.extend_from_slice(&[0x001c, 0x0010, 0x0003]);
    // 壊れたレコードの後は、次の正しいレコードで再同期する（実物もこの形）。
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("後"));

    let joined = paragraphs(&stream(&units)).join("");

    assert!(joined.contains('前') && joined.contains('後'));
}

#[test]
fn valid_record_payload_never_leaks_into_the_text() {
    // 正しいレコードのペイロードは本文ではない。文字として読めてしまう値を入れて確かめる。
    let mut units = header();
    units.extend(record(0x0010, &[0x6f22, 0x5b57, 0x0000]));
    units.extend(text("本文"));

    assert_eq!(paragraphs(&stream(&units)), vec!["本文"]);
}

#[test]
fn page_break_splits_paragraphs_without_losing_the_text_after_it() {
    // 0x000C は改ページ（RFC 0003）。制御コードとして扱うと直後の本文が落ちる。
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("前ページ"));
    units.push(0x000c);
    units.extend(text("次ページ"));

    assert_eq!(paragraphs(&stream(&units)), vec!["前ページ\n次ページ"]);
}

#[test]
fn bare_text_run_marker_reopens_the_text() {
    // レコードのフッタ（0x0005 0x0000 0x0001 0x001F）の形。0x001F を条件付きにすると
    // 直後の本文（f1.jtd では欄見出しの続き）が落ちる。
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("前"));
    units.extend_from_slice(&[0x0005, 0x0000, 0x0001, 0x001f]);
    units.extend(text("後"));

    assert_eq!(paragraphs(&stream(&units)), vec!["前後"]);
}

#[test]
fn keeps_display_text_of_recognised_inline_segments() {
    // 申請書の欄見出し（「連絡先」等）はこの形で入っている。捨てると本文から消える。
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend_from_slice(&[0x001c, 0x0001, 0x0007, 0x0000, 0x0000, 0x0003, 0x001d]);
    units.extend(text("連絡先"));
    units.push(0x001e);

    assert_eq!(paragraphs(&stream(&units)), vec!["連絡先"]);
}

#[test]
fn drops_display_text_of_unrecognised_inline_segments() {
    // セレクタが未知のものはルビ・テンプレート差込。本文には混ぜない。
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("本文"));
    units.extend_from_slice(&[0x001c, 0x0001, 0x0007, 0x0000, 0x0000, 0x00ff, 0x001d]);
    units.extend(text("よみがな"));
    units.push(0x001e);

    assert_eq!(paragraphs(&stream(&units)), vec!["本文"]);
}

#[test]
fn trailing_newlines_do_not_create_empty_paragraphs() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("本文"));
    units.push(0x000a);
    units.push(0x000a);

    assert_eq!(paragraphs(&stream(&units)), vec!["本文"]);
}

#[test]
fn keeps_line_breaks_inside_a_paragraph() {
    let mut units = header();
    units.extend(record(0x0010, &[0x0000]));
    units.extend(text("上"));
    units.push(0x000a);
    units.extend(text("下"));

    assert_eq!(paragraphs(&stream(&units)), vec!["上\n下"]);
}

#[test]
fn handles_raw_text_segment_layout() {
    // w[9]=0x0001 かつ TextV.01 が続く生テキスト形式は、名前 4 ワードと長さ 2 ワードを飛ばす。
    let mut units = vec![0x0000, 0x0003, 0x0000, 0x0100, 0x0000, 0x0001];
    units.extend_from_slice(super::TEXT_SEGMENT_NAME);
    units.extend_from_slice(&[0x0000, 0x0004]);
    units.extend(text("生テキスト"));

    assert_eq!(paragraphs(&stream(&units)), vec!["生テキスト"]);
}

#[test]
fn empty_and_non_jtd_input_yield_empty_documents() {
    assert!(parse_document(b"").blocks().is_empty());
    assert!(parse_document(b"not a jtd stream").blocks().is_empty());
    assert!(parse_document(b"SsmgV.01").blocks().is_empty());
}
