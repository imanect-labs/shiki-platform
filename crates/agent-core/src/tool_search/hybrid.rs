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

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use authz::AuthContext;
use llm_gateway::ToolDef;
use rag::{EmbedInput, EmbeddingProvider, RagError};

/// RRF の定数（評価で選んだ値・上の説明）。
pub(crate) const RRF_K: f64 = 10.0;
/// 1 tenant/org あたりの文書ベクトルの上限（超えたら、いま使う分だけ残して捨てる）。
const TENANT_CACHE_LIMIT: usize = 5_000;
/// キャッシュを持つ tenant/org の数の上限（超えたら最も長く使われていないものから追い出す）。
const TENANT_LIMIT: usize = 1_000;
/// クエリの埋め込みを待つ上限。超えたら BM25 だけで返す（ingestion-worker がインジェストで
/// 詰まっていても、検索の 1 手を数十秒止めない）。平常時は 1 回 40ms 程度。
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

type DocKey = (String, u64);

/// 1 テナントぶんの文書ベクトル。
#[derive(Default)]
struct TenantCache {
    vectors: HashMap<DocKey, Arc<Vec<f32>>>,
    /// 裏で埋め込み中の文書（同じ文書を同時に何本も送らない・single-flight）。文書単位で持つので、
    /// 同じ tenant/org で別のカタログ（tool_search と skill_search）を同時に温めても互いを捨てない。
    in_flight: HashSet<DocKey>,
    /// 最後に使った順番（追い出すときに最も古いものを選ぶ）。
    last_used: u64,
}

/// `scope` の枠を取る。**新しい枠を作る前に**数の上限を見る（作ってからでは常に「既にある」に
/// なり、上限が効かずに tenant/org の数だけ増え続ける）。上限なら最も長く使われていない枠を
/// 1 つだけ追い出す（全消去すると、上限を少し超える規模で温めが毎回やり直しになる）。
/// 埋め込み中の枠は追い出さない（戻ってきた結果の置き場を失わない）。
fn tenant_entry(guard: &mut HashMap<String, TenantCache>, scope: String) -> &mut TenantCache {
    static TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !guard.contains_key(&scope) && guard.len() >= TENANT_LIMIT {
        let oldest = guard
            .iter()
            .filter(|(_, t)| t.in_flight.is_empty())
            .min_by_key(|(_, t)| t.last_used)
            .map(|(k, _)| k.clone());
        if let Some(k) = oldest {
            guard.remove(&k);
        }
    }
    let tenant = guard.entry(scope).or_default();
    tenant.last_used = now;
    tenant
}

