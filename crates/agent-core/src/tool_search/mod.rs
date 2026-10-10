//! tool search（遅延ツールを検索して読み込む・`tool_search` メタツール）。
//!
//! 提示ツールが増えると、定義が文脈を食い、モデルのツール選択精度も落ちる。出番の限られる
//! ツール（[`ToolName::loading`] が `Deferred`）は定義を最初は載せず、モデルが `tool_search`
//! で見つけたものだけを読み込む。
//!
//! - **検索は認可を広げない。** 候補はこの run に**提示済みの**ツール（配線・プロファイル・
//!   実行前フェーズの門・ミニアプリの宣言を通った集合）だけ。読み込みは文脈の
//!   最適化であり、実行可否は従来どおり `Tool::call`（発話者の `AuthContext`）と承認ゲートが
//!   決める。
//! - **読み込み状態は履歴が正**（[`llm_gateway::tool_loading`]）。結果ブロックの
//!   `tool_references` に名前を残すだけで、別の状態を持たない（再開・剪定に耐える）。
//! - **プロバイダ差はアダプタが吸収する。** Anthropic は `defer_loading` ＋ `tool_reference`
//!   （API が展開・prefix cache を壊さない）、OpenAI 互換は読み込んだ定義を `tools` 末尾へ足す。

mod index;

use llm_gateway::ToolDef;
use serde_json::{json, Value};

use crate::profile::ToolSearchOptions;
use crate::vocab::{ToolLoading, ToolName};
use index::ToolIndex;

/// tool search メタツールの名前（ループが横取りして処理する・`plan` と同じ扱い）。
pub(crate) const TOOL_SEARCH_TOOL: &str = crate::vocab::MetaToolName::ToolSearch.as_str();

/// 1 回の検索で返す既定件数と上限。
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 10;
/// クエリ長の上限（文字数）。長文の貼り付けで索引全体を舐めさせない。
const MAX_QUERY_CHARS: usize = 500;
/// 最上位スコアに対してこの比率未満のヒットは返さない（弱い一致で枠を埋めない）。
const RELATIVE_CUTOFF: f64 = 0.25;
/// 遅延にできるツールがこれ未満なら tool search を使わない（検索の 1 手に見合わない）。
const MIN_DEFERRED_TOOLS: usize = 4;
/// description に載せる 1 ツールの説明の上限（文字数・結果テキスト用）。
const SUMMARY_CHARS: usize = 80;

/// この run の遅延ツール索引。
pub(crate) struct ToolSearch {
    /// 遅延ツールの名前と要約（索引と同じ並び）。
    tools: Vec<(String, String)>,
    index: ToolIndex,
}

/// 検索 1 回の結果（観測テキスト＋読み込むツール名）。
pub(crate) struct SearchResult {
    pub(crate) content: String,
    pub(crate) references: Vec<String>,
    pub(crate) is_error: bool,
}

/// 提示ツールに遅延区分を適用し、有効なら `tool_search` を末尾に足す。
///
/// 戻り値の `Option` は有効時の索引（`None` なら定義は無変更）。`tool_search` を**末尾**に
/// 置くのは、既存の並び（＝prefix cache と、先頭ツールを呼ぶ決定的 stub）を変えないため。
pub(crate) fn prepare(
    mut defs: Vec<ToolDef>,
    opts: &ToolSearchOptions,
) -> (Vec<ToolDef>, Option<ToolSearch>) {
    if !opts.enabled || defs.iter().any(|d| d.name == TOOL_SEARCH_TOOL) {
        return (defs, None);
    }
    let deferrable: Vec<usize> = defs
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            ToolName::parse(&d.name).is_some_and(|n| n.loading() == ToolLoading::Deferred)
                && !opts.always_load.iter().any(|a| a == &d.name)
        })
        .map(|(i, _)| i)
        .collect();
    let tokens: usize = deferrable.iter().map(|&i| def_tokens(&defs[i])).sum();
    // 常時ロードが 1 つも残らない構成では遅延にしない（Anthropic は全遅延を 400 で拒む）。
    // `tool_search` 自身は常時ロードだが、それだけの文脈では会話の基本動作も失う。
    if deferrable.len() < MIN_DEFERRED_TOOLS
        || tokens < opts.min_deferred_tokens
        || deferrable.len() == defs.len()
    {
        return (defs, None);
    }
    for &i in &deferrable {
        defs[i].defer_loading = true;
    }
    let deferred: Vec<ToolDef> = deferrable.iter().map(|&i| defs[i].clone()).collect();
    let search = ToolSearch::build(&deferred);
    tracing::info!(
        deferred = deferrable.len(),
        deferred_tokens = tokens,
        loaded = defs.len() - deferrable.len(),
        "tool search: 遅延ツールを検索経由にする"
    );
    defs.push(search.definition());
    (defs, Some(search))
}

