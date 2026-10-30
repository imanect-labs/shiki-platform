//! 「ファイルを読む」（storage.read）で PDF・Office の本文を読む結合テスト（D-111・BUG-101 の残り）。
//!
//! 実 Postgres ＋実 OpenFGA ＋ RustFS で、本番の executor（`CapabilityNodeExecutor`＋`ProdNodePorts`）に
//! 本番の `rag::SearchService` を結線して流す。取り込み（ingestion-worker のパース・埋め込み）はこのテストでは
//! 動かさず、取り込みが書く行（`rag_chunk`・`doc_block`・`rag_ingest_job`）を直接積む。読み口が読むのはこの行だけ
//! なので、取り込み済みの文書と同じ経路を通る。
//!
//! - 取り込み済みの PDF の本文（ページの印・見出し）を読む。長い本文は上限の字数で切り、切った印を立てる。
//!   名前を変えても（ファイルのバージョンが進んでも）読める。テキストのファイルはいまの経路のまま読む。
//! - 見えない文書・無い文書・索引の pre-filter で読めない文書は、同じ誤り（存在の秘匿）。
//! - 取り込み前は失敗にせず、試す回数を使わずに待って読み直す（取り込んだ後に読める・D-125）。待つ上限（30 分）を
//!   過ぎたらやり直せる失敗（index_pending）で、再開するともう一度待つ。取り込めない種類・取り込みに失敗した文書・
//!   社内文書の検索が使えない環境は、やり直さない分かる誤り（検索が使えない環境は、この環境で使えない機能の分類・
//!   BUG-158）。
//! - 取り込みの記録が無い文書は、編集できる人が取り込み直し（outbox → 取り込み・D-126）、失敗したところから再開すると
//!   読める。見えるだけの人は取り込み直せない。
//! - script の `Shiki.storage.read` の誤りは、捕まえていなければホストの code のまま、作成者が投げた誤りは
//!   script_error で失敗になる（D-121）。再開しても同じ結果になる失敗は、再開を断る（D-116）。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が無いとスキップする。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::too_many_lines,
    clippy::type_complexity
)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind, Relation};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::nodes::ports::{INDEX_FAILED, INDEX_PENDING, NOT_INDEXABLE, SEARCH_DISABLED};
use workflow_engine::vocab::STORAGE_READ_MAX_DOCUMENT_CHARS;
use workflow_engine::{
    CapabilityNodeExecutor, Catalog, DelegationStore, EffectJournal, GrantRequest, NodeExecutor,
    RegistrationService, RunStatus, RunStore, SchedulerStore, WorkerConfig, WorkflowIr,
    WorkflowRunLauncher, WorkflowStore,
};

struct NoopAudit;
impl workflow_engine::CapabilityAudit for NoopAudit {
    fn record(&self, _t: &str, _a: &str, _ok: bool, _m: &Value) {}
}

struct Env {
    pool: sqlx::PgPool,
    fga: Arc<dyn AuthzClient>,
    storage: Arc<storage::StorageService>,
    artifacts: Arc<artifact::ArtifactStore>,
    workflows: WorkflowStore,
    runs: RunStore,
    launcher: WorkflowRunLauncher,
    /// 自動実行のオン（委譲の付与）。
    registration: RegistrationService,
    /// 社内文書の検索を結線した executor と、結線しない executor（社内文書の検索が使えない環境）。
    executor: Arc<dyn NodeExecutor>,
    no_search: Arc<dyn NodeExecutor>,
    /// 取り込み直し（D-126）と、取り込みの本体（パースはフェイク・索引の行は本物）。
    search: Arc<rag::SearchService>,
    ingest: Arc<rag::PipelineDeps>,
    tenant: String,
    alice: AuthContext,
}

