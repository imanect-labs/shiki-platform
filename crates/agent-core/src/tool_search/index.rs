//! 遅延ツールの検索索引（in-memory BM25F）。
//!
//! 文書はツール 1 件。フィールドは名前・検索語・説明・引数（名前＋説明）で、名前ほど重い。
//! 数十〜数百件の規模なので転置索引も外部エンジンも要らない（run 開始時に組んで捨てる）。
//!
//! トークナイズは言語非依存の近似: ASCII は英数字の連なりを語にし（`csv.query` →
//! `csv`/`query`、末尾の複数形 `s` を落とす）、日本語（かな・漢字）は**文字 bigram** にする。
//! 形態素解析器を持ち込まずに「表計算」と「表計算ソフト」を部分一致させるため。

use std::collections::{HashMap, HashSet};

use llm_gateway::ToolDef;

/// BM25 の飽和パラメータ。
const K1: f64 = 1.2;
/// BM25 の長さ正規化の強さ。
const B: f64 = 0.75;

/// フィールドとその重み（名前 ＞ 検索語 ＞ 説明 ＞ 引数）。
#[derive(Clone, Copy)]
enum Field {
    Name,
    Keywords,
    Description,
    Params,
}

impl Field {
    const ALL: [Field; 4] = [
        Field::Name,
        Field::Keywords,
        Field::Description,
        Field::Params,
    ];

    fn weight(self) -> f64 {
        match self {
            Field::Name => 3.0,
            Field::Keywords => 2.0,
            Field::Description => 1.0,
            Field::Params => 0.5,
        }
    }

    fn at(self) -> usize {
        self as usize
    }
}

/// 1 ツール分の索引（フィールドごとの語頻度と長さ）。
struct Doc {
    tf: [HashMap<String, f64>; 4],
    len: [f64; 4],
}

/// ツール検索の索引。
pub(crate) struct ToolIndex {
    docs: Vec<Doc>,
    /// 語 → それを含む文書数。
    df: HashMap<String, usize>,
    /// フィールドごとの平均長。
    avg_len: [f64; 4],
}

impl ToolIndex {
    /// ツール定義と検索語（`defs` と同じ並び）から索引を組む。
    pub(crate) fn build(defs: &[ToolDef], keywords: &[&str]) -> Self {
        let docs: Vec<Doc> = defs
            .iter()
            .zip(keywords.iter().copied().chain(std::iter::repeat("")))
            .map(|(def, kw)| {
                let mut params = String::new();
                collect_params(&def.input_schema, &mut params, 0);
                let texts = [def.name.as_str(), kw, def.description.as_str(), &params];
                let mut tf: [HashMap<String, f64>; 4] = Default::default();
                let mut len = [0.0; 4];
                for f in Field::ALL {
                    for t in tokenize(texts[f.at()]) {
                        *tf[f.at()].entry(t).or_insert(0.0) += 1.0;
                        len[f.at()] += 1.0;
                    }
                }
                Doc { tf, len }
            })
            .collect();
        let mut df: HashMap<String, usize> = HashMap::new();
        for d in &docs {
            let terms: HashSet<&String> = d.tf.iter().flat_map(HashMap::keys).collect();
            for t in terms {
                *df.entry(t.clone()).or_insert(0) += 1;
            }
        }
        let mut avg_len = [0.0; 4];
        if !docs.is_empty() {
            for f in Field::ALL {
                #[allow(clippy::cast_precision_loss)] // 文書数は高々数百。
                let n = docs.len() as f64;
                avg_len[f.at()] = docs.iter().map(|d| d.len[f.at()]).sum::<f64>() / n;
            }
        }
        ToolIndex { docs, df, avg_len }
    }

    /// クエリに対する各文書のスコア（`build` に渡した並び・0 は無関係）。
    pub(crate) fn scores(&self, query: &str) -> Vec<f64> {
        let terms: HashSet<String> = tokenize(query).into_iter().collect();
        #[allow(clippy::cast_precision_loss)]
        let n = self.docs.len() as f64;
        self.docs
            .iter()
            .map(|doc| {
                terms
                    .iter()
                    .map(|t| {
                        let tf: f64 = Field::ALL
                            .iter()
                            .map(|f| {
                                let raw = doc.tf[f.at()].get(t).copied().unwrap_or(0.0);
                                if raw == 0.0 {
                                    return 0.0;
                                }
                                let avg = self.avg_len[f.at()].max(1.0);
                                let norm = 1.0 - B + B * doc.len[f.at()] / avg;
                                f.weight() * raw / norm
                            })
                            .sum();
                        if tf == 0.0 {
                            return 0.0;
                        }
                        #[allow(clippy::cast_precision_loss)]
                        let df = self.df.get(t).copied().unwrap_or(0) as f64;
                        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                        idf * tf / (K1 + tf)
                    })
                    .sum()
            })
            .collect()
    }
}

