//! 依存配線（RAG・チャット）。main の起動フローから切り出す（1 ファイル 500 行規約）。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use authz::AuthzClient;
use storage::{IndexerStorage, ObjectStore};

use api::config::{AppConfig, LlmBackend, VectorStoreBackend, WebSearchBackend};

/// RAG 配線の結果（検索サービスはオプション・テナント消去は常設）。
pub(crate) type RagWiring = (Option<Arc<rag::SearchService>>, Arc<rag::RagAdmin>);

/// オブジェクトストア＋StorageService を構築する（main の起動フローから切り出し）。
///
/// GCS は Phase 8 で同 trait 裏に追加する。s3 設定の必須チェックは minio の分岐内で行う
/// （gcs 選択時に s3 未設定エラーで誤って落ちないようにする）。
pub(crate) async fn wire_storage(
    config: &AppConfig,
    db: &sqlx::PgPool,
    authz: &Arc<dyn AuthzClient>,
) -> anyhow::Result<(Arc<dyn ObjectStore>, Arc<storage::StorageService>)> {
    use api::config::ObjectStoreBackend;
    let (object_store, presign_get_ttl, presign_put_ttl): (Arc<dyn ObjectStore>, _, _) =
        match config.storage.backend {
            ObjectStoreBackend::Minio => {
                let s3 = config
                    .storage
                    .s3
                    .as_ref()
                    .context("storage.s3 が未設定です（backend=minio）")?;
                (
                    Arc::new(storage::S3ObjectStore::new(s3)) as Arc<dyn ObjectStore>,
                    s3.presign_get_ttl(),
                    s3.presign_put_ttl(),
                )
            }
            ObjectStoreBackend::Gcs => {
                anyhow::bail!("storage.backend=gcs は Phase 8 で実装予定です")
            }
        };
    object_store
        .ensure_bucket()
        .await
        .context("オブジェクトストアのバケット準備に失敗")?;
    let service = Arc::new(storage::StorageService::new(
        db.clone(),
        object_store.clone(),
        Arc::clone(authz),
        presign_get_ttl,
        presign_put_ttl,
        config.storage.max_upload_size_bytes,
    ));
    Ok((object_store, service))
}

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
/// llm-gateway を構築する（chat・workflow の双方から使う共通経路）。
pub(crate) fn build_gateway(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
) -> anyhow::Result<llm_gateway::LlmGateway> {
    use llm_gateway::{
        GatewayConfig, LangfuseConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig,
        ProviderKind,
    };
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
    LlmGateway::build(db.clone(), http.clone(), gateway_config)
        .map_err(|e| anyhow::anyhow!("llm-gateway 構築に失敗: {e}"))
}

/// サンドボックスクライアントを構築する（`chat.sandbox_endpoint` 設定時のみ・chat/workflow 共通）。
pub(crate) fn build_sandbox(
    config: &AppConfig,
) -> anyhow::Result<Option<Arc<dyn agent_core::Sandbox>>> {
    let Some(endpoint) = &config.chat.sandbox_endpoint else {
        return Ok(None);
    };
    let client = sandbox_client::GrpcSandboxClient::connect_lazy(endpoint.clone())
        .map_err(|e| anyhow::anyhow!("sandbox client 構築に失敗: {e}"))?;
    tracing::info!(%endpoint, "code_interpreter サンドボックスを配線しました");
    Ok(Some(Arc::new(client)))
}

