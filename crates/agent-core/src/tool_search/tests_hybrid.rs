//! 埋め込みとの融合（#517）・名前空間の要約（#519）のテスト。偽の埋め込みで決定的に回す。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use authz::AuthContext;
use llm_gateway::ToolDef;
use rag::{EmbedInput, EmbedResponse, EmbeddingProvider, RagError};
use serde_json::json;

use super::{CatalogSearch, NAME_LIST_LIMIT};

/// 文書は「名前の先頭語」ごとの one-hot、クエリは `hint` に挙げた語を含めば対応する
/// 次元を立てる（BM25 では当たらない言い換えを埋め込みだけが拾う状況を作る）。
struct FakeEmbedder {
    dims: Vec<&'static str>,
    hints: Vec<(&'static str, &'static str)>,
    doc_calls: AtomicUsize,
    fail: bool,
}

impl FakeEmbedder {
    fn vector(&self, pick: impl Fn(&str) -> bool) -> Vec<f32> {
        self.dims
            .iter()
            .map(|d| if pick(d) { 1.0 } else { 0.0 })
            .collect()
    }
}

#[async_trait]
impl EmbeddingProvider for FakeEmbedder {
    async fn embed(
        &self,
        _ctx: &AuthContext,
        input: EmbedInput,
        texts: &[String],
    ) -> Result<EmbedResponse, RagError> {
        if self.fail {
            return Err(RagError::Worker("down".into()));
        }
        let vectors = texts
            .iter()
            .map(|t| match input {
                EmbedInput::Document => {
                    self.doc_calls.fetch_add(1, Ordering::SeqCst);
                    self.vector(|d| t.starts_with(d))
                }
                EmbedInput::Query => {
                    let hit = self
                        .hints
                        .iter()
                        .find(|(h, _)| t.contains(h))
                        .map(|(_, d)| *d);
                    self.vector(|d| Some(d) == hit)
                }
            })
            .collect();
        Ok(EmbedResponse {
            vectors,
            model_version: "fake".into(),
            dimension: self.dims.len(),
        })
    }

    fn model_version(&self) -> &str {
        "fake"
    }
}

/// テストごとに別のテナントにする（文書ベクトルのキャッシュは tenant/org 単位で共有される）。
fn ctx_of(tenant: &str) -> AuthContext {
    AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "u1".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "org1".into(),
        tenant.into(),
    )
}

/// 温めてから検索する（文書ベクトルが無いうちは BM25 のみで返す仕様のため）。
async fn warmed(defs: &[ToolDef], e: Arc<FakeEmbedder>, c: &AuthContext) -> CatalogSearch {
    let cs = CatalogSearch::new(defs, Some(e));
    cs.warm(c).await;
    cs
}

fn def(name: &str, description: &str) -> ToolDef {
    ToolDef::new(
        name,
        description,
        json!({ "type": "object", "properties": {} }),
    )
}

fn catalog() -> Vec<ToolDef> {
    vec![
        def("csv.query", "CSV に SQL を実行して集計する。"),
        def("office.edit", "Office ファイルを編集する。"),
        def("slide.edit", "スライドを編集する。"),
        def("save_note", "ノートの下書きを作る。"),
    ]
}

fn embedder(fail: bool) -> Arc<FakeEmbedder> {
    Arc::new(FakeEmbedder {
        dims: vec!["csv.query", "office.edit", "slide.edit", "save_note"],
        // 英語の依頼は日本語の説明と語が重ならず、BM25 では当たらない（埋め込みだけが拾う）。
        hints: vec![("revenue", "csv.query"), ("スライド", "slide.edit")],
        doc_calls: AtomicUsize::new(0),
        fail,
    })
}

