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

fn ctx() -> AuthContext {
    AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "u1".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("t1".into()),
        },
        "org1".into(),
        "t1".into(),
    )
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

    let fused = CatalogSearch::new(&defs, Some(embedder(false)));
    let got = fused.search(&ctx(), "roll up the revenue sheet", 5).await;
    assert_eq!(
        got.first().map(String::as_str),
        Some("csv.query"),
        "{got:?}"
    );
}

#[tokio::test]
async fn agreement_of_both_rankers_wins_and_exact_name_stays_first() {
    let fused = CatalogSearch::new(&catalog(), Some(embedder(false)));
    // BM25 も（「スライド」「編集」）埋め込みも（「スライド」）slide.edit を推す。
    let got = fused.search(&ctx(), "スライドを編集", 2).await;
    assert_eq!(got[0], "slide.edit", "{got:?}");
    // 名前そのものは融合しても先頭。
    let exact = fused.search(&ctx(), "office.edit", 3).await;
    assert_eq!(exact[0], "office.edit", "{exact:?}");
}

#[tokio::test]
async fn embedding_failure_falls_back_to_lexical() {
    let defs = catalog();
    let fused = CatalogSearch::new(&defs, Some(embedder(true)));
    let lexical = CatalogSearch::new(&defs, None);
    for q in ["スライドを編集", "roll up the revenue sheet"] {
        assert_eq!(
            fused.search(&ctx(), q, 5).await,
            lexical.search_lexical(q, 5),
            "{q}"
        );
    }
}

#[tokio::test]
async fn select_and_required_terms_behave_as_in_lexical_search() {
    let fused = CatalogSearch::new(&catalog(), Some(embedder(false)));
    assert_eq!(
        fused.search(&ctx(), "select:save_note", 5).await,
        ["save_note"]
    );
    // `+csv` は名前に csv を含むものだけ（埋め込みの順位にも効かせる）。
    let got = fused.search(&ctx(), "+csv revenue", 5).await;
    assert!(got.iter().all(|n| n.starts_with("csv.")), "{got:?}");
    assert_eq!(got.first().map(String::as_str), Some("csv.query"));
}

#[tokio::test]
async fn document_embeddings_are_reused_across_searches() {
    let e = embedder(false);
    // 他のテストとキャッシュを共有しないよう、このテストだけの説明にする。
    let defs = vec![
        def("csv.query", "キャッシュ確認用の説明 A。"),
        def("office.edit", "キャッシュ確認用の説明 B。"),
    ];
    let fused = CatalogSearch::new(&defs, Some(e.clone()));
    fused.search(&ctx(), "revenue", 5).await;
    let after_first = e.doc_calls.load(Ordering::SeqCst);
    assert_eq!(after_first, 2);
    // 同じ定義の別インスタンス（次の run）でも埋め込み直さない。
    let again = CatalogSearch::new(&defs, Some(e.clone()));
    again.search(&ctx(), "revenue", 5).await;
    assert_eq!(e.doc_calls.load(Ordering::SeqCst), after_first);
}

#[tokio::test]
async fn uninformative_query_embedding_is_ignored() {
    // ヒントの無いクエリは全文書と同じ類似度（0）になる。その並び（定義順）は情報ではないので
    // 融合に入れず BM25 だけで返す（入れると定義順が順位に紛れ込む）。
    let defs = catalog();
    let fused = CatalogSearch::new(&defs, Some(embedder(false)));
    let lexical = CatalogSearch::new(&defs, None);
    let q = "ノートの下書きを作る";
    assert_eq!(
        fused.search(&ctx(), q, 5).await,
        lexical.search_lexical(q, 5)
    );
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
