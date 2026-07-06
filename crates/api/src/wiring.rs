//! 依存配線（RAG・チャット）。main の起動フローから切り出す（1 ファイル 500 行規約）。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use authz::AuthzClient;
use storage::{IndexerStorage, ObjectStore};

use api::config::{AppConfig, LlmBackend, VectorStoreBackend};

/// RAG 配線の結果（検索サービスはオプション・テナント消去は常設）。
pub(crate) type RagWiring = (Option<Arc<rag::SearchService>>, Arc<rag::RagAdmin>);

/// RAG（Phase 2）の依存配線。`rag.enabled=false` なら何も起動せず `None` を返す。
///
/// 依存は全てトレイト裏（DocumentParser/EmbeddingProvider/Reranker/VectorStore/
/// FulltextIndex）。クラウド/オンプレ差はここでの実装選択に閉じる。
pub(crate) fn wire_rag(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    object_store: &Arc<dyn ObjectStore>,
    authz: &Arc<dyn AuthzClient>,
) -> anyhow::Result<RagWiring> {
    if !config.rag.enabled {
        tracing::info!("rag.enabled=false: インジェスト・検索は無効（/search は 503）");
        // テナント消去の DB 行掃除は RAG 無効でも行う（過去に有効だった残骸対策）。
        return Ok((None, Arc::new(rag::RagAdmin::new(db.clone(), None, None))));
    }
    if config.vector.backend != VectorStoreBackend::Qdrant {
        anyhow::bail!("vector.backend=pgvector は未実装です（Phase 2 は qdrant のみ）");
    }
    let rag_cfg = config.rag.clone();
    let _ = http; // RAG 依存は専用のタイムアウト付きクライアントを使う（下記）。
                  // 共有クライアントは無期限のため、worker/Qdrant の遅延が /search・インジェストを
                  // 永久ブロックし得る。parse は Docling+OCR で長い（大きな PDF）ため別枠で長めに取る。
    let rag_http = reqwest::Client::builder()
        .timeout(Duration::from_mins(1))
        .build()
        .context("RAG 用 HTTP クライアントの構築に失敗")?;
    let parse_http = reqwest::Client::builder()
        .timeout(Duration::from_mins(5))
        .build()
        .context("parse 用 HTTP クライアントの構築に失敗")?;
    let parser: Arc<dyn rag::DocumentParser> = Arc::new(rag::HttpDocumentParser::new(
        parse_http,
        &rag_cfg.worker_base_url,
    ));
    let embedder: Arc<dyn rag::EmbeddingProvider> = Arc::new(rag::HttpEmbeddingProvider::new(
        rag_http.clone(),
        &rag_cfg.worker_base_url,
        &rag_cfg.embedding_model_version,
    ));
    let reranker: Arc<dyn rag::Reranker> = Arc::new(rag::HttpReranker::new(
        rag_http.clone(),
        &rag_cfg.worker_base_url,
    ));
    let vector: Arc<dyn rag::VectorStore> = Arc::new(rag::QdrantVectorStore::new(
        rag_http,
        &rag_cfg.qdrant_url,
        &rag_cfg.embedding_model_version,
    ));
    let fulltext: Arc<dyn rag::FulltextIndex> =
        Arc::new(rag::TantivyFulltext::new(&rag_cfg.index_data_dir));
    let indexer_storage = Arc::new(IndexerStorage::new(db.clone(), Arc::clone(object_store)));
    // relay+consumer（advisory lock でリーダー選出。多重起動しても安全）。
    rag::spawn_pipeline(rag::PipelineDeps {
        pool: db.clone(),
        config: rag_cfg.clone(),
        parser,
        embedder: Arc::clone(&embedder),
        vector: Arc::clone(&vector),
        fulltext: Arc::clone(&fulltext),
        indexer_storage,
    });
    tracing::info!(
        worker = %rag_cfg.worker_base_url, qdrant = %rag_cfg.qdrant_url,
        "RAG パイプラインと検索を有効化しました"
    );
    let rag_admin = Arc::new(rag::RagAdmin::new(
        db.clone(),
        Some(Arc::clone(&vector)),
        Some(Arc::clone(&fulltext)),
    ));
    Ok((
        Some(Arc::new(rag::SearchService::new(
            db.clone(),
            rag_cfg,
            embedder,
            reranker,
            vector,
            fulltext,
            Arc::clone(authz),
            storage::audit::AuditRecorder::new(db.clone()),
        ))),
        rag_admin,
    ))
}

