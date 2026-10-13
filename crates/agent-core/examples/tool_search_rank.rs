//! tool search の順位 CLI（評価基盤 `eval/tool-search/` から呼ぶ）。
//!
//! 標準入力に `{"catalog": [ToolDef...], "queries": [{"id", "text"}...], "limit": 5, "depth": 50}`
//! を受け、1 クエリ 1 行の JSON を標準出力へ書く:
//! `{"id", "search": [本番の読み込み順位], "ranked": [[name, score] 上位 depth 件]}`。
//! 最終行に `{"tool_search_description": 説明}` を書く（名前一覧の長さの観測・E2E の提示用）。

use std::io::{BufWriter, Read, Write};

use agent_core::EvalCatalog;
use llm_gateway::ToolDef;
use serde::Deserialize;

#[derive(Deserialize)]
struct Query {
    id: String,
    text: String,
}

#[derive(Deserialize)]
struct Input {
    catalog: Vec<ToolDef>,
    queries: Vec<Query>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default = "default_depth")]
    depth: usize,
}

fn default_limit() -> usize {
    5
}

fn default_depth() -> usize {
    50
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let input: Input = serde_json::from_str(&raw)?;
    let catalog = EvalCatalog::new(&input.catalog);
    let mut out = BufWriter::new(std::io::stdout().lock());
    for q in &input.queries {
        let ranked: Vec<(String, f64)> = catalog
            .ranked(&q.text)
            .into_iter()
            .take(input.depth)
            .collect();
        let line = serde_json::json!({
            "id": q.id,
            "search": catalog.search(&q.text, input.limit),
            "ranked": ranked,
        });
        writeln!(out, "{line}")?;
    }
    let def = catalog.definition();
    writeln!(
        out,
        "{}",
        serde_json::json!({
            "tool_search_description": def.description,
            "tool_search_schema": def.input_schema,
        })
    )?;
    out.flush()?;
    Ok(())
}