pub(crate) async fn wire_chat(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    authz: &Arc<dyn AuthzClient>,
    search: Option<&Arc<rag::SearchService>>,
    storage: &Arc<storage::StorageService>,
    ui_validator: &Arc<gui::SpecValidator>,
) -> anyhow::Result<Option<Arc<chat::ChatStore>>> {
    if !config.chat.enabled {
        tracing::info!("chat.enabled=false: チャットは無効（/threads 系は 503）");
        return Ok(None);
    }

    let gateway = build_gateway(config, http, db)?;

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
        // コード実行系の隔離ティア（admin ポリシー）。未指定は既定（wasm）。
        sandbox_backend: config
            .chat
            .sandbox_backend
            .unwrap_or_else(|| chat::WorkerConfig::default().sandbox_backend),
        // 自律プロファイルの既定（予算/ステップ/software）は WorkerConfig::default を踏襲する。
        ..chat::WorkerConfig::default()
    };
    // サンドボックス（code_interpreter / web_fetch）: エンドポイント設定時のみ配線する。
    // 成果物保存（StorageService 裏・発話ユーザー権限）もサンドボックスとセットで配線する。
    let sandbox: Option<Arc<dyn agent_core::Sandbox>> = build_sandbox(config)?;
    let artifacts: Option<Arc<dyn agent_core::ArtifactStore>> = sandbox
        .as_ref()
        .map(|_| Arc::new(chat::StorageArtifactStore::new(Arc::clone(storage))) as _);
    let web_search = wire_websearch(config, http)?;
    let worker = chat::ChatWorker::new(
        db.clone(),
        store.clone(),
        chat::WorkerDeps {
            gateway,
            search: search.cloned(),
            sandbox,
            artifacts,
            web_search,
            // 自律プロファイルのワークスペース（file CRUD/shell・Task 5.4）。
            storage: Some(Arc::clone(storage)),
            // generative UI（emit_ui・Task 6.4）。
            ui_validator: Some(Arc::clone(ui_validator)),
        },
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

/// web 検索プロバイダ（Phase 4 web ツール）の配線。`websearch.backend` 未指定なら `None`。
///
/// クラウド/オンプレ差は `SearchProvider` トレイト裏で吸収する（Brave=SaaS / SearXNG=オンプレ /
/// Stub=テスト・エアギャップ）。
fn wire_websearch(
    config: &AppConfig,
    http: &reqwest::Client,
) -> anyhow::Result<Option<Arc<dyn websearch::SearchProvider>>> {
    let Some(backend) = config.websearch.backend else {
        return Ok(None);
    };
    // 検索は対話パスで呼ばれるため、共有クライアント（無期限）ではなく短いタイムアウトを敷く。
    let _ = http;
    let search_http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("web 検索用 HTTP クライアントの構築に失敗")?;
    let provider: Arc<dyn websearch::SearchProvider> = match backend {
        WebSearchBackend::Brave => {
            // compose の `${VAR:-}` は空文字を渡し得るため、空も未設定として扱い fail-fast する。
            let api_key = config
                .websearch
                .brave_api_key
                .clone()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("websearch.backend=brave には brave_api_key が必要です")
                })?;
            Arc::new(websearch::BraveSearchProvider::new(
                search_http,
                api_key,
                None,
            ))
        }
        WebSearchBackend::Searxng => {
            let base_url = config
                .websearch
                .searxng_base_url
                .clone()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("websearch.backend=searxng には searxng_base_url が必要です")
                })?;
            Arc::new(websearch::SearxngSearchProvider::new(
                search_http,
                &base_url,
            ))
        }
        WebSearchBackend::Stub => Arc::new(websearch::StubSearchProvider::new()),
    };
    tracing::info!(
        provider = provider.name(),
        "web 検索プロバイダを配線しました"
    );
    Ok(Some(provider))
}

/// generative UI の検証層＋UI スペックストアを配線する（Task 6.3）。
///
/// 検証は保存（UiSpecStore）・発話（emit_ui）・解決（ミニアプリ）の全経路が同一実装を共有する。
pub(crate) fn wire_gui(
    db: &sqlx::PgPool,
    artifacts: &Arc<artifact::ArtifactStore>,
) -> (Arc<gui::SpecValidator>, Arc<gui::UiSpecStore>) {
    let validator = Arc::new(gui::SpecValidator::new(Arc::clone(artifacts), db.clone()));
    let specs = Arc::new(gui::UiSpecStore::new(
        Arc::clone(artifacts),
        Arc::clone(&validator),
    ));
    (validator, specs)
}

/// workflow-engine 対話トリガの UI アクション向けアダプタ（Task 6.5 の③）。
///
/// 検証時にピンした版で起動する。認可は launcher 側（本人 viewer で IR 取得・実行時は
/// scope_ceiling ∩ 本人 ReBAC）に委ねる。IR 取得失敗（不在/権限なし）は存在秘匿で NotFound。
struct LauncherWorkflowStarter(Arc<workflow_engine::WorkflowRunLauncher>);