/// チャット（Phase 3）の依存配線。`chat.enabled=false` なら何もせず `None` を返す。
///
/// llm-gateway（in-process チョークポイント）を config.llm から構築し、chat ストア＋生成ワーカー
/// プールを起動する。プロバイダは OpenAI 互換ファースト（vLLM もこれで賄う）。
pub(crate) async fn wire_chat(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    authz: &Arc<dyn AuthzClient>,
    search: Option<&Arc<rag::SearchService>>,
) -> anyhow::Result<Option<Arc<chat::ChatStore>>> {
    use llm_gateway::{
        GatewayConfig, LangfuseConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig,
        ProviderKind,
    };

    if !config.chat.enabled {
        tracing::info!("chat.enabled=false: チャットは無効（/threads 系は 503）");
        return Ok(None);
    }

    let llm = &config.llm;
    let kind = match llm.backend {
        LlmBackend::Vllm | LlmBackend::Openai => ProviderKind::Openai,
        LlmBackend::Anthropic => ProviderKind::Anthropic,
        LlmBackend::Stub => ProviderKind::Stub,
        other => anyhow::bail!(
            "llm.backend={other:?} は未実装です（Phase 3 は openai-compat/anthropic/stub）"
        ),
    };
    let default_model = llm
        .default_model
        .clone()
        .or_else(|| llm.models.first().map(|m| m.id.clone()))
        .unwrap_or_else(|| "default".to_string());
    let models: Vec<ModelEntry> = if llm.models.is_empty() {
        // カタログ未設定なら default_model を素通しする単一エントリを合成（単価 0）。
        vec![ModelEntry {
            id: default_model.clone(),
            real_id: None,
            prompt_price_micros_per_mtok: 0,
            completion_price_micros_per_mtok: 0,
        }]
    } else {
        llm.models
            .iter()
            .map(|m| ModelEntry {
                id: m.id.clone(),
                real_id: m.real_id.clone(),
                prompt_price_micros_per_mtok: m.prompt_price_micros_per_mtok,
                completion_price_micros_per_mtok: m.completion_price_micros_per_mtok,
            })
            .collect()
    };
    let gateway_config = GatewayConfig {
        provider: ProviderConfig {
            kind,
            base_url: llm.base_url.clone(),
            api_key: llm.api_key.clone(),
            timeout_secs: 120,
        },
        catalog: ModelCatalog {
            default_model,
            models,
        },
        langfuse: llm.langfuse.as_ref().map(|l| LangfuseConfig {
            base_url: l.base_url.clone(),
            public_key: l.public_key.clone(),
            secret_key: l.secret_key.clone(),
        }),
    };
    let gateway = LlmGateway::build(db.clone(), http.clone(), gateway_config)
        .map_err(|e| anyhow::anyhow!("llm-gateway 構築に失敗: {e}"))?;

    // pub/sub は専用 URL があればそれ、無ければ BFF セッションと同じ Redis を再利用。
    let redis_url = config
        .chat
        .redis_url
        .clone()
        .unwrap_or_else(|| config.session.redis_url.clone());
    let store = chat::ChatStore::connect(db.clone(), Arc::clone(authz), Some(&redis_url))
        .await
        .map_err(|e| anyhow::anyhow!("chat store 構築に失敗: {e}"))?;

    let worker_config = chat::WorkerConfig {
        system_prompt: config
            .chat
            .system_prompt
            .clone()
            .unwrap_or_else(|| chat::WorkerConfig::default().system_prompt),
        model: config.llm.default_model.clone(),
        lease_secs: config.chat.lease_secs,
        max_steps: config.chat.max_steps,
    };
    let worker = chat::ChatWorker::new(
        db.clone(),
        store.clone(),
        gateway,
        search.cloned(),
        worker_config,
    );
    // ワーカータスクは detach（プロセス生存中は走り続ける）。
    worker.spawn(config.chat.worker_concurrency);
    tracing::info!(
        concurrency = config.chat.worker_concurrency,
        backend = ?config.llm.backend,
        "チャット生成ワーカーを起動しました"
    );
    Ok(Some(Arc::new(store)))
}