/// 1 定義の推定トークン（剪定と同じ 4 バイト ≒ 1 トークンの粗い近似）。
fn def_tokens(d: &ToolDef) -> usize {
    (d.name.len() + d.description.len() + d.input_schema.to_string().len()) / 4
}

/// 説明の先頭（最初の文・`SUMMARY_CHARS` 文字まで）。
fn summary(description: &str) -> String {
    let first = description
        .split_inclusive(['。', '\n'])
        .next()
        .unwrap_or(description)
        .trim();
    if first.chars().count() <= SUMMARY_CHARS {
        return first.to_string();
    }
    let cut: String = first.chars().take(SUMMARY_CHARS).collect();
    format!("{cut}…")
}

/// 名前の比較キー（OpenAI 互換の wire 名 `csv_query` でも `csv.query` に当たるように）。
fn name_key(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace(['.', '-'], "_")
}

impl ToolSearch {
    /// 遅延ツールの定義から索引を組む（検索語は語彙の単一定義から引く・語彙外は無し）。
    fn build(deferred: &[ToolDef]) -> Self {
        let keywords: Vec<&str> = deferred
            .iter()
            .map(|d| ToolName::parse(&d.name).map_or("", ToolName::search_keywords))
            .collect();
        ToolSearch {
            index: ToolIndex::build(deferred, &keywords),
            tools: deferred
                .iter()
                .map(|d| (d.name.clone(), summary(&d.description)))
                .collect(),
        }
    }

