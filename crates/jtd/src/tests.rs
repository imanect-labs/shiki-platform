//! `JtdFile` の単体テスト。
//!
//! 実ファイルには依存せず、合成 CFB を組んで境界条件を突く（決定的・ネットワーク不要）。
//! 実物の様式に対するゴールデン検証は後続タスクの忠実度ハーネスで行う。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Cursor, Write};

use super::{JtdError, JtdFile, JtdFormat, JtdLimitKind, JtdLimits};

/// `DocumentText` の先頭 8 バイト（一太郎 8〜13 系の本文ストリーム）。
const DOCUMENT_TEXT_MAGIC: &[u8; 8] = b"SsmgV.01";
/// マジックの後に続くヘッダ（6 ワード）。実物と同じ長さにしないと本文の開始位置がずれる。
const HEADER_UNITS: [u16; 6] = [0x0000, 0x0003, 0x0000, 0x0100, 0x0000, 0x011c];

/// 1 ストリームだけを持つ合成 CFB を作る。
fn cfb_with_stream(path: &str, payload: &[u8]) -> Vec<u8> {
    cfb_with_streams(&[(path, payload)])
}

/// 複数ストリームを持つ合成 CFB を作る。
fn cfb_with_streams(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut compound = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    for (path, payload) in entries {
        let mut stream = compound.create_stream(path).unwrap();
        stream.write_all(payload).unwrap();
    }
    compound.into_inner().into_inner()
}

/// 段落レコード（`class=0x0010`・最小長）。実物と同じ自己記述構造を持たせる。
const PARAGRAPH_RECORD: [u16; 8] = [
    0x001c, 0x0010, 0x0008, 0x0000, 0x0008, 0x0000, 0x0010, 0x001f,
];

