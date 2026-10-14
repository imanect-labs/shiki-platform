//! tool search の順位 CLI（評価基盤 `eval/tool-search/` から呼ぶ）。
//!
//! 標準入力に `{"catalog": [ToolDef...], "queries": [{"id", "text"}...], "limit": 5, "depth": 50}`
//! を受け、1 クエリ 1 行の JSON を標準出力へ書く:
//! `{"id", "search": [BM25 のみの経路で製品が読み込む順位（埋め込み未配線時）], "ranked": [[name, score] 上位 depth 件]}`。
//! `"loads": [{"id", "query", "limit"?, "names"}]` があれば、1 件 1 行で
//! `{"load_id", "content": 製品の結果本文, "names": 実際に読み込む名前}` も書く（E2E が製品の文面で
//! 会話させる・#522）。`"companions": true` なら、読み込むツールの ID 引数を調べる下調べ用ツールも
//! 一緒に読み込む（#565）。
//! `"fuses": [{"id", "query", "embedding", "limit"}]` があれば、1 件 1 行で
//! `{"fuse_id", "names": 製品の融合順位（埋め込みの順位は評価側が渡す）}` も書く。
//! 最終行に `{"tool_search_description": 説明}` を書く（名前一覧の長さの観測・E2E の提示用）。

use std::io::{BufWriter, Read, Write};

use agent_core::CatalogSearch;
use llm_gateway::ToolDef;
use serde::Deserialize;

#[derive(Deserialize)]
struct Query {
    id: String,
    text: String,
}

#[derive(Deserialize)]
struct Load {
    id: String,
    query: String,
    #[serde(default)]
    limit: Option<u64>,
    names: Vec<String>,
}

#[derive(Deserialize)]
struct Fuse {
    id: String,
    query: String,
    embedding: Vec<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Deserialize)]
struct Input {
    catalog: Vec<ToolDef>,
    queries: Vec<Query>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default = "default_depth")]
    depth: usize,
    #[serde(default)]
    loads: Vec<Load>,
    #[serde(default)]
    fuses: Vec<Fuse>,
    #[serde(default)]
    companions: bool,
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
    let mut catalog = CatalogSearch::new(&input.catalog, None);
    if input.companions {
        catalog = catalog.with_companions(&input.catalog);
    }
    let mut out = BufWriter::new(std::io::stdout().lock());
    for q in &input.queries {
        let ranked: Vec<(String, f64)> = catalog
            .ranked(&q.text)
            .into_iter()
            .take(input.depth)
            .collect();
        let line = serde_json::json!({
            "id": q.id,
            "search": catalog.search_lexical(&q.text, input.limit),
            "ranked": ranked,
        });
        writeln!(out, "{line}")?;
    }
    for l in &input.loads {
        let (content, names) = catalog.loaded(&l.query, l.limit, &l.names);
        let line = serde_json::json!({ "load_id": l.id, "content": content, "names": names });
        writeln!(out, "{line}")?;
    }
    for f in &input.fuses {
        let names = catalog.fused(&f.query, &f.embedding, f.limit);
        writeln!(
            out,
            "{}",
            serde_json::json!({ "fuse_id": f.id, "names": names })
        )?;
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
