//! ランタイム DDL ゼロの機械検査（Phase 15 Task 15.3 受け入れ条件・監査 B1-2）。
//!
//! 索引の付け外しは pivot 索引テーブルの行の追記/削除で行い、DDL を打たない（data-platform §3）。
//! 較正ベンチも migration 0097 の固定表を読むだけ。DB 不要（常に走る）。
//!
//! - `grep -r "CREATE INDEX" crates/data/src` が 0 件（受け入れ条件そのもの・テストファイルも含む）
//! - 本体（`*_tests.rs` と `#[cfg(test)]` 以降を除く）に表・索引の DDL が無い

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 空白の連続を 1 つに畳み、大文字にする（`CREATE\n  INDEX` のような改行入りも拾う）。
fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

const DDL: &[&str] = &[
    "CREATE INDEX",
    "CREATE UNIQUE INDEX",
    "CREATE TABLE",
    "CREATE TEMP TABLE",
    "CREATE TEMPORARY TABLE",
    "CREATE UNLOGGED TABLE",
    "ALTER TABLE",
    "DROP INDEX",
    "DROP TABLE",
    "REINDEX",
];

fn violations(rel: &str, src: &str) -> Vec<String> {
    let mut v = Vec::new();
    // 受け入れ条件の grep と同じ: ファイル全体（テストも含む）で CREATE INDEX が 0 件。
    if normalize(src).contains("CREATE INDEX") {
        v.push(format!("{rel}: CREATE INDEX"));
    }
    if rel.ends_with("_tests.rs") {
        return v;
    }
    let body = src.find("\n#[cfg(test)]").map_or(src, |p| &src[..p]);
    let body = normalize(body);
    for k in DDL {
        if body.contains(k) {
            v.push(format!("{rel}: {k}"));
        }
    }
    v
}

#[test]
fn data_crate_issues_no_runtime_ddl() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src_dir, &mut files);
    assert!(files.len() > 10, "走査対象が少なすぎる: {files:?}");
    let mut v = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        let rel = f.strip_prefix(&src_dir).unwrap().display().to_string();
        v.extend(violations(&rel, &src));
    }
    assert!(
        v.is_empty(),
        "crates/data/src にランタイム DDL がある（15.3: 索引は行で持つ・表は migration で作る）:\n{}",
        v.join("\n")
    );
}

/// 検査器が空振りしていないこと（陰性対照）。
#[test]
fn lint_detects_ddl() {
    let bad = "fn a() { sqlx::query(\"create  temp\n table t (a int)\"); \
               sqlx::query(\"CREATE INDEX i ON t (a)\"); }";
    let v = violations("x.rs", bad);
    assert!(v.iter().any(|s| s.contains("CREATE TEMP TABLE")), "{v:?}");
    assert!(v.iter().any(|s| s.ends_with("CREATE INDEX")), "{v:?}");
    // テスト用の入力（パーサが拒否することの確認など）は本体の検査から外す。ただし CREATE INDEX は不可。
    assert!(violations("p_tests.rs", "\"DROP TABLE orders\"").is_empty());
    assert!(!violations("p_tests.rs", "\"create index x\"").is_empty());
    assert!(violations(
        "ok.rs",
        "fn b() {}\n#[cfg(test)]\nmod t { \"DROP TABLE x\"; }"
    )
    .is_empty());
}