#[tokio::test]
async fn embedding_rescues_a_query_bm25_cannot_match() {
    let defs = catalog();
    let lexical = CatalogSearch::new(&defs, None);
    let lex = lexical.search_lexical("roll up the revenue sheet", 5);
    assert_ne!(
        lex.first().map(String::as_str),
        Some("csv.query"),
        "{lex:?}"
    );

    let c = ctx_of("rescue");
    let fused = warmed(&defs, embedder(false), &c).await;
    let got = fused.search(&c, "roll up the revenue sheet", 5).await;
    assert_eq!(
        got.first().map(String::as_str),
        Some("csv.query"),
        "{got:?}"
    );
}

#[tokio::test]
async fn agreement_of_both_rankers_wins_and_exact_name_stays_first() {
    let c = ctx_of("agree");
    let fused = warmed(&catalog(), embedder(false), &c).await;
    // BM25 も（「スライド」「編集」）埋め込みも（「スライド」）slide.edit を推す。
    let got = fused.search(&c, "スライドを編集", 2).await;
    assert_eq!(got[0], "slide.edit", "{got:?}");
    // 名前そのものは融合しても先頭。
    let exact = fused.search(&c, "office.edit", 3).await;
    assert_eq!(exact[0], "office.edit", "{exact:?}");
}

#[tokio::test]
async fn embedding_failure_falls_back_to_lexical() {
    let defs = catalog();
    let c = ctx_of("fail");
    // 文書は正常な埋め込みで温め（同じモデル版＝同じキャッシュ）、クエリの埋め込みだけ失敗させる。
    warmed(&defs, embedder(false), &c).await;
    let fused = CatalogSearch::new(&defs, Some(embedder(true)));
    let lexical = CatalogSearch::new(&defs, None);
    for q in ["スライドを編集", "roll up the revenue sheet"] {
        assert_eq!(
            fused.search(&c, q, 5).await,
            lexical.search_lexical(q, 5),
            "{q}"
        );
    }
}

#[tokio::test]
async fn select_and_required_terms_behave_as_in_lexical_search() {
    let c = ctx_of("select");
    let fused = warmed(&catalog(), embedder(false), &c).await;
    assert_eq!(fused.search(&c, "select:save_note", 5).await, ["save_note"]);
    // `+csv` は名前に csv を含むものだけ（埋め込みの順位にも効かせる）。
    let got = fused.search(&c, "+csv revenue", 5).await;
    assert!(got.iter().all(|n| n.starts_with("csv.")), "{got:?}");
    assert_eq!(got.first().map(String::as_str), Some("csv.query"));
}

#[tokio::test]
async fn document_embeddings_are_reused_and_scoped_per_tenant() {
    let e = embedder(false);
    let defs = catalog();
    let a = ctx_of("reuse-a");
    warmed(&defs, e.clone(), &a).await;
    assert_eq!(e.doc_calls.load(Ordering::SeqCst), defs.len());
    // 同じテナントなら次の run（別インスタンス）でも埋め込み直さない。
    let again = warmed(&defs, e.clone(), &a).await;
    assert_eq!(e.doc_calls.load(Ordering::SeqCst), defs.len());
    assert_eq!(again.search(&a, "revenue", 1).await, ["csv.query"]);
    // 別テナントは共有しない（キャッシュの存在が他テナントから観測できないように）。
    warmed(&defs, e.clone(), &ctx_of("reuse-b")).await;
    assert_eq!(e.doc_calls.load(Ordering::SeqCst), 2 * defs.len());
}

