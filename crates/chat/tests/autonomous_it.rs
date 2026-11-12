//! Phase 5 DoD e2e（Task 5.1/5.4/5.8/5.11）: autonomous run が worker で Autonomous プロファイルとして
//! 動き、`fs_write` が Durable Workspace（StorageService）へ書き込み、書込イベントを発行することを検証する。
//!
//! **専用テストバイナリに分離**する: worker を spawn する他テストと同一バイナリだと、共有 jobq を
//! storage 未配線の worker が claim して自律 run が chat にフォールバックし得るため（バイナリ間は逐次実行）。
//! 実 Postgres＋RustFS（S3 互換）が必要（`STORAGE_TEST_DATABASE_URL`＋`STORAGE_TEST_S3_ENDPOINT`）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    // 3 経路（既定/new_under/existing）を単一 worker に集約する e2e のため長い（jobq 相乗り回避）。
    clippy::too_many_lines
)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::{ChatStore, ChatWorker, StreamEventKind, WorkerConfig};
use futures::stream::StreamExt;
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{object_store::S3Config, ObjectStore, S3ObjectStore, StorageService};

/// 全許可のモック authz（DB/ストレージロジックの検証に集中する）。
struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _: &Subject,
        _: Relation,
        _: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn read_tuples(
        &self,
        _: &FgaObject,
        _: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _: &Subject,
        _: Relation,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _: &Subject,
        _: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
}

async fn setup() -> Option<PgPool> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    Some(pool)
}

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

fn stub_gateway(pool: PgPool) -> LlmGateway {
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            base_url: None,
            api_key: None,
            user_agent: None,
            session_header: None,
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                real_id: None,
                prompt_price_micros_per_mtok: 0,
                completion_price_micros_per_mtok: 0,
                context_window_tokens: None,
                ..Default::default()
            }],
        },
        langfuse: None,
        ..Default::default()
    };
    LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// StorageService（AllowAll authz＋RustFS）＋自律 worker を組む共通ハーネス。
async fn workspace_harness(pool: PgPool) -> (ChatStore, Arc<StorageService>) {
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let s3 = S3Config {
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
    let object_store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    object_store.ensure_bucket().await.expect("バケット準備");
    let storage = Arc::new(StorageService::new(
        pool.clone(),
        object_store,
        Arc::new(AllowAll),
        Duration::from_mins(5),
        Duration::from_mins(15),
        5 * 1024 * 1024 * 1024,
    ));
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let gateway = stub_gateway(pool.clone());
    let worker = ChatWorker::new(
        pool.clone(),
        store.clone(),
        chat::WorkerDeps {
            gateway,
            search: None,
            sandbox: None,
            artifacts: None,
            web_search: None,
            parser: None,
            storage: Some(storage.clone()),
            workflow_resolver: None,
            skill_artifacts: None,
            skill_catalog: None,
            workflow_store: None,
            workflow_catalog: None,
            collab: None,
            tabular: None,
            office: None,
            office_live: None,
            office_creator: None,
            office_engine: None,
        },
        WorkerConfig {
            system_prompt: "あなたは自律アシスタントです。".into(),
            model: Some("m".into()),
            // Coverage（cargo-llvm-cov）は計装＋全テスト並列で 1 step が大きく遅くなる。
            // 30s では worker のリース失効で run が orphan 化して flake るため余裕を持たせる。
            lease_secs: 120,
            max_steps: 4,
            ..Default::default()
        },
    );
    worker.spawn(1);
    (store, storage)
}

/// 自律 run を投入し、fs_write の tool_call と Done を待つ（stub `fswrite:` プレフィックス）。
///
/// 既定モードは承認必須（#350）のため、投入前に**本人が**オートへ切り替える（fs_write を
/// 自動承認し、承認カード待ちでテストが止まらないようにする）。
async fn run_autonomous_fswrite(store: &ChatStore, c: &AuthContext, thread_id: uuid::Uuid) {
    store
        .set_autonomous_mode(c, thread_id, chat::AutonomousMode::Auto, None)
        .await
        .unwrap();
    let res = store
        .post_message(
            c,
            thread_id,
            "fswrite: hello-e2e",
            &[],
            None,
            None,
            true,
            &[],
            None,
        )
        .await
        .unwrap();
    let mut rx = store.event_stream(&c.tenant_id, res.run_id, 0);
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(20), rx.next())
            .await
            .expect("イベント待ちタイムアウト");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Done { .. } => return,
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    panic!("autonomous run が完了しない");
}