/// `{tenant_id}/{org}` → 文書ベクトル。**キャッシュは tenant/org で閉じる**（design §4.3 の
/// キャッシュキー規約）。内容のハッシュだけで共有すると「他テナントが同じ文書を持つか」の
/// 存在オラクルになる（PIT-14 と同型・skill_search では本人のカタログの name と説明が入る）。
/// 分けておけば、上限での追い出しも他テナントへ波及せず、埋め込みは必ず自分の `AuthContext`
/// で呼ばれる（worker 側の追跡・計上が正しい）。
type Cache = Mutex<HashMap<String, TenantCache>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<String, TenantCache>> {
    cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// キャッシュを閉じる単位（`{tenant_id}/{org}`）。
fn scope(ctx: &AuthContext) -> String {
    format!("{}/{}", ctx.tenant_id, ctx.org)
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

    fn keys(&self) -> Vec<DocKey> {
        let model = self.provider.model_version().to_string();
        self.docs
            .iter()
            .map(|d| (model.clone(), text_hash(d)))
            .collect()
    }

    /// キャッシュ済みの文書ベクトル（全部そろっていれば `Some`）。欠けていれば `None` を返し、
    /// まだ誰も温めていなければ裏で温め始める（この検索は BM25 だけで返す）。
    fn cached_or_warm(&self, ctx: &AuthContext) -> Option<Vec<Arc<Vec<f32>>>> {
        let keys = self.keys();
        let mut guard = lock();
        let tenant = tenant_entry(&mut guard, scope(ctx));
        let found: Vec<Option<Arc<Vec<f32>>>> = keys
            .iter()
            .map(|k| tenant.vectors.get(k).cloned())
            .collect();
        if found.iter().all(Option::is_some) {
            return Some(found.into_iter().flatten().collect());
        }
        // 欠けていて、まだ誰も埋め込んでいない文書だけを裏で埋め込む。
        let missing: Vec<(String, DocKey)> = self
            .docs
            .iter()
            .zip(&keys)
            .zip(&found)
            .filter(|((_, k), v)| v.is_none() && !tenant.in_flight.contains(*k))
            .map(|((d, k), _)| (d.clone(), k.clone()))
            .collect();
        if !missing.is_empty() {
            tenant
                .in_flight
                .extend(missing.iter().map(|(_, k)| k.clone()));
            let missing: Vec<String> = missing.into_iter().map(|(d, _)| d).collect();
            let provider = Arc::clone(&self.provider);
            let ctx = ctx.clone();
            drop(guard);
            tokio::spawn(async move { warm(provider, ctx, missing, keys).await });
        }
        None
    }

    /// 文書ベクトルを裏で温め始める（欠けていれば・待たない）。
    pub(crate) fn prewarm(&self, ctx: &AuthContext) {
        if self.cached_or_warm(ctx).is_none() {
            tracing::debug!("tool search: 文書の埋め込みを裏で温める");
        }
    }

    /// 文書ベクトルを温め終わるまで待つ。
    pub(crate) async fn warm(&self, ctx: &AuthContext) {
        let missing: Vec<String> = {
            let guard = lock();
            let have = guard.get(&scope(ctx));
            self.docs
                .iter()
                .zip(self.keys())
                .filter(|(_, k)| have.is_none_or(|t| !t.vectors.contains_key(k)))
                .map(|(d, _)| d.clone())
                .collect()
        };
        if !missing.is_empty() {
            warm(
                Arc::clone(&self.provider),
                ctx.clone(),
                missing,
                self.keys(),
            )
            .await;
        }
    }

    /// クエリに近い順の文書の添字（コサイン類似度・ベクトルは L2 正規化済み）。
    ///
    /// `Ok(None)` は「埋め込みを使わない」: 文書ベクトルがまだ温まっていない（裏で温め始める）、
    /// または全文書の類似度が同じ（クエリの埋め込みが何も区別していない・その並びは定義順で
    /// しかなく、融合に入れると情報の無い順位が紛れ込む）。
    pub(crate) async fn rank(
        &self,
        ctx: &AuthContext,
        query: &str,
    ) -> Result<Option<Vec<usize>>, RagError> {
        let Some(docs) = self.cached_or_warm(ctx) else {
            return Ok(None);
        };
        let resp = tokio::time::timeout(
            QUERY_TIMEOUT,
            self.provider
                .embed(ctx, EmbedInput::Query, &[query.to_string()]),
        )
        .await
        .map_err(|_| RagError::Worker("クエリの埋め込みが時間内に返らない".into()))??;
        let q = resp
            .vectors
            .first()
            .ok_or_else(|| RagError::Worker("クエリの埋め込みが空".into()))?;
        // 次元が食い違うベクトル同士を切り詰めて掛けない（モデルの差し替え・設定ミス）。
        if docs.iter().any(|d| d.len() != q.len()) {
            return Err(RagError::Worker(
                "埋め込みの次元が文書とクエリで合わない".into(),
            ));
        }
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

/// 欠けている文書を埋め込んでキャッシュへ入れる（自テナントの `AuthContext` で呼ぶ）。
///
/// 上限を超えたら、そのテナントの分のうち `keep`（いま使う文書）以外を捨てる。失敗しても
/// 何も入れずに埋め込み中の印を外すだけ（次の検索で取り直す・それまでは BM25 のみ）。
async fn warm(
    provider: Arc<dyn EmbeddingProvider>,
    ctx: AuthContext,
    missing: Vec<String>,
    keep: Vec<DocKey>,
) {
    let model = provider.model_version().to_string();
    let result = provider.embed(&ctx, EmbedInput::Document, &missing).await;
    let mut guard = lock();
    let tenant = tenant_entry(&mut guard, scope(&ctx));
    for d in &missing {
        tenant.in_flight.remove(&(model.clone(), text_hash(d)));
    }
    match result {
        Ok(resp) if resp.vectors.len() == missing.len() => {
            if tenant.vectors.len() + missing.len() > TENANT_CACHE_LIMIT {
                let keep: HashSet<&DocKey> = keep.iter().collect();
                tenant.vectors.retain(|k, _| keep.contains(k));
            }
            for (d, v) in missing.iter().zip(resp.vectors) {
                tenant
                    .vectors
                    .insert((model.clone(), text_hash(d)), Arc::new(v));
            }
        }
        Ok(_) => tracing::warn!("tool search: 文書の埋め込みの件数がリクエストと合わない"),
        Err(e) => {
            tracing::warn!(error = %e, "tool search: 文書の埋め込みに失敗（BM25 のみで続ける）");
        }
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
    fn full_scope_table_evicts_only_the_least_recently_used() {
        let mut table: HashMap<String, TenantCache> = HashMap::new();
        for i in 0..TENANT_LIMIT {
            tenant_entry(&mut table, format!("t{i}/o"));
        }
        // t0 を使い直して「最近使った」側へ回す。追い出されるのは次に古い t1。
        tenant_entry(&mut table, "t0/o".into());
        tenant_entry(&mut table, "new/o".into());
        assert_eq!(table.len(), TENANT_LIMIT);
        assert!(table.contains_key("t0/o") && table.contains_key("new/o"));
        assert!(!table.contains_key("t1/o"));
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