/// 本文 `text` を持つ `/DocumentText` ストリームのバイト列を組む。
fn document_text_stream(text: &str) -> Vec<u8> {
    let mut bytes = DOCUMENT_TEXT_MAGIC.to_vec();
    for unit in HEADER_UNITS.iter().chain(PARAGRAPH_RECORD.iter()) {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

/// 本文 `text` を持つ最小の合成 JTD。
fn synthetic_jtd(text: &str) -> Vec<u8> {
    cfb_with_stream("/DocumentText", &document_text_stream(text))
}

#[test]
fn reads_body_text_from_document_text_stream() {
    let bytes = synthetic_jtd("様式第１（第７条関係）");

    let file = JtdFile::open(&bytes).expect("合成 JTD は読めること");

    assert_eq!(file.format(), JtdFormat::DocumentText);
    assert_eq!(file.plain_text(), "様式第１（第７条関係）");
}

#[test]
fn exposes_raw_document_text_for_layout_decoding() {
    // 後続タスクのレイアウト解読はこの生バイト列を入力にするため、加工せずに出すこと。
    let bytes = synthetic_jtd("本文");

    let file = JtdFile::open(&bytes).expect("合成 JTD は読めること");

    let raw = file
        .document_text_bytes()
        .expect("単一ストリームの変種では生バイト列が取れること");
    assert!(raw.starts_with(DOCUMENT_TEXT_MAGIC));
    assert_eq!(raw, document_text_stream("本文"));
    assert!(
        file.embedded_fragments().is_none(),
        "断片版のアクセサは None であること"
    );
}

#[test]
fn lists_container_streams() {
    let bytes = synthetic_jtd("本文");

    let file = JtdFile::open(&bytes).expect("合成 JTD は読めること");

    let document_text = file
        .streams()
        .iter()
        .find(|stream| stream.path() == "/DocumentText")
        .expect("/DocumentText が一覧に出ること");
    assert_eq!(
        document_text.size(),
        document_text_stream("本文").len() as u64
    );
}

#[test]
fn rejects_non_cfb_input() {
    assert!(matches!(
        JtdFile::open(b"this is not a compound document"),
        Err(JtdError::NotJtd)
    ));
    assert!(matches!(JtdFile::open(&[]), Err(JtdError::NotJtd)));
}

#[test]
fn rejects_compound_document_that_is_not_ichitaro() {
    // doc/xls など別アプリの複合文書は CFB ではあるが一太郎ではない。
    let bytes = cfb_with_stream("/WordDocument", b"not ichitaro");

    assert!(matches!(JtdFile::open(&bytes), Err(JtdError::Unsupported)));
}

#[test]
fn rejects_input_larger_than_the_limit() {
    let bytes = synthetic_jtd("本文");
    let limits = JtdLimits::DEFAULT.with_max_input_bytes(bytes.len() - 1);

    let error = JtdFile::open_with_limits(&bytes, limits).expect_err("上限を超えたら失敗すること");

    assert!(
        matches!(error, JtdError::TooLarge(JtdLimitKind::Input)),
        "入力上限として分類されること（実際は {error:?}）"
    );
    assert!(
        !error.to_string().contains(&bytes.len().to_string()),
        "実測バイト数が公開メッセージに漏れていないこと"
    );
}

#[test]
fn truncated_compressed_document_is_rejected() {
    // 圧縮ヘッダだけ名乗って中身が無い入力。
    //
    // 注意: `guard()` がパニックを `Malformed` に変換するため、**このテストは
    // 「パニックしないこと」を検証できない**（パニックしても Err で通る）。
    // パニック・abort・ハングの検出は `tests/adversarial_it.rs` の担当。
    // ここが固定するのは「壊れた圧縮文書が Ok にならない」ことだけ。
    let bytes = cfb_with_stream("/JSCompDocument", b"\x26\0JustCompressedDocument\0-lh5-\0");

    assert!(
        JtdFile::open(&bytes).is_err(),
        "壊れた圧縮文書は Ok にならないこと"
    );
}

#[test]
fn recognises_embedded_document_text_variant() {
    // `/DocumentText` も `/JSCompDocument` も持たないが、別ストリームに `SsmgV.01` の
    // 断片が埋まっている変種。本文はそこから拾える。
    let bytes = cfb_with_stream("/JSSlipObject1", &document_text_stream("埋め込み本文"));

    let file = JtdFile::open(&bytes).expect("埋め込み断片から本文が拾えること");

    assert_eq!(file.format(), JtdFormat::EmbeddedDocumentText);
    assert!(file.plain_text().contains("埋め込み本文"));
}

#[test]
fn document_text_without_text_run_yields_empty_body() {
    // マジックだけ正しく、以降がテキストランを含まないストリーム。
    // 「読めたが本文が空」に落ち、パニックにも解析エラーにもならないことを固定する。
    let mut payload = DOCUMENT_TEXT_MAGIC.to_vec();
    payload.extend_from_slice(&[0xff; 64]);
    let bytes = cfb_with_stream("/DocumentText", &payload);

    let file = JtdFile::open(&bytes).expect("マジックが正しければ読めること");

    assert_eq!(file.format(), JtdFormat::DocumentText);
    assert!(file.plain_text().is_empty(), "本文は空になること");
}

#[test]
fn format_labels_are_stable() {
    // 監査ログ・メトリクスのラベルになるため、値を勝手に変えない。
    assert_eq!(JtdFormat::DocumentText.as_str(), "document-text");
    assert_eq!(
        JtdFormat::CompressedDocument.as_str(),
        "compressed-document"
    );
    assert_eq!(
        JtdFormat::EmbeddedDocumentText.as_str(),
        "embedded-document-text"
    );
}

#[test]
fn keeps_characters_above_the_basic_plane() {
    // 日本人の氏名は CJK 拡張 B（𠮷・𩸽）を普通に使う。UTF-16 のサロゲート対を
    // code unit 単位で捨てると「𠮷田」が「田」になる。
    let bytes = synthetic_jtd("𠮷田さんと𩸽");

    let file = JtdFile::open(&bytes).expect("合成 JTD は読めること");

    assert_eq!(file.plain_text(), "𠮷田さんと𩸽");
}

#[test]
fn classifies_by_the_body_actually_read_not_by_stream_presence() {
    // 壊れた `/JSCompDocument` と有効な埋め込み断片が同居する CFB。上流は展開に失敗して
    // 埋め込み断片へフォールバックするので、系統も埋め込みでなければ監査ラベルが嘘になる。
    let bytes = cfb_with_streams(&[
        ("/JSCompDocument", b"not a JustCompressedDocument at all"),
        ("/JSSlipObject1", &document_text_stream("埋め込み本文")),
    ]);

    let file = JtdFile::open(&bytes).expect("埋め込み断片から本文が拾えること");

    assert_eq!(
        file.format(),
        JtdFormat::EmbeddedDocumentText,
        "ストリームの存在ではなく、実際に読んだ本文の出所で分類すること"
    );
    assert!(file.plain_text().contains("埋め込み本文"));
}

#[test]
fn embedded_fragments_are_not_exposed_as_a_single_stream() {
    // 埋め込み変種の本文は複数断片を 0x0000 でつないだもので、`/DocumentText` ストリーム
    // ではない。単一ストリームとして読むと断片ヘッダと境界を誤読するので、
    // document_text_bytes() では出さない。
    let mut payload = document_text_stream("いち");
    payload.extend_from_slice(&[0x00, 0x00]);
    payload.extend_from_slice(&document_text_stream("に"));
    let bytes = cfb_with_stream("/JSSlipObject1", &payload);

    let file = JtdFile::open(&bytes).expect("埋め込み断片から本文が拾えること");

    assert_eq!(file.format(), JtdFormat::EmbeddedDocumentText);
    assert!(
        file.document_text_bytes().is_none(),
        "単一ストリームとしては公開しないこと"
    );
    let fragments = file
        .embedded_fragments()
        .expect("断片版のアクセサからは取れること");
    assert!(
        fragments
            .windows(DOCUMENT_TEXT_MAGIC.len())
            .filter(|window| *window == DOCUMENT_TEXT_MAGIC)
            .count()
            >= 2,
        "断片が複数つながっていること"
    );
}