async fn setup() -> Option<Env> {
    let (Ok(db), Ok(fga_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&db)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let http = reqwest::Client::new();
    let fga = Arc::new(
        OpenFgaClient::connect(
            http.clone(),
            &OpenFgaConfig {
                base_url: fga_url,
                store_name: format!("shiki-wf-docread-{}", Uuid::new_v4()),
            },
            &authz::model::default_model(),
        )
        .await
        .expect("fga"),
    ) as Arc<dyn AuthzClient>;
    let s3 = storage::S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key: std::env::var("STORAGE_TEST_S3_ACCESS_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        secret_key: std::env::var("STORAGE_TEST_S3_SECRET_KEY")
            .unwrap_or_else(|_| "minioadmin".into()),
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let object_store: Arc<dyn storage::ObjectStore> = Arc::new(storage::S3ObjectStore::new(&s3));
    object_store.ensure_bucket().await.expect("bucket");
    let ingest = Arc::new(rag::PipelineDeps {
        pool: pool.clone(),
        config: rag::RagConfig::default(),
        parser: Arc::new(InvoiceParser),
        embedder: Arc::new(FixedEmbedder),
        vector: Arc::new(NoVectors),
        fulltext: Arc::new(NoFulltext),
        indexer_storage: Arc::new(storage::IndexerStorage::new(
            pool.clone(),
            Arc::clone(&object_store),
        )),
    });
    let storage = Arc::new(storage::StorageService::new(
        pool.clone(),
        object_store,
        Arc::clone(&fga),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    ));
    let gateway = Arc::new(
        llm_gateway::LlmGateway::build(
            pool.clone(),
            http,
            llm_gateway::GatewayConfig {
                provider: llm_gateway::ProviderConfig {
                    kind: llm_gateway::ProviderKind::Stub,
                    ..Default::default()
                },
                catalog: llm_gateway::ModelCatalog {
                    default_model: "stub".into(),
                    models: vec![llm_gateway::ModelEntry {
                        id: "stub".into(),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )
        .expect("gateway"),
    );
    // 本番の SearchService（本文の読み出しは Postgres の索引の行と OpenFGA の可読集合だけを使う）。
    let search = Arc::new(rag::SearchService::new(
        pool.clone(),
        rag::RagConfig::default(),
        Arc::new(FixedEmbedder),
        Arc::new(ZeroReranker),
        Arc::new(NoVectors),
        Arc::new(NoFulltext),
        Arc::clone(&fga),
        storage::audit::AuditRecorder::new(pool.clone()),
    ));

    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let alice = user_ctx(&tenant, "alice");
    fga.write_tuple(
        &alice.subject(),
        Relation::Member,
        &alice.ns().organization(&alice.org),
    )
    .await
    .expect("member tuple");

    let artifacts = Arc::new(artifact::ArtifactStore::new(pool.clone(), Arc::clone(&fga)));
    let workflows = WorkflowStore::new(Arc::clone(&artifacts));
    let runs = RunStore::new(pool.clone(), workflow_engine::DEFAULT_LEASE_SECS);
    let delegation = DelegationStore::new(pool.clone(), Arc::clone(&fga));
    let launcher = WorkflowRunLauncher::new(delegation.clone(), workflows.clone(), runs.clone());
    let registration = RegistrationService::new(pool.clone(), delegation);
    let executor = |search: Option<Arc<rag::SearchService>>| -> Arc<dyn NodeExecutor> {
        let ports = Arc::new(api::workflow_runtime::ProdNodePorts {
            storage: Arc::clone(&storage),
            search,
            gateway: Arc::clone(&gateway),
            sandbox: None,
            sandbox_backend: sandbox_client::SandboxBackend::Wasm,
            secrets: None,
            tabular: None,
            skill_artifacts: None,
            launcher: launcher.clone(),
            http: reqwest::Client::new(),
            db: pool.clone(),
            data: None,
            fsms: None,
            web_search: None,
        });
        Arc::new(
            CapabilityNodeExecutor::new(
                ports,
                EffectJournal::new(pool.clone()),
                Arc::new(NoopAudit),
            )
            .with_script_engine(
                script_runtime::EngineHandle::shared(),
                script_runtime::engine::Limits::default(),
            ),
        )
    };
    Some(Env {
        executor: executor(Some(Arc::clone(&search))),
        no_search: executor(None),
        search,
        ingest,
        pool,
        fga,
        storage,
        artifacts,
        workflows,
        runs,
        launcher,
        registration,
        tenant,
        alice,
    })
}

fn user_ctx(tenant: &str, id: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: id.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.to_string()),
        },
        tenant.to_string(),
        tenant.to_string(),
    )
}

/// 1 回の実行の結果（step の状態・誤り・結果）。
#[derive(Debug)]
struct Outcome {
    run_id: Uuid,
    status: RunStatus,
    code: Option<String>,
    retryable: Option<bool>,
    message: Option<String>,
    output: Option<Value>,
}

impl Env {
    /// 入力の file を「ファイルを読む」で読むワークフローを保存する。
    async fn save_reader(&self) -> Uuid {
        let ir = json!({
            "ir_version": 1, "name": "read-document",
            "declared_scopes": ["storage.read"],
            "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "rd", "type": "storage.read",
                  "params": { "file": { "$from": "input", "path": "/file" } } }
            ],
            "edges": []
        });
        self.workflows
            .create(&self.alice, &ir, &Catalog::default(), None)
            .await
            .expect("save ir")
            .0
    }

    /// ワーカーで run を最後まで進め、`rd` の結果を読む。
    async fn drive(&self, run_id: Uuid, executor: &Arc<dyn NodeExecutor>) -> Outcome {
        let worker = workflow_engine::WorkflowWorker::new(
            self.runs.clone(),
            Arc::clone(executor),
            WorkerConfig::default(),
        )
        .scoped_to_tenant(&self.tenant);
        while worker.claim_and_run_once("w1").await.unwrap() {}
        let status = self
            .runs
            .run_status(&self.tenant, run_id)
            .await
            .unwrap()
            .unwrap();
        let (code, retryable, message, output): (
            Option<String>,
            Option<bool>,
            Option<String>,
            Option<Value>,
        ) = sqlx::query_as(
            "SELECT error->>'code', (error->>'retryable')::bool, error->>'message', output \
             FROM step_execution WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'rd'",
        )
        .bind(&self.tenant)
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        Outcome {
            run_id,
            status,
            code,
            retryable,
            message,
            output,
        }
    }

    /// alice が `file` を読む実行を始めて、最後まで進める。
    async fn read(&self, wf_id: Uuid, file: Uuid, executor: &Arc<dyn NodeExecutor>) -> Outcome {
        let run_id = self
            .launcher
            .start_interactive(&self.alice, wf_id, &json!({ "file": file.to_string() }))
            .await
            .expect("start")
            .expect("run id");
        self.drive(run_id, executor).await
    }

    /// `rd` の step の（状態・試した回数・待っている理由）。
    async fn step_state(&self, run_id: Uuid) -> (String, i32, Option<String>) {
        sqlx::query_as(
            "SELECT status, attempt, wait_code FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'rd'",
        )
        .bind(&self.tenant)
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 次に読み直すまでの秒数（取り込みを待っている `rd` の step の next_retry_at まで）。
    async fn seconds_to_retry(&self, run_id: Uuid) -> f64 {
        sqlx::query_scalar(
            "SELECT extract(epoch FROM next_retry_at - now())::float8 FROM step_execution \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'rd'",
        )
        .bind(&self.tenant)
        .bind(run_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// 待っている読み直しの時刻を今にする（待つ間隔を実時間で待たない）。`waited` を渡すと、待ち始めた時刻をその秒数
    /// だけ前にする（上限を過ぎた状態を作る）。
    async fn wake(&self, run_id: Uuid, waited: Option<i64>) {
        sqlx::query(
            "UPDATE step_execution SET next_retry_at = now(), \
                 wait_since = coalesce(now() - make_interval(secs => $3::float8), wait_since) \
             WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'rd'",
        )
        .bind(&self.tenant)
        .bind(run_id)
        .bind(waited.map(|w| w as f64))
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// `rd` に script を置いたワークフローを保存する（入力の file を `Shiki.storage.read` で読むなど）。
    async fn save_script(&self, name: &str, scopes: &[&str], source: &str) -> Uuid {
        let ir = json!({
            "ir_version": 1, "name": name,
            "declared_scopes": scopes,
            "triggers": [{ "kind": "interactive" }],
            "nodes": [
                { "id": "rd", "type": "script.run",
                  "params": { "source": { "inline": source }, "input": { "$from": "input" } } }
            ],
            "edges": []
        });
        self.workflows
            .create(&self.alice, &ir, &Catalog::default(), None)
            .await
            .expect("save ir")
            .0
    }

    /// 取り込みが書く行を積む（パース済みのブロック列・検索用のチャンク・取り込みの記録）。
    ///
    /// チャンクの authz_tags は取り込みと同じく、ファイル自身と祖先のフォルダの構造タグ（PIT-1 (b)）。
    async fn ingest(
        &self,
        owner: &AuthContext,
        node: &storage::Node,
        blocks: &[(i32, &str, &str)],
    ) {
        let mut tags = vec![owner.ns().file(&node.id.to_string()).as_str().to_string()];
        if let Some(parent) = node.parent_id {
            tags.push(owner.ns().folder(&parent.to_string()).as_str().to_string());
        }
        sqlx::query(
            "insert into rag_chunk \
                 (id, tenant_id, org, node_id, version, parent_id, kind, ordinal, page, \
                  heading_path, content, char_count, authz_tags, embedding_model_version) \
             values ($1, $2, $3, $4, $5, null, 'leaf', 0, 1, '{}', '検索用', 3, $6, 'fixed')",
        )
        .bind(Uuid::new_v4())
        .bind(&owner.tenant_id)
        .bind(&owner.org)
        .bind(node.id)
        .bind(node.version)
        .bind(&tags)
        .execute(&self.pool)
        .await
        .unwrap();
        for (ordinal, (page, kind, text)) in blocks.iter().enumerate() {
            sqlx::query(
                "insert into doc_block (tenant_id, org, node_id, version, ordinal, type, level, text, page) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(&owner.tenant_id)
            .bind(&owner.org)
            .bind(node.id)
            .bind(node.version)
            .bind(i32::try_from(ordinal).unwrap())
            .bind(*kind)
            .bind((*kind == "heading").then_some(1))
            .bind(*text)
            .bind(*page)
            .execute(&self.pool)
            .await
            .unwrap();
        }
        self.job(node, "succeeded").await;
    }

    /// 取り込みの記録（`rag_ingest_job`）を書く。
    async fn job(&self, node: &storage::Node, status: &str) {
        sqlx::query(
            "insert into rag_ingest_job (tenant_id, org, node_id, version, op, status) \
             values ($1, $2, $3, $4, 'create', $5) \
             on conflict (tenant_id, node_id, version, op) do update set status = excluded.status",
        )
        .bind(&node.tenant_id)
        .bind(&node.org)
        .bind(node.id)
        .bind(node.version)
        .bind(status)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// outbox の `node` のイベントを、relay が取り込みへ渡した後のように処理済みにする（このテストは relay を動かさない）。
    async fn relayed(&self, node: &storage::Node) {
        sqlx::query(
            "update storage_event_outbox set processed_at = now() \
             where tenant_id = $1 and node_id = $2 and processed_at is null",
        )
        .bind(&node.tenant_id)
        .bind(node.id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// PDF（中身は形だけ）を `parent` に置く。
    async fn upload_pdf(
        &self,
        ctx: &AuthContext,
        parent: Option<Uuid>,
        name: &str,
    ) -> storage::Node {
        self.storage
            .write_file_internal(
                ctx,
                parent,
                name,
                b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj",
                "application/pdf",
                None,
            )
            .await
            .expect("upload")
    }

    /// このテストのテナントで作った行を片付ける（run の履歴・ワークフロー・ファイル・索引・取り込みの記録・監査）。
    async fn cleanup(
        &self,
        workflows: &[Uuid],
        files: &[(AuthContext, Uuid)],
        folders: &[(AuthContext, Uuid)],
    ) {
        self.runs.purge_tenant_history(&self.tenant).await.unwrap();
        for id in workflows {
            self.artifacts.delete(&self.alice, *id, None).await.unwrap();
        }
        for (ctx, id) in files {
            self.storage.soft_delete_file(ctx, *id, None).await.unwrap();
        }
        for (ctx, id) in folders {
            self.storage
                .soft_delete_folder(ctx, *id, None)
                .await
                .unwrap();
        }
        for sql in [
            "delete from rag_chunk where tenant_id = $1",
            "delete from doc_block where tenant_id = $1",
            "delete from rag_ingest_job where tenant_id = $1",
            "delete from storage_event_outbox where tenant_id = $1",
            "delete from trigger_firing where tenant_id = $1",
            "delete from workflow_registration where tenant_id = $1",
            "delete from audit_log where tenant_id = $1 and action = 'rag.doc_read'",
        ] {
            sqlx::query(sql)
                .bind(&self.tenant)
                .execute(&self.pool)
                .await
                .unwrap();
        }
    }
}

/// 取り込み済みの PDF の本文を、ページの印と見出しつきで読む。長い本文は上限の字数で切り、切った印（truncated）を
/// 立てる。名前を変えてファイルのバージョンが進んでも、取り込んだ本文を読める。テキストのファイルはいまの経路
/// （文字コードを決めて中身を読む）のまま読み、truncated は false。
#[tokio::test]
async fn storage_read_reads_the_indexed_body_of_a_pdf() {
    let Some(env) = setup().await else { return };
    let alice = env.alice.clone();
    let wf = env.save_reader().await;
    let folder = env
        .storage
        .create_folder(&alice, None, "契約", None)
        .await
        .unwrap();
    let pdf = env.upload_pdf(&alice, Some(folder.id), "契約書.pdf").await;
    env.ingest(
        &alice,
        &pdf,
        &[
            (1, "heading", "第1条 目的"),
            (1, "paragraph", "本契約は、業務の委託について定める。"),
            (2, "heading", "第2条 支払"),
            (2, "paragraph", "委託料は月末締め翌月末払いとする。"),
        ],
    )
    .await;
    let read = env.read(wf, pdf.id, &env.executor).await;

    // 名前を変える（ファイルのバージョンが進む・本文は変わらず取り込み直さない）。
    let renamed = env
        .storage
        .rename_file(&alice, pdf.id, "業務委託契約書.pdf", None)
        .await
        .unwrap();
    let after_rename = env.read(wf, pdf.id, &env.executor).await;

    // 長い本文（上限の字数を超える）は切る。
    let long = env
        .upload_pdf(&alice, Some(folder.id), "長い規程.pdf")
        .await;
    let paragraph = "あ".repeat(999);
    let mut blocks: Vec<(i32, &str, &str)> = vec![(1, "heading", "第1章")];
    let count = usize::try_from(STORAGE_READ_MAX_DOCUMENT_CHARS).unwrap() / 1_000 + 20;
    blocks.extend((0..count).map(|_| (2, "paragraph", paragraph.as_str())));
    env.ingest(&alice, &long, &blocks).await;
    let cut = env.read(wf, long.id, &env.executor).await;

    let txt = env
        .storage
        .write_file_internal(
            &alice,
            Some(folder.id),
            "メモ.txt",
            "経費の申請\n".as_bytes(),
            "text/plain",
            None,
        )
        .await
        .unwrap();
    let text = env.read(wf, txt.id, &env.executor).await;

    env.cleanup(
        &[wf],
        &[
            (alice.clone(), pdf.id),
            (alice.clone(), long.id),
            (alice.clone(), txt.id),
        ],
        &[(alice.clone(), folder.id)],
    )
    .await;

    assert_eq!(read.status, RunStatus::Succeeded, "{read:?}");
    let out = read.output.unwrap();
    assert_eq!(
        out["text"],
        "〔p.1〕\n# 第1条 目的\n\n本契約は、業務の委託について定める。\n\n\
         〔p.2〕\n# 第2条 支払\n\n委託料は月末締め翌月末払いとする。"
    );
    assert_eq!(out["truncated"], false);
    assert_eq!(out["file_id"], pdf.id.to_string());
    assert_eq!(out["id"], pdf.id.to_string());
    assert_eq!(out["name"], "契約書.pdf");
    assert_eq!(out["content_type"], "application/pdf");
    assert_eq!(out["version"], pdf.version);
    assert!(
        out.get("encoding").is_none(),
        "取り込んだ本文に文字コードは無い"
    );

    assert!(
        renamed.version > pdf.version,
        "名前の変更でバージョンが進む"
    );
    assert_eq!(
        after_rename.status,
        RunStatus::Succeeded,
        "{after_rename:?}"
    );
    let out = after_rename.output.unwrap();
    assert_eq!(out["name"], "業務委託契約書.pdf");
    assert_eq!(out["version"], pdf.version, "読んだ本文のバージョン");

    assert_eq!(cut.status, RunStatus::Succeeded, "{:?}", cut.code);
    let out = cut.output.unwrap();
    assert_eq!(out["truncated"], true);
    let chars = out["text"].as_str().unwrap().chars().count();
    let max = usize::try_from(STORAGE_READ_MAX_DOCUMENT_CHARS).unwrap();
    assert!(chars <= max && chars > max - 2_000, "{chars} 字");

    assert_eq!(text.status, RunStatus::Succeeded);
    let out = text.output.unwrap();
    assert_eq!(out["text"], "経費の申請\n");
    assert_eq!(out["encoding"], "UTF-8");
    assert_eq!(out["truncated"], false);
}

/// 見えない文書（他人だけのフォルダーの PDF とテキスト）・無い文書・索引の pre-filter で読めない文書は、どれも同じ
/// 誤り（not_found・同じ文）で失敗する（存在の秘匿・チャットの doc_read と同じ契約）。pre-filter の文書は閲覧の判定を
/// 通っても本文を返さない（二段 authz）。
#[tokio::test]
async fn hidden_and_missing_documents_fail_the_same_way() {
    let Some(env) = setup().await else { return };
    let alice = env.alice.clone();
    let bob = user_ctx(&env.tenant, "bob");
    env.fga
        .write_tuple(
            &bob.subject(),
            Relation::Member,
            &bob.ns().organization(&bob.org),
        )
        .await
        .unwrap();
    let wf = env.save_reader().await;
    let private = env
        .storage
        .create_folder(&bob, None, "人事", None)
        .await
        .unwrap();
    let secret = env.upload_pdf(&bob, Some(private.id), "評価.pdf").await;
    env.ingest(&bob, &secret, &[(1, "paragraph", "社外秘の評価")])
        .await;
    let secret_txt = env
        .storage
        .write_file_internal(&bob, Some(private.id), "給与.txt", b"x", "text/plain", None)
        .await
        .unwrap();
    // alice が見られる PDF だが、索引の本文の authz_tags が alice の可読集合に入っていない（索引と権限の食い違い）。
    let mine = env.upload_pdf(&alice, None, "議事録.pdf").await;
    env.ingest(&bob, &mine, &[(1, "paragraph", "本文")]).await;
    sqlx::query("update rag_chunk set authz_tags = $1 where node_id = $2")
        .bind(vec![alice
            .ns()
            .folder(&Uuid::new_v4().to_string())
            .as_str()
            .to_string()])
        .bind(mine.id)
        .execute(&env.pool)
        .await
        .unwrap();

    let hidden_pdf = env.read(wf, secret.id, &env.executor).await;
    let hidden_txt = env.read(wf, secret_txt.id, &env.executor).await;
    let missing = env.read(wf, Uuid::new_v4(), &env.executor).await;
    let prefiltered = env.read(wf, mine.id, &env.executor).await;
    // 社内文書の検索が使えない環境でも、見えない文書は同じ誤り（種類の誤りで存在を明かさない）。
    let hidden_no_search = env.read(wf, secret.id, &env.no_search).await;

    env.cleanup(
        &[wf],
        &[
            (bob.clone(), secret.id),
            (bob.clone(), secret_txt.id),
            (alice.clone(), mine.id),
        ],
        &[(bob.clone(), private.id)],
    )
    .await;

    let expected = "ファイルが見つかりません。削除されたか、見る権限がありません。";
    for (what, o) in [
        ("見えない PDF", &hidden_pdf),
        ("見えないテキスト", &hidden_txt),
        ("無い文書", &missing),
        ("pre-filter で読めない PDF", &prefiltered),
        ("検索が使えない環境の見えない PDF", &hidden_no_search),
    ] {
        assert_eq!(o.status, RunStatus::Failed, "{what}: {o:?}");
        assert_eq!(
            (o.code.as_deref(), o.retryable, o.message.as_deref()),
            (Some("not_found"), Some(false), Some(expected)),
            "{what}"
        );
        assert!(
            o.output.as_ref().is_none_or(Value::is_null),
            "{what}: 結果に何も残さない {o:?}"
        );
    }
}

/// 取り込み前の PDF は失敗にせず、試す回数を使わずに待って読み直し、取り込んだ後に読める（試す回数は既定の 1 のまま・
/// D-125・GAP-50）。待っている間、実行の詳細の step は取り込みを待っている（`index`）と言う。
/// 取り込めない種類（画像）・取り込みに失敗した PDF・AI エージェントの作業用のフォルダーの PDF・取り込みが届いて
/// いない PDF・社内文書の検索が使えない環境は、やり直さない分かる誤り。
#[tokio::test]
async fn unindexed_documents_fail_with_a_clear_reason() {
    let Some(env) = setup().await else { return };
    let alice = env.alice.clone();
    let wf = env.save_reader().await;
    let pending = env.upload_pdf(&alice, None, "請求書.pdf").await;
    let before = env.read(wf, pending.id, &env.executor).await;
    let waiting = env.step_state(before.run_id).await;
    let first_delay = env.seconds_to_retry(before.run_id).await;
    let waiting_for = env
        .runs
        .run_detail(&env.tenant, wf, before.run_id)
        .await
        .unwrap()
        .unwrap()
        .steps
        .into_iter()
        .find(|s| s.node_id == "rd")
        .and_then(|s| s.waiting_for);
    // 取り込みが終わってから、読み直しの時刻が来る（間隔は実時間で待たない）。
    env.ingest(&alice, &pending, &[(1, "paragraph", "請求金額 12,000 円")])
        .await;
    env.wake(before.run_id, None).await;
    let after = env.drive(before.run_id, &env.executor).await;
    let done = env.step_state(before.run_id).await;

    let image = env
        .storage
        .write_file_internal(
            &alice,
            None,
            "領収書.png",
            b"\x89PNG\r\n\x1a\n",
            "image/png",
            None,
        )
        .await
        .unwrap();
    let unsupported = env.read(wf, image.id, &env.executor).await;
    let broken = env.upload_pdf(&alice, None, "壊れた.pdf").await;
    env.relayed(&broken).await;
    env.job(&broken, "dead").await;
    let failed = env.read(wf, broken.id, &env.executor).await;
    let no_search = env.read(wf, pending.id, &env.no_search).await;
    // AI エージェントの作業用のフォルダー（システム領域）のファイルは、取り込みへ積まれない。
    let scratch = env.upload_pdf(&alice, None, "作業メモ.pdf").await;
    sqlx::query("update node set system = true where id = $1")
        .bind(scratch.id)
        .execute(&env.pool)
        .await
        .unwrap();
    let excluded = env.read(wf, scratch.id, &env.executor).await;
    // 取り込みへ向かう記録がどこにも無い（社内文書の検索を有効にする前に置いたファイルなど）。
    let orphan = env.upload_pdf(&alice, None, "前からある.pdf").await;
    env.relayed(&orphan).await;
    let not_ingested = env.read(wf, orphan.id, &env.executor).await;

    env.cleanup(
        &[wf],
        &[
            (alice.clone(), pending.id),
            (alice.clone(), image.id),
            (alice.clone(), broken.id),
            (alice.clone(), scratch.id),
            (alice.clone(), orphan.id),
        ],
        &[],
    )
    .await;

    assert_eq!(
        before.status,
        RunStatus::Running,
        "取り込みを待つ間は失敗にしない: {before:?}"
    );
    assert_eq!(before.code, None, "{before:?}");
    assert_eq!(
        waiting,
        ("ready".to_string(), 0, Some(INDEX_PENDING.to_string())),
        "試す回数を使わずに、取り込み待ちとして読み直しを待つ"
    );
    assert!(
        (1.0..=6.0).contains(&first_delay),
        "最初は 5 秒ほど待つ: {first_delay}"
    );
    assert_eq!(waiting_for.as_deref(), Some("index"));
    assert_eq!(after.status, RunStatus::Succeeded, "{after:?}");
    assert_eq!(after.output.unwrap()["text"], "〔p.1〕\n請求金額 12,000 円");
    assert_eq!(
        done,
        ("succeeded".to_string(), 1, None),
        "読めた実行も 1 回目のまま"
    );

    for (what, o, code, says) in [
        ("画像", &unsupported, NOT_INDEXABLE, "取り込めないため"),
        (
            "取り込みに失敗",
            &failed,
            INDEX_FAILED,
            "「検索に取り込み直す」を選んでください",
        ),
        (
            "作業用のフォルダー",
            &excluded,
            NOT_INDEXABLE,
            "AI エージェントの作業用のフォルダーにあり",
        ),
        (
            "取り込みが届いていない",
            &not_ingested,
            INDEX_FAILED,
            "「検索に取り込み直す」を選ぶと、取り込まれます",
        ),
        (
            "検索が使えない環境",
            &no_search,
            SEARCH_DISABLED,
            "社内文書の検索が使えない",
        ),
    ] {
        assert_eq!(o.status, RunStatus::Failed, "{what}: {o:?}");
        assert_eq!(
            (o.code.as_deref(), o.retryable),
            (Some(code), Some(false)),
            "{what}"
        );
        assert!(
            o.message.as_deref().unwrap().contains(says),
            "{what}: {o:?}"
        );
    }
}

/// 取り込みの記録が無い PDF（社内文書の検索を有効にする前に置いた）は、実行の詳細の 1 行目でも取り込み直す案内を
/// 言う（D-126）。見えるだけの人は取り込み直せない。編集できる人が取り込み直すと、取り込み直しの依頼が outbox に
/// 積まれ（同じ内容は二重に積まない）、取り込みの本体（`rag_ingest_job`・`reindex`）が本文を索引に入れる。失敗した
/// ところから再開すると、本文を読める。社内文書の検索が使えない環境の失敗は、環境の問題として言う（BUG-158）。
#[tokio::test]
async fn reindexing_an_unrecorded_pdf_makes_it_readable() {
    let Some(env) = setup().await else { return };
    let alice = env.alice.clone();
    let bob = user_ctx(&env.tenant, "bob");
    env.fga
        .write_tuple(
            &bob.subject(),
            Relation::Member,
            &bob.ns().organization(&bob.org),
        )
        .await
        .unwrap();
    let wf = env.save_reader().await;
    let orphan = env.upload_pdf(&alice, None, "前からある請求書.pdf").await;
    env.relayed(&orphan).await;
    let failed = env.read(wf, orphan.id, &env.executor).await;
    let failure = env
        .runs
        .run_detail(&env.tenant, wf, failed.run_id)
        .await
        .unwrap()
        .unwrap()
        .failure;
    let no_search = env.read(wf, orphan.id, &env.no_search).await;
    let no_search_detail = env
        .runs
        .run_detail(&env.tenant, wf, no_search.run_id)
        .await
        .unwrap()
        .unwrap();

    env.storage
        .share_node(
            &alice,
            orphan.id,
            &storage::ShareTarget::User { id: "bob".into() },
            storage::ShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let viewer = env.storage.authorize_reindex(&bob, orphan.id, None).await;
    let node = env
        .storage
        .authorize_reindex(&alice, orphan.id, None)
        .await
        .unwrap();
    let queued = env
        .search
        .request_reindex(&alice, &node, None)
        .await
        .unwrap();
    let again = env
        .search
        .request_reindex(&alice, &node, None)
        .await
        .unwrap();
    // relay が job_queue へ写すイベント（op はそのまま）を、取り込みの本体に渡す。
    let events: Vec<(i64, String, String)> = sqlx::query_as(
        "select version, op, actor from storage_event_outbox \
         where tenant_id = $1 and node_id = $2 and op = 'reindex' and processed_at is null",
    )
    .bind(&env.tenant)
    .bind(orphan.id)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    let mut indexed = Vec::new();
    for (version, op, actor) in &events {
        let message = rag::IngestMessage {
            tenant_id: env.tenant.clone(),
            org: alice.org.clone(),
            node_id: orphan.id,
            version: *version,
            op: op.clone(),
            actor: actor.clone(),
        };
        indexed.push(
            rag::pipeline::indexer::handle(&env.ingest, &message, None)
                .await
                .unwrap()
                .to_string(),
        );
    }
    env.relayed(&orphan).await;
    let state = env.search.index_state(&alice, &node).await.unwrap();
    let resumed = env
        .runs
        .resume_failed(&env.tenant, wf, failed.run_id)
        .await
        .unwrap();
    let after = env.drive(failed.run_id, &env.executor).await;
    let audits: i64 = sqlx::query_scalar(
        "select count(*) from audit_log where tenant_id = $1 and action = 'file.reindex'",
    )
    .bind(&env.tenant)
    .fetch_one(&env.pool)
    .await
    .unwrap();

    env.cleanup(&[wf], &[(alice.clone(), orphan.id)], &[]).await;
    sqlx::query("delete from audit_log where tenant_id = $1 and action = 'file.reindex'")
        .bind(&env.tenant)
        .execute(&env.pool)
        .await
        .unwrap();

    assert_eq!(failed.code.as_deref(), Some(INDEX_FAILED), "{failed:?}");
    let failure = failure.unwrap();
    assert!(
        failure
            .message
            .contains("「検索に取り込み直す」を選んでから"),
        "{failure:?}"
    );
    assert!(!failure.message.contains("設定に問題"), "{failure:?}");
    // 社内文書の検索が使えない環境は、この環境で使えない機能（作成者でなく管理者に連絡する）。
    assert_eq!(
        no_search.code.as_deref(),
        Some(SEARCH_DISABLED),
        "{no_search:?}"
    );
    let f = no_search_detail.failure.unwrap();
    assert_eq!(f.category, workflow_engine::FailureCategory::Environment);
    assert!(
        f.message
            .starts_with("この環境では社内文書の検索が使えないため"),
        "{f:?}"
    );
    assert_eq!(
        no_search_detail.resume.unwrap().advice,
        workflow_engine::ResumeAdvice::FixFirst
    );

    assert!(
        matches!(viewer, Err(storage::StorageError::Forbidden)),
        "{viewer:?}"
    );
    assert_eq!(
        queued,
        rag::ReindexOutcome::Queued {
            version: node.version
        }
    );
    assert_eq!(again, rag::ReindexOutcome::InProgress, "二重に積まない");
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        indexed.len() == 1 && indexed[0].starts_with("indexed("),
        "取り込み直しの依頼で本文を取り込んだ: {indexed:?}"
    );
    assert_eq!(state, rag::IndexState::Ready);
    assert!(
        matches!(resumed, workflow_engine::ResumeOutcome::Resumed),
        "{resumed:?}"
    );
    assert_eq!(after.status, RunStatus::Succeeded, "{after:?}");
    assert_eq!(
        after.output.unwrap()["text"],
        "〔p.1〕\n請求金額 34,000 円",
        "取り込み直した本文を読む"
    );
    assert!(
        audits >= 2,
        "許可（alice）と拒否（bob）を監査に残す: {audits}"
    );
}

/// 自動実行（workflow プリンシパル）でも、委譲したフォルダーの PDF の本文は読め、委譲していないフォルダーの PDF は
/// 見えないと同じ誤り（自動実行で読める範囲を 2 文目で言う）になる。閲覧の判定（StorageService）も索引の可読集合
/// （ListObjects）も、実行の主体（委譲を受けたワークフロー）で引く。
#[tokio::test]
async fn automated_runs_read_only_delegated_folders() {
    let Some(env) = setup().await else { return };
    let alice = env.alice.clone();
    let shared = env
        .storage
        .create_folder(&alice, None, "委譲する", None)
        .await
        .unwrap();
    let private = env
        .storage
        .create_folder(&alice, None, "委譲しない", None)
        .await
        .unwrap();
    let granted = env.upload_pdf(&alice, Some(shared.id), "規程.pdf").await;
    env.ingest(&alice, &granted, &[(1, "paragraph", "就業時間は 9 時から")])
        .await;
    let other = env.upload_pdf(&alice, Some(private.id), "人事.pdf").await;
    env.ingest(&alice, &other, &[(1, "paragraph", "評価の基準")])
        .await;
    let ir = json!({
        "ir_version": 1, "name": "nightly-read",
        "declared_scopes": ["storage.read"],
        "triggers": [ { "kind": "schedule", "cron": "* * * * *", "tz": "UTC" } ],
        "nodes": [
            { "id": "ok", "type": "storage.read", "params": { "file": granted.id.to_string() } },
            { "id": "rd", "type": "storage.read", "params": { "file": other.id.to_string() } }
        ],
        "edges": [ { "from": "ok", "to": "rd" } ]
    });
    let (wf, _) = env
        .workflows
        .create(&alice, &ir, &Catalog::default(), None)
        .await
        .expect("save ir");
    let parsed = WorkflowIr::from_json(&ir).unwrap();
    env.registration
        .enable(
            &alice,
            wf,
            1,
            &parsed,
            &[GrantRequest {
                scope: "storage.read".into(),
                object: alice.ns().folder(&shared.id.to_string()),
                relation: Relation::Viewer,
            }],
        )
        .await
        .expect("enable");
    let fired = SchedulerStore::new(env.pool.clone())
        .tick_schedules(
            chrono::Utc::now() + chrono::Duration::seconds(61),
            Some(&env.tenant),
            &env.launcher,
        )
        .await
        .unwrap();
    let run_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT run_id FROM workflow_run WHERE tenant_id = $1 AND workflow_id = $2 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&env.tenant)
    .bind(wf)
    .fetch_optional(&env.pool)
    .await
    .unwrap();
    let outcome = match run_id {
        Some(run_id) => Some(env.drive(run_id, &env.executor).await),
        None => None,
    };
    let (principal, first): (Option<String>, Option<Value>) = match run_id {
        Some(run_id) => sqlx::query_as(
            "SELECT r.principal_kind, s.output FROM workflow_run r \
             JOIN step_execution s ON s.tenant_id = r.tenant_id AND s.run_id = r.run_id AND s.node_id = 'ok' \
             WHERE r.tenant_id = $1 AND r.run_id = $2",
        )
        .bind(&env.tenant)
        .bind(run_id)
        .fetch_optional(&env.pool)
        .await
        .unwrap()
        .unwrap_or((None, None)),
        None => (None, None),
    };

    env.cleanup(
        &[wf],
        &[(alice.clone(), granted.id), (alice.clone(), other.id)],
        &[(alice.clone(), shared.id), (alice.clone(), private.id)],
    )
    .await;

    assert!(fired >= 1, "スケジュールが発火する（fired={fired}）");
    let o = outcome.expect("自動実行の run が作られる");
    assert_eq!(principal.as_deref(), Some("workflow"), "自動実行の主体");
    assert_eq!(
        first.as_ref().and_then(|v| v["text"].as_str()),
        Some("〔p.1〕\n就業時間は 9 時から"),
        "委譲したフォルダーの PDF は読める: {first:?}"
    );
    assert_eq!(o.status, RunStatus::Failed, "{o:?}");
    assert_eq!(
        (o.code.as_deref(), o.retryable),
        (Some("not_found"), Some(false)),
        "{o:?}"
    );
    assert_eq!(
        o.message.as_deref(),
        Some(
            "ファイルが見つかりません。削除されたか、見る権限がありません。\
             自動実行では、オンにするときに使えるようにしたファイルとフォルダーの中だけを読めます。"
        ),
        "{o:?}"
    );
}

/// 取り込みを待つ上限（30 分）を過ぎたら、待った時間を言うやり直せる失敗（index_pending）になる。失敗したところから
/// 再開すると、待ち始め直してもう一度待ち、取り込んだ後に読める（D-125）。再開の API はこの失敗を断らない（D-116）。
#[tokio::test]
async fn index_wait_gives_up_after_the_limit_and_resume_waits_again() {
    let Some(env) = setup().await else { return };
    let alice = env.alice.clone();
    let wf = env.save_reader().await;
    let pdf = env.upload_pdf(&alice, None, "見積書.pdf").await;
    let first = env.read(wf, pdf.id, &env.executor).await;
    // 待ち始めてから上限を過ぎた（時計を進める代わりに、待ち始めた時刻を戻す）。
    env.wake(
        first.run_id,
        Some(workflow_engine::INDEX_WAIT.max_wait_secs + 1),
    )
    .await;
    let gave_up = env.drive(first.run_id, &env.executor).await;
    let gave_up_state = env.step_state(first.run_id).await;
    let detail = env
        .runs
        .run_detail(&env.tenant, wf, first.run_id)
        .await
        .unwrap()
        .unwrap();
    let resumed = env
        .runs
        .resume_failed(&env.tenant, wf, first.run_id)
        .await
        .unwrap();
    let rewaiting = env.drive(first.run_id, &env.executor).await;
    let rewaiting_state = env.step_state(first.run_id).await;
    env.ingest(&alice, &pdf, &[(1, "paragraph", "見積金額 30,000 円")])
        .await;
    env.wake(first.run_id, None).await;
    let done = env.drive(first.run_id, &env.executor).await;

    env.cleanup(&[wf], &[(alice.clone(), pdf.id)], &[]).await;

    assert_eq!(first.status, RunStatus::Running, "{first:?}");
    assert_eq!(gave_up.status, RunStatus::Failed, "{gave_up:?}");
    assert_eq!(
        (gave_up.code.as_deref(), gave_up.retryable),
        (Some(INDEX_PENDING), Some(true))
    );
    let message = gave_up.message.as_deref().unwrap();
    assert!(message.contains("30 分待っても"), "{message}");
    assert!(!message.contains("試す回数"), "{message}");
    assert_eq!(gave_up_state, ("failed".to_string(), 1, None));
    let failure = detail.failure.expect("失敗の理由");
    assert_eq!(
        failure.category,
        workflow_engine::FailureCategory::Temporary
    );
    assert!(failure.message.contains("30 分待っても"), "{failure:?}");
    let resume = detail.resume.expect("再開の勧め");
    assert_eq!(resume.advice, workflow_engine::ResumeAdvice::Resume);
    assert!(resume.offer);
    assert!(
        matches!(resumed, workflow_engine::ResumeOutcome::Resumed),
        "{resumed:?}"
    );
    assert_eq!(
        rewaiting.status,
        RunStatus::Running,
        "再開すると待ち直す: {rewaiting:?}"
    );
    assert_eq!(rewaiting_state.0, "ready");
    assert_eq!(rewaiting_state.2.as_deref(), Some(INDEX_PENDING));
    assert_eq!(done.status, RunStatus::Succeeded, "{done:?}");
    assert_eq!(done.output.unwrap()["text"], "〔p.1〕\n見積金額 30,000 円");
}

/// script の `Shiki.storage.read` の誤りは、捕まえていなければホストの code のまま失敗になる（使ってよい操作の天井の
/// 外は out_of_scope・無いファイルは not_found・BUG-136）。作成者が投げた誤りは script_error（BUG-137）。再開しても
/// 同じ結果になる失敗（out_of_scope・script_error）は、再開を断って run を失敗のまま残す（D-116）。原因を直せば
/// 通る失敗（not_found）は再開できる。
#[tokio::test]
async fn script_failures_keep_the_host_code() {
    let Some(env) = setup().await else { return };
    let read = "function main(input) { return Shiki.storage.read(input.file); }";
    let no_scope = env.save_script("script-no-scope", &[], read).await;
    let missing = env
        .save_script("script-missing", &["storage.read"], read)
        .await;
    let thrown = env
        .save_script(
            "script-throw",
            &["storage.read"],
            "function main(input) { if (!input.amount) { throw new Error('金額がありません'); } return 1; }",
        )
        .await;
    let file = Uuid::new_v4();
    let mut outcomes = Vec::new();
    for wf in [no_scope, missing, thrown] {
        let o = env.read(wf, file, &env.executor).await;
        let detail = env
            .runs
            .run_detail(&env.tenant, wf, o.run_id)
            .await
            .unwrap()
            .unwrap();
        let resumed = env
            .runs
            .resume_failed(&env.tenant, wf, o.run_id)
            .await
            .unwrap();
        let status = env
            .runs
            .run_status(&env.tenant, o.run_id)
            .await
            .unwrap()
            .unwrap();
        outcomes.push((o, detail, resumed, status));
    }

    env.cleanup(&[no_scope, missing, thrown], &[], &[]).await;

    let [(scope_o, scope_d, scope_r, scope_s), (missing_o, missing_d, missing_r, _), (throw_o, throw_d, throw_r, throw_s)] =
        <[_; 3]>::try_from(outcomes).unwrap();
    // 使ってよい操作の天井の外: 権限の分類で、この実行のバージョンの設定で決まる（再開も同じ入力の実行も勧めない）。
    assert_eq!(scope_o.status, RunStatus::Failed, "{scope_o:?}");
    assert_eq!(
        (scope_o.code.as_deref(), scope_o.retryable),
        (Some("out_of_scope"), Some(false)),
        "{scope_o:?}"
    );
    assert_eq!(
        scope_d.failure.unwrap().category,
        workflow_engine::FailureCategory::Permission
    );
    let hint = scope_d.resume.unwrap();
    assert_eq!(hint.advice, workflow_engine::ResumeAdvice::SameResult);
    assert!(!hint.offer && !hint.offer_rerun, "{hint:?}");
    assert_eq!(
        scope_r,
        workflow_engine::ResumeOutcome::NotResumable(hint),
        "再開しても同じ結果になる失敗は断る"
    );
    assert_eq!(scope_s, RunStatus::Failed, "断った run は失敗のまま");
    // 無いファイル: ポートの誤りのまま（原因を直せば通るので再開できる）。
    assert_eq!(
        (missing_o.code.as_deref(), missing_o.retryable),
        (Some("not_found"), Some(false)),
        "{missing_o:?}"
    );
    assert_eq!(
        missing_d.resume.unwrap().advice,
        workflow_engine::ResumeAdvice::FixFirst
    );
    assert!(
        matches!(missing_r, workflow_engine::ResumeOutcome::Resumed),
        "{missing_r:?}"
    );
    // 作成者が投げた誤り: 設定の誤りで、再開しても同じ結果になる（同じ入力でもう一度実行は勧める）。
    assert_eq!(
        (throw_o.code.as_deref(), throw_o.retryable),
        (Some(script_runtime::SCRIPT_ERROR), Some(false)),
        "{throw_o:?}"
    );
    assert!(
        throw_o
            .message
            .as_deref()
            .unwrap()
            .contains("金額がありません"),
        "{throw_o:?}"
    );
    let failure = throw_d.failure.unwrap();
    assert_eq!(
        failure.category,
        workflow_engine::FailureCategory::Configuration
    );
    assert!(failure.message.contains("作成者に連絡"), "{failure:?}");
    let hint = throw_d.resume.unwrap();
    assert_eq!(hint.advice, workflow_engine::ResumeAdvice::SameResult);
    assert!(!hint.offer && hint.offer_rerun, "{hint:?}");
    assert!(
        matches!(throw_r, workflow_engine::ResumeOutcome::NotResumable(_)),
        "{throw_r:?}"
    );
    assert_eq!(throw_s, RunStatus::Failed);
}

// ------- 社内文書の検索の依存（本文の読み出しでは使わない）-------------------------------

/// 取り込み直しのテストで、PDF の本文を 1 段落で返すパーサ（原本は読まない）。
struct InvoiceParser;

#[async_trait]
impl rag::DocumentParser for InvoiceParser {
    async fn parse(
        &self,
        _ctx: &AuthContext,
        _req: rag::ParseRequest<'_>,
    ) -> Result<rag::types::ParsedDocument, rag::RagError> {
        Ok(rag::types::ParsedDocument {
            blocks: vec![rag::types::ParsedBlock {
                block_type: rag::types::BlockType::Paragraph,
                level: None,
                text: "請求金額 34,000 円".into(),
                page: Some(1),
                prov: Vec::new(),
                list_marker: None,
            }],
            used_ocr: false,
        })
    }
}

struct FixedEmbedder;

#[async_trait]
impl rag::embedding::EmbeddingProvider for FixedEmbedder {
    async fn embed(
        &self,
        _ctx: &AuthContext,
        _input: rag::embedding::EmbedInput,
        texts: &[String],
    ) -> Result<rag::embedding::EmbedResponse, rag::RagError> {
        Ok(rag::embedding::EmbedResponse {
            vectors: texts.iter().map(|_| vec![1.0, 0.0]).collect(),
            model_version: "fixed".into(),
            dimension: 2,
        })
    }

    fn model_version(&self) -> &str {
        "fixed"
    }
}

struct ZeroReranker;

#[async_trait]
impl rag::rerank::Reranker for ZeroReranker {
    async fn rerank(
        &self,
        _ctx: &AuthContext,
        _query: &str,
        passages: &[rag::rerank::RerankPassage],
    ) -> Result<Vec<rag::rerank::RerankScore>, rag::RagError> {
        Ok(passages
            .iter()
            .map(|p| rag::rerank::RerankScore {
                id: p.id.clone(),
                score: 0.0,
            })
            .collect())
    }
}

struct NoVectors;

#[async_trait]
impl rag::vector_store::VectorStore for NoVectors {
    async fn ensure_ready(&self, _dimension: usize) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn upsert(
        &self,
        _ctx: &AuthContext,
        _points: &[rag::vector_store::ChunkPoint],
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn delete_node(&self, _ctx: &AuthContext, _node_id: Uuid) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn delete_stale_versions(
        &self,
        _ctx: &AuthContext,
        _node_id: Uuid,
        _keep_version: i64,
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn set_authz_tags(
        &self,
        _ctx: &AuthContext,
        _node_id: Uuid,
        _tags: &[String],
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    async fn search(
        &self,
        _ctx: &AuthContext,
        _query: &rag::vector_store::VectorSearch<'_>,
    ) -> Result<Vec<rag::vector_store::ScoredChunk>, rag::RagError> {
        Ok(vec![])
    }
    async fn purge_tenant(&self, _tenant_id: &str) -> Result<(), rag::RagError> {
        Ok(())
    }
}

struct NoFulltext;

impl rag::fulltext::FulltextIndex for NoFulltext {
    fn replace_node(
        &self,
        _ctx: &AuthContext,
        _node_id: Uuid,
        _docs: &[rag::fulltext::FulltextDoc<'_>],
    ) -> Result<(), rag::RagError> {
        Ok(())
    }
    fn delete_node(&self, _ctx: &AuthContext, _node_id: Uuid) -> Result<(), rag::RagError> {
        Ok(())
    }
    fn search(
        &self,
        _ctx: &AuthContext,
        _query_text: &str,
        _limit: usize,
        _prefilter: &rag::vector_store::PreFilter,
        _scope_tags: &[String],
        _exclude: &[Uuid],
    ) -> Result<Vec<rag::vector_store::ScoredChunk>, rag::RagError> {
        Ok(vec![])
    }
    fn purge_tenant(&self, _tenant_id: &str) -> Result<(), rag::RagError> {
        Ok(())
    }
}
