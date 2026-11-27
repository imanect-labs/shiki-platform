//! `send_traced`（traceparent を付ける送信）を使ってよいファイルを、内部サービス向けに限る。
//!
//! 外部の LLM プロバイダ・公開 Web の取得（web-access）・外部 OIDC/JWKS へトレース ID を出さない
//! ための境界。送り先の方針が変わったら、このリストを更新する（呼び出し点を足すだけでは通らない）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// 内部サービス（OpenFGA・Qdrant・ingestion-worker・Langfuse）だけを呼ぶファイル。
const ALLOWED: &[&str] = &[
    "crates/authz/src/fga_http.rs",       // OpenFGA
    "crates/rag/src/vector_qdrant.rs",    // Qdrant
    "crates/rag/src/embedding.rs",        // ingestion-worker /embed
    "crates/rag/src/parser_http.rs",      // ingestion-worker /parse
    "crates/rag/src/rerank.rs",           // ingestion-worker /rerank
    "crates/office/src/edit.rs",          // ingestion-worker /edit
    "crates/office/src/compose.rs",       // ingestion-worker /edit（docx 合成）
    "crates/llm-gateway/src/langfuse.rs", // Langfuse
];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn send_traced_is_used_only_for_internal_services() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let crates = root.join("crates");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).unwrap().flatten() {
        let src = entry.path().join("src");
        // このクレート自身（定義側）は対象外。
        if src.is_dir() && entry.file_name() != "http-trace" {
            rs_files(&src, &mut files);
        }
    }
    assert!(!files.is_empty(), "crates/*/src が見つからない");

    let mut used = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        if text.contains("send_traced") || text.contains("TracedSend") {
            let rel = f
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            used.push(rel);
        }
    }
    let unexpected: Vec<_> = used
        .iter()
        .filter(|p| !ALLOWED.contains(&p.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "内部サービス以外で traceparent を付けている: {unexpected:?}"
    );
    // リストが古くならないよう、許可したファイルは実際に使っていること。
    let missing: Vec<_> = ALLOWED
        .iter()
        .filter(|p| !used.iter().any(|u| u == *p))
        .collect();
    assert!(
        missing.is_empty(),
        "許可リストにあるが使っていない: {missing:?}"
    );
}
