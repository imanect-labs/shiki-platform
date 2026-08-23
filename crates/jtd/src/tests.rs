//! `JtdFile` の単体テスト。
//!
//! 実ファイルには依存せず、合成 CFB を組んで境界条件を突く（決定的・ネットワーク不要）。
//! 実物の様式に対するゴールデン検証は後続タスクの忠実度ハーネスで行う。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Cursor, Write};

use super::{JtdError, JtdFile, JtdFormat, JtdLimits};

/// `DocumentText` の先頭 8 バイト（一太郎 8〜13 系の本文ストリーム）。
const DOCUMENT_TEXT_MAGIC: &[u8; 8] = b"SsmgV.01";
/// テキストランの開始マーカー。これ以降の UTF-16BE が本文になる。
const TEXT_RUN_MARKER: u16 = 0x001f;

/// 1 ストリームだけを持つ合成 CFB を作る。
fn cfb_with_stream(path: &str, payload: &[u8]) -> Vec<u8> {
    let mut compound = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    let mut stream = compound.create_stream(path).unwrap();
    stream.write_all(payload).unwrap();
    drop(stream);
    compound.into_inner().into_inner()
}

/// 本文 `text` を持つ `/DocumentText` ストリームのバイト列を組む。
fn document_text_stream(text: &str) -> Vec<u8> {
    let mut bytes = DOCUMENT_TEXT_MAGIC.to_vec();
    bytes.extend_from_slice(&TEXT_RUN_MARKER.to_be_bytes());
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

    assert!(file.document_text_bytes().starts_with(DOCUMENT_TEXT_MAGIC));
    assert_eq!(file.document_text_bytes(), document_text_stream("本文"));
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

    match error {
        JtdError::TooLarge {
            resource,
            limit,
            actual,
        } => {
            assert_eq!(resource, "input bytes");
            assert_eq!(limit, bytes.len() - 1);
            assert_eq!(actual, bytes.len());
        }
        other => panic!("TooLarge を期待したが {other:?} だった"),
    }
}

#[test]
fn truncated_compressed_document_is_rejected() {
    // 圧縮ヘッダだけ名乗って中身が無い入力。プロセス内パースなので、
    // ここで落ちずにエラーへ閉じ込められることが可用性の要件になる。
    let bytes = cfb_with_stream("/JSCompDocument", b"\x26\0JustCompressedDocument\0-lh5-\0");

    assert!(
        JtdFile::open(&bytes).is_err(),
        "壊れた圧縮文書はエラーになること（パニックしないこと）"
    );
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