#[tokio::test]
async fn cold_cache_answers_lexically_and_warms_in_the_background() {
    let e = embedder(false);
    let defs = catalog();
    let c = ctx_of("cold");
    let cs = CatalogSearch::new(&defs, Some(e.clone()));
    // 文書ベクトルがまだ無い最初の検索は待たずに BM25 で返す。
    let lexical = CatalogSearch::new(&defs, None);
    let q = "roll up the revenue sheet";
    assert_eq!(cs.search(&c, q, 5).await, lexical.search_lexical(q, 5));
    // 裏で温まったら融合で返す。
    for _ in 0..50 {
        if e.doc_calls.load(Ordering::SeqCst) == defs.len() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(
        cs.search(&c, q, 5).await.first().map(String::as_str),
        Some("csv.query")
    );
}

#[tokio::test]
async fn two_catalogs_of_one_tenant_warm_without_dropping_each_other() {
    // tool_search と skill_search のように、同じ tenant/org で別のカタログを続けて温める。
    let e = embedder(false);
    let c = ctx_of("two-catalogs");
    let tools = catalog();
    let skills = vec![
        def("expense-check", "経費精算の確認"),
        def("weekly-report", "週報を書く"),
    ];
    let a = CatalogSearch::new(&tools, Some(e.clone()));
    let b = CatalogSearch::new(&skills, Some(e.clone()));
    a.prewarm(&c);
    b.prewarm(&c);
    for _ in 0..100 {
        if e.doc_calls.load(Ordering::SeqCst) == tools.len() + skills.len() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // 後から来たカタログの温めも捨てられず、両方の文書が 1 回ずつ埋め込まれる。
    assert_eq!(
        e.doc_calls.load(Ordering::SeqCst),
        tools.len() + skills.len()
    );
}

#[tokio::test]
async fn mismatched_dimensions_fall_back_to_lexical() {
    // クエリだけ次元が違う（モデルの差し替え・設定ミス）→ 切り詰めて掛けずに BM25 へ。
    struct Skewed(Arc<FakeEmbedder>);
    #[async_trait]
    impl EmbeddingProvider for Skewed {
        async fn embed(
            &self,
            ctx: &AuthContext,
            input: EmbedInput,
            texts: &[String],
        ) -> Result<EmbedResponse, RagError> {
            let mut r = self.0.embed(ctx, input, texts).await?;
            if input == EmbedInput::Query {
                for v in &mut r.vectors {
                    v.push(1.0);
                }
            }
            Ok(r)
        }
        fn model_version(&self) -> &str {
            "fake-skewed"
        }
    }
    let defs = catalog();
    let c = ctx_of("skew");
    let cs = CatalogSearch::new(&defs, Some(Arc::new(Skewed(embedder(false)))));
    cs.warm(&c).await;
    let q = "roll up the revenue sheet";
    assert_eq!(
        cs.search(&c, q, 5).await,
        CatalogSearch::new(&defs, None).search_lexical(q, 5)
    );
}

#[tokio::test]
async fn uninformative_query_embedding_is_ignored() {
    // ヒントの無いクエリは全文書と同じ類似度（0）になる。その並び（定義順）は情報ではないので
    // 融合に入れず BM25 だけで返す（入れると定義順が順位に紛れ込む）。
    let defs = catalog();
    let c = ctx_of("uninformative");
    let fused = warmed(&defs, embedder(false), &c).await;
    let lexical = CatalogSearch::new(&defs, None);
    let q = "ノートの下書きを作る";
    assert_eq!(fused.search(&c, q, 5).await, lexical.search_lexical(q, 5));
}

#[test]
fn large_catalogs_list_namespaces_instead_of_every_name() {
    let mut defs: Vec<ToolDef> = (0..60)
        .map(|i| def(&format!("github_op{i}"), "GitHub"))
        .collect();
    defs.extend((0..60).map(|i| def(&format!("slack_op{i}"), "Slack")));
    assert!(defs.len() > NAME_LIST_LIMIT);
    let desc = CatalogSearch::new(&defs, None).definition().description;
    assert!(
        desc.contains("github（60）") && desc.contains("slack（60）"),
        "{desc}"
    );
    assert!(
        !desc.contains("github_op59"),
        "名前は全部は並べない: {desc}"
    );

    let small = CatalogSearch::new(&catalog(), None)
        .definition()
        .description;
    assert!(small.contains("csv.query, office.edit"), "{small}");
}
