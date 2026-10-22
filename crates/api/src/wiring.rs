//! 依存配線（RAG・チャット）。main の起動フローから切り出す（1 ファイル行数規約）。

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use authz::AuthzClient;
use storage::{IndexerStorage, ObjectStore};

use api::config::{AppConfig, LlmBackend, VectorStoreBackend};

/// RAG 配線の成果物: 検索サービス（オプション）・テナント消去（常設）・文書パーサ。
///
/// パーサはインジェストだけでなく `web_fetch` の PDF/Office 経路でも使う（#405）。
/// **同一インスタンスを共有する**（worker のエンドポイント・タイムアウト設定を二重定義しない）。
pub(crate) type RagWiring = (
    Option<Arc<rag::SearchService>>,
    Arc<rag::RagAdmin>,
    Option<Arc<dyn rag::DocumentParser>>,
);

/// オブジェクトストア＋StorageService を構築する（main の起動フローから切り出し）。
///
/// GCS は Phase 8 で同 trait 裏に追加する。s3 設定の必須チェックは s3 の分岐内で行う
/// （gcs 選択時に s3 未設定エラーで誤って落ちないようにする）。
pub(crate) async fn wire_storage(
    config: &AppConfig,
    db: &sqlx::PgPool,
    authz: &Arc<dyn AuthzClient>,
) -> anyhow::Result<(Arc<dyn ObjectStore>, Arc<storage::StorageService>)> {
    use api::config::ObjectStoreBackend;
    let (object_store, presign_get_ttl, presign_put_ttl): (Arc<dyn ObjectStore>, _, _) =
        match config.storage.backend {
            ObjectStoreBackend::S3 => {
                let s3 = config
                    .storage
                    .s3
                    .as_ref()
                    .context("storage.s3 が未設定です（backend=s3）")?;
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
    // 一般アクセス有効期限の失効タイマ（#338・イベント駆動・定期ポーリング無し）。
    storage::expiry_timer::spawn_share_link_expiry_timer(Arc::clone(&service));
    // outbox GC（#413）。`storage_event_outbox` は書込チョークポイントの真下にあり、GC が回らないと
    // 永久成長して claim_undelivered の anti-join が累積イベント数に線形劣化する（＝イベント経路の
    // 性能前提）。全レプリカで同時に走っても安全（SKIP LOCKED で刻む）ためリーダー選出はしない。
    // 配送を待つべきコンシューマ集合は outbox_consumer 表から読むので、workflow/gateway の
    // フィーチャフラグの状態に依らず「他コンシューマ宛の未配送イベントを消す」事故が起きない。
    storage::outbox_gc::spawn_outbox_gc(db.clone());
    // 中断アップロードの回収（#468）。クライアント切断で finalize が中断されると
    // pending_upload 行と staging/incoming オブジェクトが残り、回収経路が無いと
    // テナントごとに単調増加する。TTL sweep と孤児 sweep を対で回す。
    let gc = config.storage.upload_gc;
    if gc.enabled {
        storage::upload_gc_timer::spawn_upload_gc_timer(
            Arc::clone(&service),
            storage::upload_gc_timer::UploadGcOptions {
                ttl: std::time::Duration::from_secs(gc.ttl_secs),
                interval: std::time::Duration::from_secs(gc.interval_secs),
            },
        );
    } else {
        tracing::warn!(
            "upload_gc.enabled=false: 中断アップロードの staging/incoming が回収されません（#468）"
        );
    }
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
        return Ok((
            None,
            Arc::new(rag::RagAdmin::new(db.clone(), None, None)),
            None,
        ));
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
    let reranker: Arc<dyn rag::Reranker> = Arc::new(
        rag::HttpReranker::new(rag_http.clone(), &rag_cfg.worker_base_url)
            .expect_model(&rag_cfg.relevance_model),
    );
    let vector: Arc<dyn rag::VectorStore> = Arc::new(rag::QdrantVectorStore::new(
        rag_http,
        &rag_cfg.qdrant_url,
        &rag_cfg.embedding_model_version,
    ));
    // 全テナント分の writer・索引を常駐させない（#816）。アイドル解放は周期タスクで回す。
    let tantivy = Arc::new(rag::TantivyFulltext::with_limits(
        &rag_cfg.index_data_dir,
        rag_cfg.tantivy_limits(),
    ));
    tantivy.spawn_idle_reaper();
    let fulltext: Arc<dyn rag::FulltextIndex> = tantivy;
    let indexer_storage = Arc::new(IndexerStorage::new(db.clone(), Arc::clone(object_store)));
    // relay+consumer（advisory lock でリーダー選出。多重起動しても安全）。
    rag::spawn_pipeline(rag::PipelineDeps {
        pool: db.clone(),
        config: rag_cfg.clone(),
        parser: Arc::clone(&parser),
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
        Some(parser),
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
        AutoConfig, Effort, GatewayConfig, LangfuseConfig, LlmGateway, ProviderConfig,
    };
    let llm = &config.llm;
    let primary = primary_provider(llm)?;
    let extra_providers = llm
        .providers
        .iter()
        .map(|p| {
            Ok(ProviderConfig {
                id: p.id.clone(),
                display_name: non_empty(p.display_name.as_deref()),
                kind: provider_kind(p.backend)?,
                base_url: non_empty(p.base_url.as_deref()),
                api_key: p.api_key.clone().filter(|k| !k.is_empty()),
                user_agent: non_empty(p.user_agent.as_deref()),
                session_header: non_empty(p.session_header.as_deref()),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let auto = match &llm.auto {
        None => AutoConfig::default(),
        Some(a) => AutoConfig {
            model: a.model.clone(),
            effort: match a.effort.as_deref() {
                None => AutoConfig::default().effort,
                Some(e) => Effort::parse(e).ok_or_else(|| {
                    anyhow::anyhow!("llm.auto.effort={e} は low/medium/high のいずれか")
                })?,
            },
        },
    };
    let gateway_config = GatewayConfig {
        provider: primary,
        extra_providers,
        catalog: llm.model_catalog(),
        auto,
        langfuse: llm.langfuse.as_ref().map(|l| LangfuseConfig {
            base_url: l.base_url.clone(),
            public_key: l.public_key.clone(),
            secret_key: l.secret_key.clone(),
        }),
        timeouts: llm.timeouts(),
    };
    LlmGateway::build(db.clone(), http.clone(), gateway_config)
        .map_err(|e| anyhow::anyhow!("llm-gateway 構築に失敗: {e}"))
}

/// 設定の backend をプロバイダ種別へ写す（未実装の枠は起動を止める）。
fn provider_kind(backend: LlmBackend) -> anyhow::Result<llm_gateway::ProviderKind> {
    use llm_gateway::ProviderKind;
    Ok(match backend {
        LlmBackend::Vllm | LlmBackend::Openai => ProviderKind::Openai,
        LlmBackend::Anthropic => ProviderKind::Anthropic,
        LlmBackend::OpenaiResponses => ProviderKind::OpenaiResponses,
        LlmBackend::Stub => ProviderKind::Stub,
        other => anyhow::bail!(
            "llm.backend={other:?} は未実装です（openai-compat/openai_responses/anthropic/stub のみ）"
        ),
    })
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

#[allow(clippy::too_many_arguments)] // 依存束の注入点（main からの一回きり・構造化は wire_gui 側で担保）。
pub(crate) async fn wire_chat(
    config: &AppConfig,
    http: &reqwest::Client,
    db: &sqlx::PgPool,
    authz: &Arc<dyn AuthzClient>,
    search: Option<&Arc<rag::SearchService>>,
    // parser: 文書パーサ（web_fetch の PDF/Office 経路・#405）。RAG 配線と同一インスタンス。
    parser: Option<&Arc<dyn rag::DocumentParser>>,
    storage: &Arc<storage::StorageService>,
    workflow_resolver: &Arc<gui::WorkflowResolver>,
    skill_artifacts: &Arc<artifact::ArtifactStore>,
    workflows: &Arc<workflow_engine::WorkflowStore>,
    secrets: Option<&Arc<secrets::SecretStore>>,
    collab: &Arc<collab::CollabHub>,
    tabular: &Arc<tabular::TabularService>,
    office_runtime: Option<&api::state::OfficeRuntime>,
    skill_installs: &Arc<app_platform::SkillInstallService>,
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

    let system_prompt = config
        .chat
        .system_prompt
        .clone()
        .unwrap_or_else(|| chat::WorkerConfig::default().system_prompt);
    let worker_config = chat::WorkerConfig {
        // 図（```ui ブロック）を書かせるなら書き方を足す（#530）。
        system_prompt: if config.chat.yui {
            chat::with_yui_guide(&system_prompt)
        } else {
            system_prompt
        },
        model: config.llm.default_model.clone(),
        lease_secs: config.chat.lease_secs,
        max_steps: config.chat.max_steps,
        max_tokens: config
            .chat
            .max_tokens
            .unwrap_or_else(|| chat::WorkerConfig::default().max_tokens),
        chat_max_tokens: config
            .chat
            .budget_max_tokens
            .unwrap_or_else(|| chat::WorkerConfig::default().chat_max_tokens),
        chat_max_cost_usd_micros: config
            .chat
            .budget_max_cost_usd_micros
            .unwrap_or_else(|| chat::WorkerConfig::default().chat_max_cost_usd_micros),
        parallel_read_tools: config
            .chat
            .parallel_read_tools
            .unwrap_or_else(|| chat::WorkerConfig::default().parallel_read_tools)
            .max(1),
        tool_search: config
            .chat
            .tool_search
            .unwrap_or_else(|| chat::WorkerConfig::default().tool_search),
        classic_rag: config.chat.classic_rag,
        timezone: match &config.chat.timezone {
            Some(name) => chat::parse_timezone(name)
                .map_err(|e| anyhow::anyhow!("SHIKI__CHAT__TIMEZONE: {e}"))?,
            None => chat::WorkerConfig::default().timezone,
        },
        // コード実行系の隔離ティア（admin ポリシー）。未指定は既定（gVisor・#346）。
        sandbox_backend: config
            .chat
            .sandbox_backend
            .unwrap_or_else(|| chat::WorkerConfig::default().sandbox_backend),
        // 自律プロファイルの既定（予算/ステップ/software）は WorkerConfig::default を踏襲する。
        ..chat::WorkerConfig::default()
    };
    // サンドボックス（code_interpreter / shell）: エンドポイント設定時のみ配線する。
    // 成果物保存（StorageService 裏・発話ユーザー権限）もサンドボックスとセットで配線する。
    let sandbox: Option<Arc<dyn agent_core::Sandbox>> = build_sandbox(config)?;
    let artifacts: Option<Arc<dyn agent_core::ArtifactStore>> = sandbox
        .as_ref()
        .map(|_| Arc::new(chat::StorageArtifactStore::new(Arc::clone(storage))) as _);
    let web_search = crate::wiring_websearch::wire_websearch(config, http)?;
    let worker = chat::ChatWorker::new(
        db.clone(),
        store.clone(),
        chat::WorkerDeps {
            gateway,
            search: search.cloned(),
            sandbox,
            artifacts,
            web_search,
            // web_fetch の PDF/Office 経路（#405）。RAG 無効なら None＝従来どおり拒否。
            parser: parser.cloned(),
            // 自律プロファイルのワークスペース（file CRUD/shell・Task 5.4）。
            storage: Some(Arc::clone(storage)),
            // 図のボタンの `-> workflow 名前` を回答の確定時に版へ固定する（#604）。
            workflow_resolver: Some(Arc::clone(workflow_resolver)),
            // skill / ミニアプリのピン解決（Task 6.9・fail-closed）。
            skill_artifacts: Some(Arc::clone(skill_artifacts)),
            // skill ツールのカタログ源（インストール済み ∪ 本人 owner・#344）。
            skill_catalog: Some(Arc::new(api::skill_catalog::ApiSkillCatalogSource::new(
                Arc::clone(skill_installs),
                Arc::clone(skill_artifacts),
            ))),
            // AI ワークフロー編集（emit_workflow / read_workflow・Task 10.13）。
            // カタログ源は保存 API（build_catalog）と同一実装を注入する（検証乖離の禁止）。
            workflow_store: Some(Arc::clone(workflows)),
            workflow_catalog: Some(Arc::new(
                api::workflow_catalog::ApiWorkflowCatalogSource::new(
                    secrets.cloned(),
                    Some(Arc::clone(skill_installs)),
                    config.llm.models.iter().map(|m| m.id.clone()).collect(),
                ),
            )),
            // AI ノート共同編集（document.edit / document.read・Task 11P.4）。
            collab: Some(Arc::clone(collab)),
            // CSV ツール（csv.query / csv.patch / csv.write・Task 11P.9）。
            tabular: Some(Arc::clone(tabular)),
            // AI Office 編集（office.edit・Task 11.8）: office 有効時のみ配線する。
            // 編集 worker は RAG と同じ ingestion-worker（/edit）を再利用する。
            office: config.office.enabled.then(|| {
                Arc::new(office::OfficeEditor::new(
                    http.clone(),
                    &config.rag.worker_base_url,
                    Arc::clone(storage),
                    Arc::clone(authz),
                    db.clone(),
                ))
            }),
            // AI ライブ編集（office.live_edit・CoolWSD headless 参加・issue #352）。
            // wire_office が構築した LiveEditor（WOPI と同一トークン鍵）を共有する。
            office_live: office_runtime.map(|o| Arc::clone(&o.live)),
            // Office の新規作成（save_document / save_sheet・#381）: 空テンプレを
            // StorageService へ実体化してから同じ LiveEditor で本文を paste する。
            office_creator: office_runtime.map(|o| {
                Arc::new(office::OfficeCreator::new(
                    Arc::clone(storage),
                    Arc::clone(&o.live),
                ))
            }),
            // Excel・Word のレイアウト編集エンジン（office.read / office.excel_edit / office.word_edit・#725）。
            office_engine: office_runtime.map(|o| Arc::clone(&o.engine)),
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

/// ワークフロー実行時（run ワーカー/スケジューラ/イベント relay）を配線する（Stage A W3）。
///
/// `workflow.enabled=false` なら `(None, None)`。enabled なら launcher/runs を組んで
/// worker/scheduler/relay を spawn し、API 用の launcher/runs を返す（AppState に載る）。
///
/// **実行履歴 GC ワーカーだけは `enabled` に依らず起動する**（#448・engine.md §12.2）。保持は
/// コンプライアンス側の義務で、新規 run を受け付けるかとは独立した責務のため。
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
    tabular: &Arc<tabular::TabularService>,
    artifacts: &Arc<artifact::ArtifactStore>,
) -> anyhow::Result<(
    Option<Arc<workflow_engine::WorkflowRunLauncher>>,
    Option<Arc<workflow_engine::RunStore>>,
)> {
    // 実行履歴 GC は **`workflow.enabled` の外**で起動する（#448）。保持期間はプライバシー/
    // コンプライアンス側の義務で、「新規 run を受け付けるか」とは独立した責務。ランタイムの中に
    // 置くと、過去にワークフローを使っていたデプロイが無効化した瞬間に保持義務まで止まり、
    // 既存履歴が期限を過ぎても永久に残る。日次投入もワーカーが持つのでリーダー選出も要らない。
    workflow_engine::HistoryGcWorker::new(db.clone()).spawn();
    tracing::info!("実行履歴 GC ワーカーを起動しました");

    if !config.workflow.enabled {
        tracing::info!("workflow.enabled=false: ワークフロー実行時は無効（/workflows は保存のみ）");
        return Ok((None, None));
    }
    // resume の猶予をワーカーのリースから導出させる（固定値だと lease_secs を上げた構成で
    // 猶予がリースより短くなる・#446）。ワーカーと同じ値を 1 箇所で渡す。
    let runs = workflow_engine::RunStore::new(db.clone(), config.workflow.lease_secs);
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
        tabular: Some(Arc::clone(tabular)),
        // skill.invoke の実行時解決（実行主体 ReBAC・fail-closed・#344）。
        skill_artifacts: Some(Arc::clone(artifacts)),
        http: http.clone(),
        redis_url: Some(config.session.redis_url.clone()),
        config: config.workflow.clone(),
    })
    .await;
    Ok((Some(Arc::new(launcher)), Some(Arc::new(runs))))
}

/// 既定プロバイダ（`llm.backend` 等の単一設定）の接続設定。
fn primary_provider(llm: &api::config::LlmConfig) -> anyhow::Result<llm_gateway::ProviderConfig> {
    Ok(llm_gateway::ProviderConfig {
        display_name: non_empty(llm.display_name.as_deref()),
        kind: provider_kind(llm.backend)?,
        base_url: non_empty(llm.base_url.as_deref()),
        api_key: llm.api_key.clone(),
        user_agent: non_empty(llm.user_agent.as_deref()),
        session_header: non_empty(llm.session_header.as_deref()),
        ..Default::default()
    })
}

/// compose は未設定の変数を空文字で渡すため、空（空白だけを含む）は未指定として扱い、前後の空白を落とす。
fn non_empty(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{non_empty, primary_provider};
    use api::config::{LlmBackend, LlmConfig};

    #[test]
    fn primary_provider_carries_display_name() {
        // 既定プロバイダにも表示名を付けられる（#583）。空は未指定（ID の default を出す）。
        let mut llm = LlmConfig {
            backend: LlmBackend::Openai,
            base_url: Some("https://openrouter.ai/api/v1".into()),
            display_name: Some(" OpenRouter ".into()),
            ..Default::default()
        };
        let p = primary_provider(&llm).unwrap();
        assert_eq!(p.label(), "OpenRouter");
        assert_eq!(p.effective_id(), "default");
        llm.display_name = Some(String::new());
        assert_eq!(primary_provider(&llm).unwrap().label(), "default");
    }

    #[test]
    fn empty_or_blank_settings_are_unset_and_values_are_trimmed() {
        assert_eq!(non_empty(None), None);
        assert_eq!(non_empty(Some("")), None);
        assert_eq!(non_empty(Some("  ")), None);
        assert_eq!(
            non_empty(Some(" x-opencode-session\r")).as_deref(),
            Some("x-opencode-session")
        );
    }
}