#[async_trait::async_trait]
impl gui::WorkflowStarter for LauncherWorkflowStarter {
    async fn start_pinned(
        &self,
        ctx: &authz::AuthContext,
        workflow_id: uuid::Uuid,
        version: i64,
        input: &serde_json::Value,
    ) -> Result<Option<uuid::Uuid>, gui::ActionError> {
        self.0
            .start_interactive_version(ctx, workflow_id, version, input)
            .await
            .map_err(|e| match e {
                workflow_engine::run::LauncherError::Ir(_) => gui::ActionError::NotFound,
                other => gui::ActionError::Internal(format!("run 起動: {other}")),
            })
    }
}

/// 宣言的 UI アクションの実行系を配線する（Task 6.5）。
///
/// 利用可能な束縛先（chat.submit ハンドラ・安全ツール・workflow 起動）だけを登録する。
/// 未登録の束縛はディスパッチ時に 503（Unavailable）で fail-closed。
pub(crate) fn wire_ui_actions(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    chat: Option<&Arc<chat::ChatStore>>,
    search: Option<&Arc<rag::SearchService>>,
    workflow_launcher: Option<&Arc<workflow_engine::WorkflowRunLauncher>>,
) -> anyhow::Result<Arc<gui::ActionDispatcher>> {
    let mut dispatcher = gui::ActionDispatcher::new(storage::audit::AuditRecorder::new(db.clone()));
    if let Some(chat) = chat {
        dispatcher.register_handler(Arc::new(chat::ChatSubmitHandler::new((**chat).clone())));
    }
    if let Some(search) = search {
        dispatcher.register_tool(
            agent_core::ToolName::DocSearch,
            Arc::new(agent_core::DocSearchTool::new(Arc::clone(search))),
        );
    }
    if let Some(provider) = wire_websearch(config, http)? {
        dispatcher.register_tool(
            agent_core::ToolName::WebSearch,
            Arc::new(agent_core::WebSearchTool::new(provider)),
        );
    }
    if let Some(launcher) = workflow_launcher {
        dispatcher.set_workflow_starter(Arc::new(LauncherWorkflowStarter(Arc::clone(launcher))));
    }
    Ok(Arc::new(dispatcher))
}

/// ワークフロー実行時（run ワーカー/スケジューラ/イベント relay）を配線する（Stage A W3）。
///
/// `workflow.enabled=false` なら `(None, None)`。enabled なら launcher/runs を組んで
/// worker/scheduler/relay を spawn し、API 用の launcher/runs を返す（AppState に載る）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn wire_workflow(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    authz: &Arc<dyn AuthzClient>,
    workflows: &Arc<workflow_engine::WorkflowStore>,
    storage: &Arc<storage::StorageService>,
    search: Option<&Arc<rag::SearchService>>,
    secrets: Option<&Arc<secrets::SecretStore>>,
) -> anyhow::Result<(
    Option<Arc<workflow_engine::WorkflowRunLauncher>>,
    Option<Arc<workflow_engine::RunStore>>,
)> {
    if !config.workflow.enabled {
        tracing::info!("workflow.enabled=false: ワークフロー実行時は無効（/workflows は保存のみ）");
        return Ok((None, None));
    }
    let runs = workflow_engine::RunStore::new(db.clone());
    let delegation = workflow_engine::DelegationStore::new(db.clone(), Arc::clone(authz));
    let launcher =
        workflow_engine::WorkflowRunLauncher::new(delegation, (**workflows).clone(), runs.clone());
    let gateway = Arc::new(build_gateway(config, http, db)?);
    let sandbox = build_sandbox(config)?;
    api::workflow_runtime::spawn_workflow_runtime(api::workflow_runtime::RuntimeDeps {
        db: db.clone(),
        launcher: launcher.clone(),
        runs: runs.clone(),
        storage: Arc::clone(storage),
        search: search.cloned(),
        gateway,
        sandbox,
        // code_interpreter の隔離ティアは chat と同一の admin ポリシー（単一ソース）。
        sandbox_backend: config.chat.sandbox_backend.unwrap_or_default(),
        secrets: secrets.cloned(),
        http: http.clone(),
        redis_url: Some(config.session.redis_url.clone()),
        config: config.workflow.clone(),
    })
    .await;
    Ok((Some(Arc::new(launcher)), Some(Arc::new(runs))))
}
