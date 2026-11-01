//! /data の SQL と chat の `data_query` を、素の PostgreSQL と突き合わせる生成テスト（#1004）。
//!
//! ランダムな表と単純な SQL（比較・論理・NULL・`LIMIT` と `ORDER BY`・集計）を作り、同じデータを Shiki と
//! 素の PostgreSQL に入れて同じ SQL を流し、結果を比べる。違ったら最小の再現まで縮めて保存する。
//! Shiki がわざと違えている箇所は [`allow`] に書く。発想は Jepsen の ParticleDB の検証の compare ワークロード。

#![allow(dead_code, unreachable_pub)]

pub mod allow;
pub mod exec;
pub mod gen;
pub mod model;
pub mod shrink;

use std::path::PathBuf;

use model::Case;

/// 縮めた再現を保存する（`SQL_DIFF_OUT` か `target/sql-diff`）。保存先を返す。
pub fn save(case: &Case, detail: &str) -> PathBuf {
    let dir = std::env::var_os("SQL_DIFF_OUT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/sql-diff"),
        PathBuf::from,
    );
    let _ = std::fs::create_dir_all(&dir);
    let body = serde_json::to_string_pretty(case).unwrap_or_default();
    let name = format!("{:016x}.json", fxhash(&body));
    let path = dir.join(name);
    let _ = std::fs::write(&path, &body);
    let _ = std::fs::write(
        path.with_extension("sql"),
        format!("-- {}\n{}", detail.replace('\n', "\n-- "), case.describe()),
    );
    path
}

/// 内容から決まる短い名前（同じ再現は同じファイルになる）。
fn fxhash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// 保存済みの再現（`tests/sql_diff/corpus/*.json`）。直した食い違いの回帰として毎回流す。
#[derive(serde::Deserialize)]
pub struct Saved {
    /// 期待する結果（`same` か、許容リストの拒否の id）。
    pub expect: String,
    /// どの食い違いだったか（人が読む）。
    #[serde(default)]
    pub note: String,
    pub case: Case,
}

/// 保存済みの再現を読む（読めないファイルは黙って捨てずに落とす）。
pub fn corpus() -> Vec<(String, Saved)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/sql_diff/corpus");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut out: Vec<(String, Saved)> = rd
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let body = std::fs::read_to_string(e.path())
                .unwrap_or_else(|err| panic!("{name} を読めない: {err}"));
            let saved = serde_json::from_str(&body)
                .unwrap_or_else(|err| panic!("{name} の形が今の Case と合わない: {err}"));
            (name, saved)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
