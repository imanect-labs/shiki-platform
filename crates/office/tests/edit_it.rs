//! AI Office 編集の結合テスト（Task 11.8・実 Postgres が必要）。
//!
//! worker `/edit` はモック HTTP サーバ（実 TCP・reqwest 経路そのまま）で代替し、
//! 保存分岐（非ロック=新バージョン／WOPI ロック中=提案）と提案の採用・認可を検証する。
//!
//! 検証（受け入れ条件）:
//! - 非ロック時: 編集が通常の新バージョンになり、書込イベント outbox が流れる
//! - ロック中: current を進めず提案バージョンとして保存され、outbox が**流れない**
//! - 提案の存在下でも通常版の採番が衝突しない（NEXT_CONTENT_VERSION）
//! - 採用: 提案が通常の新バージョンへ昇格し、このとき初めて outbox が流れる
//! - editor でない実行主体は編集も採用もできない（存在秘匿）
//! - 適用 0 件は保存しない／提案は restore_version の対象にならない

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::sync::Arc;
use std::time::Duration;

use authz::{AuthContext, AuthzClient, Relation};
use axum::{routing::post, Json, Router};
use base64::Engine as _;
use office::{EditOutcome, OfficeEditor, OfficeError, SavedEdit};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{Node, StorageService};
use uuid::Uuid;

mod common;
use common::{ctx_for, MemStore, RoleAuthz};

const DOCX_TYPE: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// worker `/edit` のモック。
///
/// - ops[0].find == "missing" → 適用 0 件（バイトはそのまま返す）
/// - それ以外 → バイト末尾に `:EDITED` を付けて適用 1 件を返す
async fn mock_edit(Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let engine = base64::engine::general_purpose::STANDARD;
    let data = engine.decode(req["data_base64"].as_str().unwrap()).unwrap();
    let miss = req["ops"][0]["find"].as_str() == Some("missing");
    let (out, applied) = if miss {
        (data, 0)
    } else {
        ([data.as_slice(), b":EDITED"].concat(), 1)
    };
    Json(serde_json::json!({
        "data_base64": engine.encode(out),
        "report": {
            "applied_ops": applied,
            "results": [{
                "op": "replace_text",
                "applied": applied,
                "warning": if miss { Some("一致なし") } else { None },
            }],
        },
    }))
}

struct Env {
    storage: Arc<StorageService>,
    authz: Arc<RoleAuthz>,
    pool: PgPool,
    editor: OfficeEditor,
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let pool: PgPool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let authz = Arc::new(RoleAuthz::new());
    let authz_dyn: Arc<dyn AuthzClient> = authz.clone();
    let storage = Arc::new(StorageService::new(
        pool.clone(),
        Arc::new(MemStore::default()),
        authz_dyn.clone(),
        Duration::from_secs(120),
        Duration::from_secs(900),
        64 * 1024 * 1024,
    ));

    // モック worker を実 TCP で起動（reqwest の実経路を通す）。
    let app = Router::new().route("/edit", post(mock_edit));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let editor = OfficeEditor::new(
        reqwest::Client::new(),
        &format!("http://{addr}"),
        storage.clone(),
        authz_dyn,
        pool.clone(),
    );
    Some(Env {
        storage,
        authz,
        pool,
        editor,
    })
}

async fn create_docx(env: &Env, owner: &AuthContext) -> Node {
    env.authz.grant(&owner.subject(), Relation::Member);
    env.authz.grant(&owner.subject(), Relation::Editor);
    env.authz.grant(&owner.subject(), Relation::Viewer);
    let name = format!("doc-{}.docx", Uuid::new_v4());
    let bytes = format!("base:nonce:{}", Uuid::new_v4());
    env.storage
        .write_file_internal(owner, None, &name, bytes.as_bytes(), DOCX_TYPE, None)
        .await
        .expect("create docx")
}

/// WOPI ロック（編集セッション）を直接挿入する（Collabora の LOCK 相当）。
async fn hold_lock(env: &Env, ctx: &AuthContext, file_id: Uuid) {
    sqlx::query(
        "INSERT INTO office_lock (file_id, lock_id, locked_by, tenant_id, expires_at) \
         VALUES ($1, 'L-test', $2, $3, now() + interval '30 minutes')",
    )
    .bind(file_id)
    .bind(ctx.subject().as_str())
    .bind(&ctx.tenant_id)
    .execute(&env.pool)
    .await
    .expect("hold lock");
}

async fn edit(env: &Env, ctx: &AuthContext, file_id: Uuid) -> Result<EditOutcome, OfficeError> {
    env.editor
        .edit_file(
            ctx,
            file_id,
            &[serde_json::json!({ "op": "replace_text", "find": "旧", "replace": "新" })],
            None,
        )
        .await
}

/// 対象ノードの書込イベント outbox 件数。
async fn outbox_count(env: &Env, node_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM storage_event_outbox WHERE node_id = $1")
        .bind(node_id)
        .fetch_one(&env.pool)
        .await
        .expect("outbox count")
}

/// 受け入れ条件: 非ロック時の AI 編集は通常の新バージョン＋outbox（RAG 再索引）。
#[tokio::test]
async fn unlocked_edit_creates_new_version_and_emits_event() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice).await;
    let events_before = outbox_count(&env, node.id).await;

    let outcome = edit(&env, &alice, node.id).await.expect("edit");
    assert!(matches!(
        outcome.saved,
        Some(SavedEdit::NewVersion { version: 2 })
    ));
    assert_eq!(outcome.report.applied_ops, 1);

    let (meta, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("read back");
    assert_eq!(meta.version, 2);
    assert!(String::from_utf8_lossy(&bytes).ends_with(":EDITED"));
    assert_eq!(outbox_count(&env, node.id).await, events_before + 1);
}