/// JSON Schema の引数名と説明を集める（入れ子は浅く辿る・巨大スキーマで暴れない）。
fn collect_params(schema: &serde_json::Value, out: &mut String, depth: usize) {
    const MAX_DEPTH: usize = 3;
    if depth > MAX_DEPTH {
        return;
    }
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        for (name, prop) in props {
            out.push_str(name);
            out.push(' ');
            if let Some(d) = prop.get("description").and_then(|d| d.as_str()) {
                out.push_str(d);
                out.push(' ');
            }
            collect_params(prop, out, depth + 1);
        }
    }
    if let Some(items) = schema.get("items") {
        collect_params(items, out, depth + 1);
    }
}

/// 日本語（かな・漢字・長音）か。
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30FF}' // ひらがな・カタカナ（長音 `ー` を含む）
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FF66}'..='\u{FF9F}') // 半角カナ
}

/// 全角英数を半角へ寄せて小文字化する。
fn normalize(c: char) -> char {
    let c = match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    };
    c.to_ascii_lowercase()
}

/// 検索用の語へ分割する（ASCII は語単位、日本語は文字 bigram）。
pub(crate) fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut cjk: Vec<char> = Vec::new();
    for c in text.chars().map(normalize) {
        if c.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            word.push(c);
        } else if is_cjk(c) {
            flush_word(&mut word, &mut out);
            cjk.push(c);
        } else {
            flush_word(&mut word, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_word(&mut word, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out
}

fn flush_word(word: &mut String, out: &mut Vec<String>) {
    if word.is_empty() {
        return;
    }
    // 素朴な単複の同一視（`sheets` → `sheet`）。`ss` で終わる語（`class`）は触らない。
    if word.len() > 3 && word.ends_with('s') && !word.ends_with("ss") {
        word.pop();
    }
    out.push(std::mem::take(word));
}

fn flush_cjk(run: &mut Vec<char>, out: &mut Vec<String>) {
    match run.len() {
        0 => {}
        1 => out.push(run[0].to_string()),
        _ => out.extend(run.windows(2).map(|w| w.iter().collect::<String>())),
    }
    run.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tokenizes_ascii_words_and_japanese_bigrams() {
        assert_eq!(tokenize("csv.query"), ["csv", "query"]);
        assert_eq!(tokenize("Spreadsheets"), ["spreadsheet"]);
        assert_eq!(tokenize("ＣＳＶ"), ["csv"]);
        assert_eq!(tokenize("表計算"), ["表計", "計算"]);
        assert_eq!(tokenize("表 を"), ["表", "を"]);
        assert_eq!(tokenize("class"), ["class"]);
    }

    #[test]
    fn name_and_param_fields_contribute_and_name_outweighs_description() {
        let defs = vec![
            ToolDef::new("csv.query", "表に問い合わせる", json!({})),
            ToolDef::new(
                "note_read",
                "csv を読む",
                json!({"properties": {"path": {"description": "対象"}}}),
            ),
        ];
        let idx = ToolIndex::build(&defs, &["", ""]);
        let s = idx.scores("csv");
        assert!(s[0] > s[1], "{s:?}");
        let p = idx.scores("path");
        assert!(p[1] > 0.0 && p[0] == 0.0, "{p:?}");
    }

    #[test]
    fn keywords_bridge_english_queries_to_japanese_descriptions() {
        let defs = vec![
            ToolDef::new("save_sheet", "新規ブックを作成する", json!({})),
            ToolDef::new("save_note", "新規ノートを作成する", json!({})),
        ];
        let idx = ToolIndex::build(&defs, &["excel spreadsheet", "note markdown"]);
        let s = idx.scores("create an Excel spreadsheet");
        assert!(s[0] > s[1], "{s:?}");
    }
}