/// 自律 run の Durable Workspace e2e（Task 5.1/5.4/5.8/5.11）＋ワークスペース場所選択（Phase 6 UX）。
///
/// ⚠️ このテストバイナリの worker spawn は **1 テストに集約**する（共有 jobq を別テストの
/// worker が奪って run が宙に浮くフレークを避ける・memory: local-test-infra）。既定作成／
/// new_under／existing の 3 経路を単一 worker で順に検証する。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn autonomous_run_writes_workspace_file_e2e() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let (store, storage) = workspace_harness(pool.clone()).await;
    let c = ctx(&tenant);

    // ── ① 既定（場所未指定）: Drive 直下にシステム領域が lazy 作成され、fs_write がそこへ書く ──
    let thread = store
        .create_thread(&c, "自律", false, None, None)
        .await
        .unwrap();
    // **モードは既定（承認必須）のまま**投入する（#392）。自動生成のワークスペースは
    // ドライブに見えず RAG にも載らない使い捨て領域なので、書込は事前許可され承認カードで
    // 止まらない。ここで Auto へ切り替えないことがこのテストの主眼。
    // autonomous=true で投入 → stub が fs_write を呼ぶ（fswrite: プレフィックス）。
    let res = store
        .post_message(
            &c,
            thread.id,
            "fswrite: hello-e2e",
            &[],
            None,
            None,
            true,
            &[],
            None,
        )
        .await
        .unwrap();
    let mut rx = store.event_stream(&c.tenant_id, res.run_id, 0);
    let mut done = false;
    let mut saw_fs_write = false;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(20), rx.next())
            .await
            .expect("イベント待ちタイムアウト");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::ToolCall { name, .. } if name == "fs_write" => saw_fs_write = true,
            StreamEventKind::Done { .. } => {
                done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    assert!(done, "autonomous run が完了する");
    assert!(
        saw_fs_write,
        "fs_write ツール呼び出しが流れる（Autonomous プロファイル＋フルツール）"
    );

    let folder = store
        .workspace_folder_id(thread.id, &c.tenant_id)
        .await
        .unwrap()
        .expect("ワークスペースフォルダが作成される");
    let (default_parent, folder_system): (Option<uuid::Uuid>, bool) =
        sqlx::query_as("SELECT parent_id, system FROM node WHERE id = $1")
            .bind(folder)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(default_parent, None, "未指定は Drive 直下（親なし）");
    assert!(
        folder_system,
        "自動生成のワークスペースはシステム領域（ドライブ非表示・RAG 非索引・#392）"
    );
    // 承認モードは run 前後で変わらない（skill も自動処理もポリシに触れない・#392）。
    let (mode_after, _) = store
        .thread_autonomous_mode(thread.id, &c.tenant_id)
        .await
        .unwrap();
    assert_eq!(
        mode_after,
        chat::AutonomousMode::RequireApproval,
        "承認モードを勝手に緩めない（事前許可はシステム領域の書込に限る）"
    );
    let node = storage
        .resolve_child_file(&c, folder, "agent-note.txt", None)
        .await
        .unwrap()
        .expect("agent-note.txt が書き込まれる");
    let (_, bytes) = storage.read_file_internal(&c, node, None).await.unwrap();
    assert_eq!(bytes, b"hello-e2e", "書込内容がワークスペースに永続する");
    // 書込イベント（→自動再索引）が発行されている（Task 5.8）。
    let outbox: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM storage_event_outbox WHERE node_id = $1 AND op = 'create'",
    )
    .bind(node)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(outbox, 1, "書込イベントが再索引経路へ発行される");

    // ── ② new_under: 選んだ親フォルダの配下にワークスペースを作る ──
    let parent = storage
        .create_folder(&c, None, "選んだ親", None)
        .await
        .unwrap();
    let t1 = store
        .create_thread(&c, "新規作成", false, None, None)
        .await
        .unwrap();
    store
        .set_thread_workspace(t1.id, &c.tenant_id, None, Some(parent.id))
        .await
        .unwrap();
    run_autonomous_fswrite(&store, &c, t1.id).await;
    let ws1 = store
        .workspace_folder_id(t1.id, &c.tenant_id)
        .await
        .unwrap()
        .expect("ワークスペースが作られる");
    let ws1_parent: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT parent_id FROM node WHERE id = $1")
            .bind(ws1)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        ws1_parent,
        Some(parent.id),
        "new_under は選んだ親フォルダの配下にワークスペースを作る"
    );
    let ws1_system: bool = sqlx::query_scalar("SELECT system FROM node WHERE id = $1")
        .bind(ws1)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !ws1_system,
        "場所を明示選択した run のワークスペースは可視・索引のまま（#392）"
    );

    // ── ③ existing: 既存フォルダをそのままワークスペースにする（新規作成しない） ──
    let existing = storage
        .create_folder(&c, None, "既存を使う", None)
        .await
        .unwrap();
    let t2 = store
        .create_thread(&c, "既存", false, None, None)
        .await
        .unwrap();
    store
        .set_thread_workspace(t2.id, &c.tenant_id, Some(existing.id), None)
        .await
        .unwrap();
    run_autonomous_fswrite(&store, &c, t2.id).await;
    let ws2 = store
        .workspace_folder_id(t2.id, &c.tenant_id)
        .await
        .unwrap()
        .expect("ワークスペースが確定する");
    assert_eq!(ws2, existing.id, "existing は選んだフォルダをそのまま使う");
    storage
        .resolve_child_file(&c, existing.id, "agent-note.txt", None)
        .await
        .unwrap()
        .expect("選んだフォルダ直下に書き込まれる");

    // ── ④ 遅延生成: fs_* を使わない自律 run では Drive にフォルダを作らない（#392）──
    let t3 = store
        .create_thread(&c, "fs を使わない", false, None, None)
        .await
        .unwrap();
    let res3 = store
        .post_message(
            &c,
            t3.id,
            "plan: 調査だけ",
            &[],
            None,
            None,
            true,
            &[],
            None,
        )
        .await
        .unwrap();
    let mut rx3 = store.event_stream(&c.tenant_id, res3.run_id, 0);
    let mut t3_done = false;
    for _ in 0..500 {
        let next = tokio::time::timeout(Duration::from_secs(20), rx3.next())
            .await
            .expect("イベント待ちタイムアウト");
        let Some(ev) = next else { break };
        match ev.event {
            StreamEventKind::Done { .. } => {
                t3_done = true;
                break;
            }
            StreamEventKind::Error { message } => panic!("生成失敗: {message}"),
            _ => {}
        }
    }
    // **完了を確かめてから**フォルダ未作成を見る。未完了のまま assert すると
    // 「まだ作っていないだけ」を「作らない」と誤判定する（偽陽性・レビュー指摘）。
    assert!(t3_done, "fs を使わない自律 run も完了する");
    assert!(
        store
            .workspace_folder_id(t3.id, &c.tenant_id)
            .await
            .unwrap()
            .is_none(),
        "fs ツールを使わない run では空のワークスペースフォルダを作らない（遅延生成）"
    );

    // ── ⑤ ソフト削除済みフォルダの選択は拒否される（FGA タプルは残るため DB 存在も確かめる）──
    let doomed = storage
        .create_folder(&c, None, "消したフォルダ", None)
        .await
        .unwrap();
    sqlx::query("UPDATE node SET deleted_at = now() WHERE id = $1")
        .bind(doomed.id)
        .execute(&pool)
        .await
        .unwrap();
    let rejected = storage
        .require_folder_access(&c, doomed.id, authz::Relation::Editor, None)
        .await;
    assert!(
        matches!(rejected, Err(storage::StorageError::NotFound)),
        "削除済みフォルダの選択は NotFound: {rejected:?}"
    );
}

