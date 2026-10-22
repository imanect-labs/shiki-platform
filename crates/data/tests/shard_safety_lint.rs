//! シャード安全の禁止事項の機械検査（Phase 15 Task 15.1・docs/data/shard-safety.md）。
//!
//! `crates/data/src` のソースを読み、プール分割（data-platform §12 第 2 段）を壊す書き方を CI で止める。
//! DB 不要（常に走る）。検査は字句レベルのヒューリスティックで、見逃しうる形は文書側で禁じている。
//!
//! | 規則 | 内容 |
//! |---|---|
//! | S1 | 接続プール型 `PgPool` を参照しない（`PoolRouter` 経由に統一） |
//! | S2 | シーケンス関数（`nextval` / `currval` / `lastval` / `setval`）を直接呼ばない（採番はレジストリの採番サービス経由） |
//! | S3 | `data_*` 表を読み書きする SQL は tenant 条件（`tenant_id =` / `tenant_int =`、INSERT は列）を含む |
//! | S4 | `data_*` 表どうしの JOIN を書かない |
//!
//! S3・S4 の例外（保守スイープ等）は直前 3 行以内の `shard-safety: allow(<規則>) <理由>` 注記で明示する。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

/// 対象とするテナント境界の表（`data_` で始まる構造化データの表）。
const DATA_TABLES: &[&str] = &[
    "data_table",
    "data_record",
    "data_record_revision",
    // pivot 索引テーブルと台帳（Task 15.2・tenant_int で持つ）。
    "data_index_text",
    "data_index_num",
    "data_link",
    "data_link_field",
    "data_unique",
    "data_index_search",
    "data_index_state",
    "data_index_backfill",
    // テナント設定（Task 15.3）。
    "data_tenant_settings",
];

/// 文字列リテラル（開始行・中身）。
#[derive(Debug)]
struct Lit {
    line: usize,
    text: String,
}

/// Rust ソースから文字列リテラルを取り出す（コメント・char リテラル・ライフタイムを読み飛ばす）。
fn string_literals(src: &str) -> Vec<Lit> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        // 行コメント。
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // ブロックコメント。
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        // char リテラル / ライフタイム。
        if c == '\'' {
            if chars.get(i + 1) == Some(&'\\') {
                i += 2;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if chars.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
            }
            continue;
        }
        // 生文字列 r"..." / r#"..."#（b 接頭辞も可）。
        let raw_start = if c == 'r' && !prev_is_ident(&chars, i) {
            Some(i + 1)
        } else if c == 'b' && chars.get(i + 1) == Some(&'r') && !prev_is_ident(&chars, i) {
            Some(i + 2)
        } else {
            None
        };
        if let Some(mut j) = raw_start {
            let mut hashes = 0;
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                let start_line = line;
                j += 1;
                let mut text = String::new();
                loop {
                    if j >= chars.len() {
                        break;
                    }
                    if chars[j] == '"' && (0..hashes).all(|h| chars.get(j + 1 + h) == Some(&'#')) {
                        j += 1 + hashes;
                        break;
                    }
                    if chars[j] == '\n' {
                        line += 1;
                    }
                    text.push(chars[j]);
                    j += 1;
                }
                out.push(Lit {
                    line: start_line,
                    text,
                });
                i = j;
                continue;
            }
        }
        // 通常の文字列。
        if c == '"' {
            let start_line = line;
            let mut j = i + 1;
            let mut text = String::new();
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '\\' {
                    if chars.get(j + 1) == Some(&'\n') {
                        line += 1;
                    }
                    if let Some(n) = chars.get(j + 1) {
                        text.push(*n);
                    }
                    j += 2;
                    continue;
                }
                if chars[j] == '\n' {
                    line += 1;
                }
                text.push(chars[j]);
                j += 1;
            }
            out.push(Lit {
                line: start_line,
                text,
            });
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

fn prev_is_ident(chars: &[char], i: usize) -> bool {
    i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')
}

/// `#[cfg(test)]` 以降（テストモジュール）を除いた本体部分。
fn production_part(src: &str) -> &str {
    match src.find("\n#[cfg(test)]") {
        Some(pos) => &src[..pos],
        None => src,
    }
}

fn allowed(lines: &[&str], line: usize, rule: &str) -> bool {
    let from = line.saturating_sub(4);
    let to = line.min(lines.len());
    lines[from..to]
        .iter()
        .any(|l| l.contains(&format!("shard-safety: allow({rule})")))
}

fn mentions_data_table(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    DATA_TABLES.iter().any(|t| {
        lower.match_indices(t).any(|(pos, _)| {
            let after = lower[pos + t.len()..].chars().next();
            let before = lower[..pos].chars().next_back();
            let boundary =
                |c: Option<char>| !c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            boundary(after) && boundary(before)
        })
    })
}

