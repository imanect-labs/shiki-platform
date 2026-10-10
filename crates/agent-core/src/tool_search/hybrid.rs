//! 埋め込みとの融合（BM25F と Ruri v3 埋め込みの RRF・#517）。
//!
//! BM25F は語の一致しか見ないため、英語の説明に日本語で頼む・言い換えるといった語彙のずれに
//! 弱い（評価 #516: 英語の説明 × 日本語の依頼で R@5 16%）。埋め込みの順位と RRF で混ぜると、
//! 1,003 ツールで日本語の説明 65.4% → 73.8%、英語の説明 50.7% → 67.1% に上がる。
//!
//! - **k=10**（`crates/rag` の文書検索の既定 60 ではない）。候補の少ないツール検索では、
//!   k が大きいと各方式の上位の一致が薄まり、英語の説明では融合が埋め込み単体を下回った。
//! - 文書は「名前＋説明＋引数」。名前を抜くと埋め込み単体の R@5 は 66% → 54% に落ちる。
//! - 文書の埋め込みは**プロセス内で使い回す**（ツール定義は配備中ほぼ不変・run ごとに
//!   埋め込み直さない）。クエリの埋め込みは 1 回 40ms 程度。
//! - 埋め込みが失敗したら呼び出し側が BM25 だけで返す（検索そのものは止めない）。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use authz::AuthContext;
use llm_gateway::ToolDef;
use rag::{EmbedInput, EmbeddingProvider, RagError};

/// RRF の定数（評価で選んだ値・上の説明）。
pub(crate) const RRF_K: f64 = 10.0;
/// 文書埋め込みのキャッシュ上限（超えたら空にする・定義の総数は高々数千）。
const CACHE_LIMIT: usize = 20_000;

type CacheKey = (String, u64);
type DocCache = Mutex<HashMap<CacheKey, Arc<Vec<f32>>>>;