/// コピーは元の版を変えずに新規ファイルを作り、テナント境界と衝突を保つ。
#[tokio::test]
async fn drive_copy_preserves_source_and_tenant_boundary() {
    use agent_core::Tool;
    let Some(pool) = setup().await else { return };
    let tenant = format!("copy-{}", uuid::Uuid::new_v4());
    let ctx = ctx(&tenant);
    let (_, storage) = workspace_harness(pool.clone()).await;
    let source = storage
        .write_file_internal(&ctx, None, "source.csv", b"a,b\n1,2\n", "text/csv", None)
        .await
        .unwrap();
    let folder = storage
        .create_folder(&ctx, None, "copies", None)
        .await
        .unwrap();
    let run_id = uuid::Uuid::new_v4();
    let tool = chat::drive_copy_tool::DriveCopyTool::new(storage.clone(), run_id);
    assert!(tool.requires_confirmation());
    let input = serde_json::json!({"node_id":source.id,"new_name":"copy.csv","destination_folder_id":folder.id});
    let copied = tool.call(&ctx, input.clone(), None).await.unwrap();
    assert!(!copied.is_error, "{}", copied.content);
    assert_eq!(copied.artifacts.len(), 1);
    let id = uuid::Uuid::parse_str(&copied.artifacts[0].node_id).unwrap();
    assert_ne!(id, source.id);
    let (node, bytes) = storage.read_file_internal(&ctx, id, None).await.unwrap();
    assert_eq!(node.parent_id, Some(folder.id));
    assert_eq!(node.version, 1);
    assert_eq!(bytes, b"a,b\n1,2\n");
    let (unchanged, bytes) = storage
        .read_file_internal(&ctx, source.id, None)
        .await
        .unwrap();
    assert_eq!(unchanged.version, source.version);
    assert_eq!(bytes, b"a,b\n1,2\n");
    let resumed = chat::drive_copy_tool::DriveCopyTool::new(storage.clone(), run_id);
    let replay = resumed.call(&ctx, input.clone(), None).await.unwrap();
    assert!(!replay.is_error);
    assert_eq!(replay.artifacts[0].node_id, id.to_string());
    let different_run =
        chat::drive_copy_tool::DriveCopyTool::new(storage.clone(), uuid::Uuid::new_v4());
    assert!(
        different_run
            .call(&ctx, input.clone(), None)
            .await
            .unwrap()
            .is_error
    );
    let foreign = super_context(&format!("other-{tenant}"));
    let hidden = tool.call(&foreign, input, None).await.unwrap();
    assert!(hidden.is_error);
    assert!(hidden.content.contains("アクセスできません"));
    assert!(tool
        .call(
            &ctx,
            serde_json::json!({"node_id":source.id,"new_name":"bad.png"}),
            None
        )
        .await
        .is_err());
    assert!(tool
        .call(
            &ctx,
            serde_json::json!({"node_id":source.id,"new_name":""}),
            None
        )
        .await
        .is_err());
    assert!(tool.call(&ctx, serde_json::json!({"node_id":source.id,"new_name":"copy.csv","destination_folder_id":"bad"}), None).await.is_err());
    let dotted = storage
        .write_file_internal(
            &ctx,
            None,
            ".docx",
            b"data",
            "application/octet-stream",
            None,
        )
        .await
        .unwrap();
    assert!(tool
        .call(
            &ctx,
            serde_json::json!({"node_id":dotted.id,"new_name":".xlsx"}),
            None
        )
        .await
        .is_err());
    let dotted_copy = tool
        .call(
            &ctx,
            serde_json::json!({"node_id":dotted.id,"new_name":"dotted.DOCX"}),
            None,
        )
        .await
        .unwrap();
    assert!(!dotted_copy.is_error);
    let root = tool
        .call(
            &ctx,
            serde_json::json!({"node_id":id,"new_name":"root.csv","destination_folder_id":"root"}),
            None,
        )
        .await
        .unwrap();
    assert!(!root.is_error);
    let same_folder = tool
        .call(
            &ctx,
            serde_json::json!({"node_id":id,"new_name":"sibling.csv"}),
            None,
        )
        .await
        .unwrap();
    assert!(!same_folder.is_error);
    let sibling_id = uuid::Uuid::parse_str(&same_folder.artifacts[0].node_id).unwrap();
    assert_eq!(
        storage
            .get_metadata(&ctx, sibling_id, None)
            .await
            .unwrap()
            .parent_id,
        Some(folder.id)
    );
}