/// 受け入れ条件: ロック中の AI 編集は提案バージョン（current 不変・outbox 無し・PIT-44）。
/// 提案の存在下でも人間の保存（通常版）が採番衝突せず、提案の採用で outbox が流れる。
#[tokio::test]
async fn locked_edit_becomes_proposal_then_adopt_promotes() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice).await;
    hold_lock(&env, &alice, node.id).await;
    let events_before = outbox_count(&env, node.id).await;

    // ロック中の編集 → 提案 v2（current は v1 のまま・内容不変・outbox 無し）。
    let outcome = edit(&env, &alice, node.id).await.expect("edit");
    let Some(SavedEdit::Proposal { version: proposal }) = outcome.saved else {
        panic!("提案として保存されること: {:?}", outcome.saved);
    };
    assert_eq!(proposal, 2);
    let (meta, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("read back");
    assert_eq!(meta.version, 1, "current が進まないこと");
    assert!(
        !String::from_utf8_lossy(&bytes).contains(":EDITED"),
        "現内容が上書きされないこと"
    );
    assert_eq!(
        outbox_count(&env, node.id).await,
        events_before,
        "提案は書込イベントを発火しないこと"
    );

    // 一覧に提案として現れる（is_proposal / proposed_by）。
    let (versions, _) = env
        .storage
        .list_versions(&alice, node.id, None, 10, None)
        .await
        .expect("list versions");
    let prop = versions.iter().find(|v| v.version == proposal).unwrap();
    assert!(prop.is_proposal);
    assert_eq!(prop.proposed_by.as_deref(), Some("alice"));

    // 提案の存在下でも人間の保存は衝突しない（v2 を跳んで v3 になる）。
    let human = format!("human:nonce:{}", Uuid::new_v4());
    let updated = env
        .storage
        .update_file_content_internal(&alice, node.id, human.as_bytes(), DOCX_TYPE, None)
        .await
        .expect("human save");
    assert_eq!(updated.version, 3, "提案 v2 を跳んで採番されること");

    // 提案は復元経路の対象外（採用のみ）。
    let err = env
        .storage
        .restore_version(&alice, node.id, proposal, None)
        .await
        .expect_err("提案は restore 不可");
    assert!(matches!(err, storage::StorageError::NotFound));

    // 採用 → 通常の新バージョン v4（提案の内容）＋このとき初めて outbox。
    let events_before_adopt = outbox_count(&env, node.id).await;
    let adopted = env
        .storage
        .adopt_proposal_version(&alice, node.id, proposal, None)
        .await
        .expect("adopt");
    assert_eq!(adopted.version, 4);
    let (meta, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("read back");
    assert_eq!(meta.version, 4);
    assert!(String::from_utf8_lossy(&bytes).ends_with(":EDITED"));
    assert_eq!(outbox_count(&env, node.id).await, events_before_adopt + 1);
}

/// 受け入れ条件: editor でない実行主体は編集も採用もできない（存在秘匿の NotFound）。
#[tokio::test]
async fn non_editor_cannot_edit_or_adopt() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice).await;
    hold_lock(&env, &alice, node.id).await;
    let _ = edit(&env, &alice, node.id).await.expect("propose");

    // viewer のみの bob: 編集は存在秘匿の NotFound（読めるかも明かさない）。
    let bob = ctx_for("bob", "default");
    env.authz.grant(&bob.subject(), Relation::Viewer);
    let err = edit(&env, &bob, node.id)
        .await
        .expect_err("viewer は編集不可");
    assert!(matches!(err, OfficeError::NotFound));

    // 採用も editor 限定（viewer は Forbidden→呼び出し側で秘匿）。
    let err = env
        .storage
        .adopt_proposal_version(&bob, node.id, 2, None)
        .await
        .expect_err("viewer は採用不可");
    assert!(matches!(err, storage::StorageError::Forbidden));
}

/// 適用 0 件（対象不一致）は保存せず、レポートだけ返す。
#[tokio::test]
async fn zero_applied_ops_do_not_save() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice).await;

    let outcome = env
        .editor
        .edit_file(
            &alice,
            node.id,
            &[serde_json::json!({ "op": "replace_text", "find": "missing", "replace": "x" })],
            None,
        )
        .await
        .expect("edit");
    assert!(outcome.saved.is_none());
    assert_eq!(outcome.report.applied_ops, 0);
    let meta = env
        .storage
        .get_metadata(&alice, node.id, None)
        .await
        .expect("meta");
    assert_eq!(meta.version, 1, "版が増えないこと");
}

/// 通常版（非提案）を採用対象に指定しても NotFound（採用は提案専用）。
#[tokio::test]
async fn adopt_rejects_non_proposal_version() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    let node = create_docx(&env, &alice).await;
    let err = env
        .storage
        .adopt_proposal_version(&alice, node.id, 1, None)
        .await
        .expect_err("通常版は採用対象外");
    assert!(matches!(err, storage::StorageError::NotFound));
}

/// AI 編集非対応の content_type は明示エラー（保存もしない）。
#[tokio::test]
async fn unsupported_content_type_is_rejected() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice", "default");
    env.authz.grant(&alice.subject(), Relation::Member);
    env.authz.grant(&alice.subject(), Relation::Editor);
    env.authz.grant(&alice.subject(), Relation::Viewer);
    let node = env
        .storage
        .write_file_internal(
            &alice,
            None,
            &format!("note-{}.md", Uuid::new_v4()),
            format!("md:nonce:{}", Uuid::new_v4()).as_bytes(),
            "text/markdown",
            None,
        )
        .await
        .expect("create md");
    let err = edit(&env, &alice, node.id).await.expect_err("md は非対応");
    assert!(matches!(err, OfficeError::Invalid(_)));
}