/// (モデル版, 文書のハッシュ) → ベクトル。キーは文書の**内容**なので、テナントをまたいで
/// 共有しても何も開示しない（同じ文字列を持つ者だけが同じベクトルに当たる。ベクトルは順位付けに
/// 使うだけで呼び出し側へ返さない）。中身は製品のツール定義と、skill_search では本人の
/// カタログの name と説明。
fn cache() -> &'static DocCache {
    static CACHE: OnceLock<DocCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn text_hash(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// 埋め込みに渡す文書（名前・説明・引数の名前と説明）。
pub(crate) fn doc_text(def: &ToolDef) -> String {
    let mut out = format!("{}: {}", def.name, def.description);
    let props = def
        .input_schema
        .get("properties")
        .and_then(serde_json::Value::as_object);
    if let Some(props) = props.filter(|p| !p.is_empty()) {
        let args: Vec<String> = props
            .iter()
            .map(|(k, v)| {
                let d = v
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                format!("{k}（{d}）")
            })
            .collect();
        out.push_str("\n引数: ");
        out.push_str(&args.join("、"));
    }
    out
}

/// 1 カタログぶんの埋め込み検索器（文書は索引と同じ並び）。
pub(crate) struct Embedder {
    provider: Arc<dyn EmbeddingProvider>,
    docs: Vec<String>,
}

impl Embedder {
    pub(crate) fn new(provider: Arc<dyn EmbeddingProvider>, defs: &[ToolDef]) -> Self {
        Embedder {
            provider,
            docs: defs.iter().map(doc_text).collect(),
        }
    }

    /// 文書のベクトル（キャッシュに無いものだけ埋め込む）。
    async fn doc_vectors(&self, ctx: &AuthContext) -> Result<Vec<Arc<Vec<f32>>>, RagError> {
        let model = self.provider.model_version().to_string();
        let keys: Vec<CacheKey> = self
            .docs
            .iter()
            .map(|d| (model.clone(), text_hash(d)))
            .collect();
        let missing: Vec<String> = {
            let guard = cache()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.docs
                .iter()
                .zip(&keys)
                .filter(|(_, k)| !guard.contains_key(k))
                .map(|(d, _)| d.clone())
                .collect()
        };
        if !missing.is_empty() {
            let resp = self
                .provider
                .embed(ctx, EmbedInput::Document, &missing)
                .await?;
            if resp.vectors.len() != missing.len() {
                return Err(RagError::Worker(
                    "埋め込みの件数がリクエストと合わない".into(),
                ));
            }
            let mut guard = cache()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.len() + missing.len() > CACHE_LIMIT {
                guard.clear();
            }
            for (d, v) in missing.iter().zip(resp.vectors) {
                guard.insert((model.clone(), text_hash(d)), Arc::new(v));
            }
        }
        let guard = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        keys.iter()
            .map(|k| {
                guard
                    .get(k)
                    .cloned()
                    .ok_or_else(|| RagError::Worker("文書の埋め込みが見つからない".into()))
            })
            .collect()
    }

    /// クエリに近い順の文書の添字（コサイン類似度・ベクトルは L2 正規化済み）。
    ///
    /// 全文書の類似度が同じ（クエリの埋め込みが何も区別していない）なら `None`。その並びは
    /// 定義順でしかなく、融合に入れると情報の無い順位が紛れ込む。
    pub(crate) async fn rank(
        &self,
        ctx: &AuthContext,
        query: &str,
    ) -> Result<Option<Vec<usize>>, RagError> {
        let docs = self.doc_vectors(ctx).await?;
        let resp = self
            .provider
            .embed(ctx, EmbedInput::Query, &[query.to_string()])
            .await?;
        let q = resp
            .vectors
            .first()
            .ok_or_else(|| RagError::Worker("クエリの埋め込みが空".into()))?;
        let mut scored: Vec<(usize, f32)> = docs
            .iter()
            .enumerate()
            .map(|(i, d)| (i, d.iter().zip(q).map(|(a, b)| a * b).sum()))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let spread = match (scored.first(), scored.last()) {
            (Some(hi), Some(lo)) => hi.1 - lo.1,
            _ => 0.0,
        };
        if spread <= f32::EPSILON {
            return Ok(None);
        }
        Ok(Some(scored.into_iter().map(|(i, _)| i).collect()))
    }
}

/// 2 つの順位の Reciprocal Rank Fusion（同点は添字順で決定的に）。
pub(crate) fn rrf(a: &[usize], b: &[usize]) -> Vec<usize> {
    let mut score: HashMap<usize, f64> = HashMap::new();
    for list in [a, b] {
        for (rank0, &i) in list.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)] // 順位は高々ツール数（数千）。
            let contribution = 1.0 / (RRF_K + rank0 as f64 + 1.0);
            *score.entry(i).or_insert(0.0) += contribution;
        }
    }
    let mut out: Vec<usize> = score.keys().copied().collect();
    out.sort_by(|x, y| score[y].total_cmp(&score[x]).then(x.cmp(y)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rrf_rewards_agreement_and_breaks_ties_by_index() {
        // 0 は両方で上位・1 は BM25 だけ・2 は埋め込みだけ。
        assert_eq!(rrf(&[0, 1], &[0, 2]), [0, 1, 2]);
        // 片方にしか無い同順位は添字順。
        assert_eq!(rrf(&[3], &[1]), [1, 3]);
    }

    #[test]
    fn doc_text_includes_name_description_and_params() {
        let def = ToolDef::new(
            "csv.query",
            "CSV に SQL を実行する。",
            json!({"properties": {"sql": {"description": "SELECT 文"}}}),
        );
        assert_eq!(
            doc_text(&def),
            "csv.query: CSV に SQL を実行する。\n引数: sql（SELECT 文）"
        );
        let bare = ToolDef::new("noop", "何もしない", json!({}));
        assert_eq!(doc_text(&bare), "noop: 何もしない");
    }
}