// テスト内のctx変数とコンテキスト生成関数を区別する。
fn super_context(tenant: &str) -> AuthContext {
    ctx(tenant)
}

/// 確定済みコピーは、元の更新・移動・削除後にも同じ結果へ収束する。
#[tokio::test]
async fn drive_copy_replays_after_source_changes() {
    use agent_core::Tool;
    let Some(pool) = setup().await else { return };
    let tenant = format!("copy-replay-{}", uuid::Uuid::new_v4());
    let ctx = ctx(&tenant);
    let (_, storage) = workspace_harness(pool).await;
    let source = storage
        .write_file_internal(&ctx, None, "original.txt", b"original", "text/plain", None)
        .await
        .unwrap();
    let folder = storage
        .create_folder(&ctx, None, "moved", None)
        .await
        .unwrap();
    let run_id = uuid::Uuid::new_v4();
    let tool = chat::drive_copy_tool::DriveCopyTool::new(storage.clone(), run_id);
    let input = serde_json::json!({"node_id":source.id,"new_name":"copied.txt"});
    let first = tool.call(&ctx, input.clone(), None).await.unwrap();
    assert!(!first.is_error);
    let copied_id = uuid::Uuid::parse_str(&first.artifacts[0].node_id).unwrap();
    storage
        .update_file_content_internal(&ctx, source.id, b"changed", "text/plain", None)
        .await
        .unwrap();
    storage
        .update_file(&ctx, source.id, None, Some(Some(folder.id)), None)
        .await
        .unwrap();
    let resumed = chat::drive_copy_tool::DriveCopyTool::new(storage.clone(), run_id);
    let replay = resumed.call(&ctx, input.clone(), None).await.unwrap();
    assert!(!replay.is_error, "{}", replay.content);
    assert_eq!(replay.artifacts[0].node_id, copied_id.to_string());
    let (copy, bytes) = storage
        .read_file_internal(&ctx, copied_id, None)
        .await
        .unwrap();
    assert_eq!(copy.parent_id, None);
    assert_eq!(bytes, b"original");
    storage
        .soft_delete_file(&ctx, source.id, None)
        .await
        .unwrap();
    assert!(
        !resumed
            .call(&ctx, input.clone(), None)
            .await
            .unwrap()
            .is_error
    );
    storage
        .soft_delete_file(&ctx, copied_id, None)
        .await
        .unwrap();
    assert!(resumed.call(&ctx, input, None).await.unwrap().is_error);
}

