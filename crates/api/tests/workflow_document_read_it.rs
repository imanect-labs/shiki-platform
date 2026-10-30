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
//! - 取り込み前はやり直せる失敗（index_pending）で、取り込んだ後に再開すると読める。取り込めない種類・取り込みに
//!   失敗した文書・社内文書の検索が使えない環境は、やり直さない分かる誤り。
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
use workflow_engine::nodes::ports::{INDEX_FAILED, INDEX_PENDING, NOT_INDEXABLE, NOT_TEXT_FILE};
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
        Arc::new(CapabilityNodeExecutor::new(
            ports,
            EffectJournal::new(pool.clone()),
            Arc::new(NoopAudit),
        ))
    };
    Some(Env {
        executor: executor(Some(search)),
        no_search: executor(None),
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

/// 取り込み前の PDF はやり直せる失敗（index_pending）で、取り込んだ後に失敗したところから再開すると読める。
/// 取り込めない種類（画像）・取り込みに失敗した PDF・AI エージェントの作業用のフォルダーの PDF・取り込みが届いて
/// いない PDF・社内文書の検索が使えない環境は、やり直さない分かる誤り。
#[tokio::test]
async fn unindexed_documents_fail_with_a_clear_reason() {
    let Some(env) = setup().await else { return };
    let alice = env.alice.clone();
    let wf = env.save_reader().await;
    let pending = env.upload_pdf(&alice, None, "請求書.pdf").await;
    let before = env.read(wf, pending.id, &env.executor).await;
    // 取り込みが終わってから、失敗したところから再開する（再開は同じファイルを読み直す）。
    env.ingest(&alice, &pending, &[(1, "paragraph", "請求金額 12,000 円")])
        .await;
    let resumed = env
        .runs
        .resume_failed(&env.tenant, wf, before.run_id)
        .await
        .unwrap();
    let after = env.drive(before.run_id, &env.executor).await;

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

    assert_eq!(before.status, RunStatus::Failed, "{before:?}");
    assert_eq!(
        (before.code.as_deref(), before.retryable),
        (Some(INDEX_PENDING), Some(true))
    );
    assert!(
        before
            .message
            .as_deref()
            .unwrap()
            .contains("取り込みが終わっていないため"),
        "{before:?}"
    );
    assert!(
        matches!(resumed, workflow_engine::ResumeOutcome::Resumed),
        "{resumed:?}"
    );
    assert_eq!(after.status, RunStatus::Succeeded, "{after:?}");
    assert_eq!(after.output.unwrap()["text"], "〔p.1〕\n請求金額 12,000 円");

    for (what, o, code, says) in [
        ("画像", &unsupported, NOT_INDEXABLE, "取り込めないため"),
        ("取り込みに失敗", &failed, INDEX_FAILED, "保存し直して"),
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
            "「新しいバージョン」でアップロードし直すと",
        ),
        (
            "検索が使えない環境",
            &no_search,
            NOT_TEXT_FILE,
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

// ------- 社内文書の検索の依存（本文の読み出しでは使わない）-------------------------------

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