    /// `tool_search` のツール定義。検索できるツールの**名前の一覧**を説明に載せる
    /// （何が在るかを知らないと探しようがない・run 内で不変＝prefix cache を壊さない）。
    fn definition(&self) -> ToolDef {
        let names: Vec<&str> = self.tools.iter().map(|(n, _)| n.as_str()).collect();
        ToolDef::new(
            TOOL_SEARCH_TOOL,
            format!(
                "まだ読み込まれていないツールを検索して読み込む。次のツールは定義が読み込まれて\
                 いないため、このままでは呼び出せない: {}。\
                 必要になったら、やりたいこと（日本語/英語の自然文）かツール名の一部で検索する。\
                 見つかったツールは読み込まれ、以降は通常どおり呼び出せる。\
                 名前が分かっていれば `select:csv.query,csv.patch` の形で直接読み込める。",
                names.join(", ")
            ),
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "探したい操作（例: 「CSV を集計する」「edit a slide」）、\
                            ツール名の一部、または `select:<名前>,<名前>`。`+csv 集計` のように \
                            `+語` を付けると名前にその語を含むものに絞る。",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_LIMIT,
                        "description": format!("読み込む最大件数（既定 {DEFAULT_LIMIT}）"),
                    },
                },
                "required": ["query"],
            }),
        )
    }

    /// `tool_search` の呼び出しを処理する。
    pub(crate) fn handle(&self, input: &Value) -> SearchResult {
        let Some(query) = input.get("query").and_then(Value::as_str).map(str::trim) else {
            return SearchResult::error("query（文字列）が必要です。");
        };
        if query.is_empty() {
            return SearchResult::error(
                "query が空です。探したい操作かツール名を指定してください。",
            );
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return SearchResult::error(format!(
                "query が長すぎます（{MAX_QUERY_CHARS} 文字以内）。"
            ));
        }
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .map_or(DEFAULT_LIMIT, |n| n.clamp(1, MAX_LIMIT));

        let (hits, unknown) = match query.strip_prefix("select:") {
            Some(list) => self.select(list, limit),
            None => (self.rank(query, limit), Vec::new()),
        };
        self.render(&hits, &unknown)
    }

    /// `select:a,b` — 名前の完全一致（区切り違いは同一視）。
    fn select(&self, list: &str, limit: usize) -> (Vec<usize>, Vec<String>) {
        let mut hits = Vec::new();
        let mut unknown = Vec::new();
        for want in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let key = name_key(want);
            match self.tools.iter().position(|(n, _)| name_key(n) == key) {
                Some(i) if !hits.contains(&i) => hits.push(i),
                Some(_) => {}
                None => unknown.push(want.to_string()),
            }
        }
        hits.truncate(limit);
        (hits, unknown)
    }

    /// 自然文/キーワード検索（`+語` は名前に含むものへの絞り込み）。
    fn rank(&self, query: &str, limit: usize) -> Vec<usize> {
        let scored = self.scored(query);
        let top = scored.first().map_or(0.0, |(_, s)| *s);
        scored
            .into_iter()
            .filter(|(_, s)| top <= 0.0 || *s >= top * RELATIVE_CUTOFF)
            .take(limit)
            .map(|(i, _)| i)
            .collect()
    }

    /// 打ち切り前の全順位（スコア降順・同点は定義順）。
    fn scored(&self, query: &str) -> Vec<(usize, f64)> {
        let mut required: Vec<String> = Vec::new();
        let mut rest: Vec<&str> = Vec::new();
        for word in query.split_whitespace() {
            match word.strip_prefix('+') {
                Some(r) if !r.is_empty() => required.push(name_key(r)),
                _ => rest.push(word),
            }
        }
        let text = if rest.is_empty() {
            required.join(" ")
        } else {
            rest.join(" ")
        };
        let whole = name_key(query);
        let mut scored: Vec<(usize, f64)> = self
            .index
            .scores(&text)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| {
                let key = name_key(&self.tools[*i].0);
                required.iter().all(|r| key.contains(r.as_str()))
            })
            .map(|(i, s)| {
                // 名前そのものを打ったなら必ず先頭に出す。
                let exact = if name_key(&self.tools[i].0) == whole {
                    1_000.0
                } else {
                    0.0
                };
                (i, s + exact)
            })
            .filter(|(_, s)| *s > 0.0 || !required.is_empty())
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored
    }

    fn render(&self, hits: &[usize], unknown: &[String]) -> SearchResult {
        let mut content = if hits.is_empty() {
            let names: Vec<&str> = self.tools.iter().map(|(n, _)| n.as_str()).collect();
            format!(
                "該当するツールは見つかりませんでした。検索できるツール: {}。\
                 別の言い方（日本語/英語・ツール名の一部）で検索するか、\
                 `select:<名前>` で直接読み込んでください。",
                names.join(", ")
            )
        } else {
            let lines: Vec<String> = hits
                .iter()
                .map(|&i| format!("\n- {}: {}", self.tools[i].0, self.tools[i].1))
                .collect();
            format!(
                "{} 件のツールを読み込みました。以降は通常どおり呼び出せます。{}",
                hits.len(),
                lines.concat()
            )
        };
        if !unknown.is_empty() {
            content = format!("{content}\n見つからなかった名前: {}", unknown.join(", "));
        }
        SearchResult {
            content,
            references: hits.iter().map(|&i| self.tools[i].0.clone()).collect(),
            is_error: false,
        }
    }
}

impl SearchResult {
    fn error(msg: impl Into<String>) -> Self {
        SearchResult {
            content: msg.into(),
            references: Vec::new(),
            is_error: true,
        }
    }
}

/// 評価用の窓口（`eval/tool-search/` の順位 CLI が使う・製品の経路は [`prepare`]）。
///
/// 製品と**同じ索引・同じ順位付け**（名前一致ブースト・相対カットオフ）を外へ出すだけで、
/// 別実装を持たない。評価が測るのは本番のコードそのもの。
#[doc(hidden)]
pub struct EvalCatalog(ToolSearch);

impl EvalCatalog {
    /// 検索対象のツール定義から索引を組む（語彙のツールは検索語も入る）。
    #[must_use]
    pub fn new(defs: &[ToolDef]) -> Self {
        EvalCatalog(ToolSearch::build(defs))
    }

    /// 本番の `tool_search` が読み込む順位（`limit` 件・相対カットオフ込み）。
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<String> {
        self.0
            .rank(query, limit)
            .into_iter()
            .map(|i| self.0.tools[i].0.clone())
            .collect()
    }

    /// 打ち切り前の全順位とスコア（他の検索との融合の入力）。
    #[must_use]
    pub fn ranked(&self, query: &str) -> Vec<(String, f64)> {
        self.0
            .scored(query)
            .into_iter()
            .map(|(i, s)| (self.0.tools[i].0.clone(), s))
            .collect()
    }

    /// `tool_search` の定義（説明に載る名前一覧の長さを測る）。
    #[must_use]
    pub fn definition(&self) -> ToolDef {
        self.0.definition()
    }
}

#[cfg(test)]
mod tests;