/// 未保存の共有Yjsを複製し、古いファイルblobを成功として返さない。
#[tokio::test]
async fn drive_copy_uses_open_note_and_slide_state() {
    use agent_core::Tool;
    let Some(pool) = setup().await else { return };
    let tenant = format!("copy-live-{}", uuid::Uuid::new_v4());
    let ctx = ctx(&tenant);
    let (_, storage) = workspace_harness(pool.clone()).await;
    let hub = Arc::new(collab::CollabHub::new(
        pool,
        Arc::new(AllowAll),
        storage.clone(),
    ));
    let tool = chat::drive_copy_tool::DriveCopyTool::new(storage.clone(), uuid::Uuid::new_v4())
        .with_collab(Some(hub.clone()));
    let empty_slide = collab::slide::SlideDoc::default().to_json();
    for (name, copy_name, initial, marker) in [
        ("note.md", "note_copy.md", "# Old\n", "LatestNoteMarker"),
        (
            "slides.slide",
            "slides_copy.slide",
            empty_slide.as_str(),
            "LatestSlideMarker",
        ),
    ] {
        let node = storage
            .write_file_internal(
                &ctx,
                None,
                name,
                initial.as_bytes(),
                "application/octet-stream",
                None,
            )
            .await
            .unwrap();
        let open = hub.join(&ctx, &node).await.unwrap();
        if collab::note::is_note_file(name) {
            open.import_markdown(&format!("# New\n\n{marker}\n"))
                .unwrap();
        } else {
            let (_, report) = open
                .apply_ai_slide_edit(&[collab::slide::SlideEditOp::AppendSlide {
                    html: format!("<h1>{marker}</h1>"),
                    notes: None,
                }])
                .unwrap();
            assert_eq!(report.applied, 1);
        }
        let copied = tool
            .call(
                &ctx,
                serde_json::json!({"node_id":node.id,"new_name":copy_name}),
                None,
            )
            .await
            .unwrap();
        assert!(!copied.is_error, "{}", copied.content);
        let id = uuid::Uuid::parse_str(&copied.artifacts[0].node_id).unwrap();
        let (_, bytes) = storage.read_file_internal(&ctx, id, None).await.unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(marker), "{name}: {text}");
        let (_, original) = storage
            .read_file_internal(&ctx, node.id, None)
            .await
            .unwrap();
        assert_eq!(original, initial.as_bytes());
        hub.leave(&open).await;
    }
}