fn has_tenant_equality(sql: &str) -> bool {
    let compact: String = sql.split_whitespace().collect();
    compact.contains("tenant_id=") || compact.contains("tenant_int=")
}

/// 1 ファイル分の違反（`ファイル:行: 規則 内容`）。
fn check_source(path: &str, src: &str) -> Vec<String> {
    let body = production_part(src);
    let lines: Vec<&str> = body.lines().collect();
    let mut v = Vec::new();
    for (n, l) in lines.iter().enumerate() {
        if l.contains("PgPool") {
            v.push(format!("{path}:{}: S1 PgPool を直接参照している", n + 1));
        }
        for f in ["nextval(", "currval(", "lastval(", "setval("] {
            if l.contains(f) {
                v.push(format!(
                    "{path}:{}: S2 シーケンス関数 {f}) を直接呼んでいる",
                    n + 1
                ));
            }
        }
    }
    for lit in string_literals(body) {
        if !mentions_data_table(&lit.text) {
            continue;
        }
        let upper = lit.text.to_ascii_uppercase();
        let is_insert = upper.contains("INSERT INTO");
        let is_dml = is_insert
            || upper.contains("SELECT ")
            || upper.contains("UPDATE ")
            || upper.contains("DELETE ");
        if !is_dml {
            continue;
        }
        let tenant_ok = if is_insert {
            lit.text.contains("tenant_id") || lit.text.contains("tenant_int")
        } else {
            has_tenant_equality(&lit.text)
        };
        if !tenant_ok && !allowed(&lines, lit.line, "cross-tenant") {
            v.push(format!(
                "{path}:{}: S3 data_* への SQL に tenant 条件が無い: {}",
                lit.line,
                lit.text.split_whitespace().collect::<Vec<_>>().join(" ")
            ));
        }
        let joins_data = upper.match_indices("JOIN").any(|(pos, _)| {
            let rest = lit.text[pos + 4..].trim_start().to_ascii_lowercase();
            let rest = rest.strip_prefix("public.").unwrap_or(&rest);
            rest.starts_with("data_")
        });
        if joins_data && !allowed(&lines, lit.line, "join") {
            v.push(format!("{path}:{}: S4 data_* 表どうしの JOIN", lit.line));
        }
    }
    v
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs")
            && !p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("_tests.rs"))
        {
            out.push(p);
        }
    }
}

#[test]
fn data_crate_follows_shard_safety_rules() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src_dir, &mut files);
    files.sort();
    assert!(files.len() > 10, "走査対象が少なすぎる: {files:?}");
    let mut violations = Vec::new();
    let mut sql_count = 0;
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        sql_count += string_literals(production_part(&src))
            .iter()
            .filter(|l| mentions_data_table(&l.text))
            .count();
        let rel = f.strip_prefix(&src_dir).unwrap().display().to_string();
        violations.extend(check_source(&rel, &src));
    }
    assert!(
        sql_count > 20,
        "data_* の SQL を拾えていない（字句解析の退行）: {sql_count}"
    );
    assert!(
        violations.is_empty(),
        "シャード安全の禁止事項に違反（docs/data/shard-safety.md）:\n{}",
        violations.join("\n")
    );
}

/// 検査器自体が違反を検出できること（陰性対照・規則が空振りしていないか）。
#[test]
fn lint_detects_each_rule() {
    let bad = r#"
use sqlx::PgPool;
fn a() {
    let q = "SELECT data FROM data_record WHERE id = $1";
    let s = "SELECT nextval('data_table_int_seq')";
    let j = "SELECT 1 FROM data_record r JOIN data_table t ON t.id = r.table_id \
             WHERE r.tenant_id = $1";
    let i = "INSERT INTO data_record (id, data) VALUES ($1, $2)";
}
"#;
    let v = check_source("bad.rs", bad);
    let joined = v.join("\n");
    for rule in ["S1", "S2", "S3", "S4"] {
        assert!(joined.contains(rule), "{rule} を検出できない: {joined}");
    }
    assert_eq!(v.iter().filter(|x| x.contains("S3")).count(), 2, "{joined}");

    let ok = r#"
fn b() {
    let q = "SELECT data FROM data_record r WHERE r.tenant_id = $1 AND r.id = $2";
    let i = "INSERT INTO data_record (tenant_id, id) VALUES ($1, $2)";
    // shard-safety: allow(cross-tenant) 保守スイープ
    let s = "SELECT id FROM data_table WHERE key_format = 1";
    let c = '"'; let l: &'static str = "data_record は説明文";
}
#[cfg(test)]
mod tests { const X: &str = "SELECT 1 FROM data_record"; }
"#;
    assert!(
        check_source("ok.rs", ok).is_empty(),
        "{:?}",
        check_source("ok.rs", ok)
    );
}