/// 巨大なノートは共同編集ドキュメントをロードする前に拒否する。
#[tokio::test]
async fn drive_copy_rejects_large_note_before_collab_join() {
    use agent_core::Tool;
    let Some(pool) = setup().await else { return };
    let tenant = format!("copy-size-{}", uuid::Uuid::new_v4());
    let ctx = ctx(&tenant);
    let (_, storage) = workspace_harness(pool.clone()).await;
    let node = storage
        .write_file_internal(
            &ctx,
            None,
            "large.md",
            b"# Small fixture",
            "text/markdown",
            None,
        )
        .await
        .unwrap();
    // 巨大blobを確保せず、サイズ宣言が上限超のケースを再現する。
    sqlx::query("UPDATE node SET size_bytes = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(&tenant)
        .bind(node.id)
        .bind(51_i64 * 1024 * 1024)
        .execute(&pool)
        .await
        .unwrap();
    let hub = Arc::new(collab::CollabHub::new(
        pool.clone(),
        Arc::new(AllowAll),
        storage.clone(),
    ));
    let tool = chat::drive_copy_tool::DriveCopyTool::new(storage, uuid::Uuid::new_v4())
        .with_collab(Some(hub));
    let result = tool
        .call(
            &ctx,
            serde_json::json!({"node_id":node.id,"new_name":"copy.md"}),
            None,
        )
        .await
        .unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("サイズ上限"));
    let loaded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM collab_doc WHERE tenant_id = $1 AND node_id = $2)",
    )
    .bind(&tenant)
    .bind(node.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!loaded, "拒否前に共同編集状態をロードしない");
}
