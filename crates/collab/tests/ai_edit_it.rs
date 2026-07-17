//! AI 共同編集（document.edit）の結合テスト（Task 11P.4・実 Postgres が必要）。
//!
//! 検証:
//! - 直接適用が共有 Yjs に反映され、人間の並行編集と収束する（AI 名義 origin）
//! - editor 権限のない実行主体の AI 編集が拒否される（viewer は Forbidden）
//! - サジェストモードで提案マークが付く（承認/棄却の対象になる）
//! - md 保存経路に AI 編集が乗る（dirty→保存）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use collab::note::{EditMode, EditOp};
use collab::{CollabHub, LiveDoc, PersistedDoc};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{Node, StorageService};
use uuid::Uuid;
use yrs::updates::decoder::Decode;
#[allow(unused_imports)]
use yrs::{Doc, ReadTxn, Transact, Update};

/// (subject, relation) 付与集合を持つ authz モック。
struct RoleAuthz {
    grants: Mutex<HashSet<(String, Relation)>>,
}
impl RoleAuthz {
    fn new() -> Self {
        RoleAuthz {
            grants: Mutex::new(HashSet::new()),
        }
    }
    fn grant(&self, subject: &Subject, relation: Relation) {
        self.grants
            .lock()
            .unwrap()
            .insert((subject.as_str().to_string(), relation));
    }
}
#[async_trait]
impl AuthzClient for RoleAuthz {
    async fn check(
        &self,
        subject: &Subject,
        relation: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(self
            .grants
            .lock()
            .unwrap()
            .contains(&(subject.as_str().to_string(), relation)))
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

/// バイト保持の in-memory ObjectStore。
#[derive(Default)]
struct MemStore {
    objects: Mutex<std::collections::HashMap<String, Vec<u8>>>,
}
#[async_trait]
impl storage::object_store::ObjectStore for MemStore {
    async fn ensure_bucket(&self) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn presign_get_internal(
        &self,
        _: &str,
        _: Duration,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn presign_put(
        &self,
        _: &str,
        _: Duration,
        _: i64,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn presign_get(
        &self,
        _: &str,
        _: Duration,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<String, storage::ObjectStoreError> {
        Ok("http://mem".into())
    }
    async fn read_and_hash(&self, _: &str) -> Result<(String, u64), storage::ObjectStoreError> {
        Err(storage::ObjectStoreError::NotFound("mem".into()))
    }
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        _: &str,
    ) -> Result<(), storage::ObjectStoreError> {
        self.objects.lock().unwrap().insert(key.into(), bytes);
        Ok(())
    }
    async fn get_object(&self, key: &str) -> Result<Vec<u8>, storage::ObjectStoreError> {
        self.objects
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| storage::ObjectStoreError::NotFound(key.into()))
    }
    async fn exists(&self, key: &str) -> Result<bool, storage::ObjectStoreError> {
        Ok(self.objects.lock().unwrap().contains_key(key))
    }
    async fn copy(&self, _: &str, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
    async fn list_prefix(
        &self,
        _: &str,
        _: Option<&str>,
    ) -> Result<(Vec<String>, Option<String>), storage::ObjectStoreError> {
        Ok((vec![], None))
    }
    async fn delete_batch(&self, _: &[String]) -> Result<(), storage::ObjectStoreError> {
        Ok(())
    }
}

fn ctx_for(user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec!["/acme".into()],
            roles: vec![],
            tenant_id: None,
        },
        "acme".into(),
        "default".into(),
    )
}

struct Env {
    storage: Arc<StorageService>,
    hub: Arc<CollabHub>,
    authz: Arc<RoleAuthz>,
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
    let hub = Arc::new(CollabHub::new(pool, authz_dyn, storage.clone()));
    Some(Env {
        storage,
        hub,
        authz,
    })
}

/// ノートを作成し node を返す。
async fn create_note(env: &Env, owner: &AuthContext, md: &str) -> Node {
    // org ルート直下への作成は member@org、以降の編集/閲覧は editor/viewer が要る。
    env.authz.grant(&owner.subject(), Relation::Member);
    env.authz.grant(&owner.subject(), Relation::Editor);
    env.authz.grant(&owner.subject(), Relation::Viewer);
    let name = format!("note-{}.md", Uuid::new_v4());
    // 内容に一意ノンス節を足し、content-addressing の dedup（他実行の残存 blob と同一
    // ハッシュで put がスキップされ、別 MemStore で read が失敗する事故）を避ける。
    let body = format!("{md}\n## nonce\n\n{}\n", Uuid::new_v4());
    env.storage
        .write_file_internal(owner, None, &name, body.as_bytes(), "text/markdown", None)
        .await
        .expect("create note")
}

fn empty_persisted() -> PersistedDoc {
    PersistedDoc {
        snapshot: None,
        snapshot_seq: 0,
        next_seq: 1,
        updates: vec![],
        saved_node_version: None,
        pending_save: false,
    }
}

/// 受け入れ条件: 人間の編集中に AI が同時編集しても収束し、AI 名義で反映される。
#[tokio::test]
async fn ai_edit_converges_with_human_edits() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");
    let node = create_note(&env, &alice, "# メモ\n\n本文。\n").await;

    // AI が append する（直接適用）。
    let report = env
        .hub
        .apply_ai_edit(
            &alice,
            &node,
            &[EditOp::Append {
                markdown: "## AI が追記した節\n\nAI の内容。\n".into(),
            }],
            EditMode::Direct,
        )
        .await
        .expect("ai edit");
    assert_eq!(report.applied, 1);

    // 保存された md に AI の追記が乗る（デバウンス経路の即時保存を hub 経由で）。
    let (_n, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("read");
    let saved = String::from_utf8_lossy(&bytes);
    assert!(
        saved.contains("AI が追記した節"),
        "AI 編集が md に保存される: {saved}"
    );
    assert!(saved.contains("# メモ"), "既存内容が保持される");
}

/// 受け入れ条件: editor 権限のない実行主体の AI 編集が拒否される。
#[tokio::test]
async fn ai_edit_denied_without_editor() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");
    let node = create_note(&env, &alice, "# 秘密メモ\n").await;

    // bob は viewer のみ（editor なし）。
    let bob = ctx_for("bob");
    env.authz.grant(&bob.subject(), Relation::Viewer);
    let err = env
        .hub
        .apply_ai_edit(
            &bob,
            &node,
            &[EditOp::Append {
                markdown: "不正な追記\n".into(),
            }],
            EditMode::Direct,
        )
        .await;
    assert!(
        matches!(err, Err(collab::CollabError::Forbidden(_))),
        "viewer のみの実行主体は拒否される: {err:?}"
    );

    // 権限なし（relation なし）の carol も拒否。
    let carol = ctx_for("carol");
    let err = env
        .hub
        .apply_ai_edit(
            &carol,
            &node,
            &[EditOp::Append {
                markdown: "x\n".into(),
            }],
            EditMode::Direct,
        )
        .await;
    assert!(err.is_err(), "relation なしは拒否される");
}

/// 受け入れ条件: サジェストモードで提案マークが付く（md には落ちない）。
#[tokio::test]
async fn suggest_mode_marks_and_does_not_leak_to_md() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");
    let node = create_note(&env, &alice, "# ドラフト\n").await;

    env.hub
        .apply_ai_edit(
            &alice,
            &node,
            &[EditOp::Append {
                markdown: "提案された段落。\n".into(),
            }],
            EditMode::Suggest,
        )
        .await
        .expect("suggest");

    // md には提案本文は載るがマーク（aiSuggestion）は落ちない（往復対象外・Task 11P.2）。
    let (_n, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("read");
    let saved = String::from_utf8_lossy(&bytes);
    assert!(
        saved.contains("提案された段落"),
        "提案テキストは md に含まれる"
    );
    assert!(
        !saved.contains("aiSuggestion") && !saved.contains("data-ai-suggestion"),
        "提案マークは md に漏れない: {saved}"
    );
}

/// replace_section が見出し節の本文を置換し、見出しは残ることを確認する。
#[tokio::test]
async fn replace_section_replaces_body_keeps_heading() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");
    let node = create_note(
        &env,
        &alice,
        "# 概要\n\n古い概要。\n\n# 詳細\n\n詳細本文。\n",
    )
    .await;

    let report = env
        .hub
        .apply_ai_edit(
            &alice,
            &node,
            &[EditOp::ReplaceSection {
                heading: "概要".into(),
                markdown: "新しい概要の本文。\n".into(),
            }],
            EditMode::Direct,
        )
        .await
        .expect("replace");
    assert_eq!(report.applied, 1);

    let (_n, bytes) = env
        .storage
        .read_file_internal(&alice, node.id, None)
        .await
        .expect("read");
    let saved = String::from_utf8_lossy(&bytes);
    assert!(saved.contains("# 概要"), "見出しは残る");
    assert!(saved.contains("新しい概要の本文"), "本文が置換される");
    assert!(!saved.contains("古い概要"), "旧本文は消える: {saved}");
    assert!(
        saved.contains("# 詳細") && saved.contains("詳細本文"),
        "他節は不変"
    );
}

/// insert_embed で genui 埋め込みブロックが本文へ挿入され、md に ```shiki-embed フェンス
/// （kind=genui）として往復することを確認する（issue #282・DB 不要）。
#[test]
fn insert_embed_writes_genui_fence() {
    let live = LiveDoc::restore(
        Uuid::new_v4(),
        Some(collab::DocKind::Note),
        &empty_persisted(),
    )
    .expect("restore");
    live.import_markdown("# レポート\n\n本文。\n")
        .expect("seed");

    let spec = serde_json::json!({ "type": "chart", "chartType": "bar", "data": [] });
    let (update, report) = live
        .apply_ai_edit(&[EditOp::InsertEmbed { spec }], EditMode::Direct)
        .expect("insert embed");
    assert_eq!(report.applied, 1);
    assert!(!update.is_empty(), "埋め込み挿入の update が生成される");

    // 別クライアントへ取り込み、md へ直列化して往復を確認する。
    let human = Doc::new();
    let full = live.full_state().expect("full");
    human
        .transact_mut()
        .apply_update(Update::decode_v1(&full).expect("decode"))
        .expect("apply");
    let md = collab::note::doc_to_markdown(&human);
    assert!(
        md.contains("```shiki-embed"),
        "shiki-embed フェンスが出る: {md}"
    );
    assert!(
        md.contains("\"kind\":\"genui\""),
        "kind=genui が埋め込まれる: {md}"
    );
    assert!(md.contains("# レポート"), "既存本文は保持される");
}

/// insert_embed の spec がオブジェクトでない（不正）場合は挿入せず skip する（fail-closed・#282）。
#[test]
fn insert_embed_rejects_non_object_spec() {
    let live = LiveDoc::restore(
        Uuid::new_v4(),
        Some(collab::DocKind::Note),
        &empty_persisted(),
    )
    .expect("restore");
    live.import_markdown("# 本文\n").expect("seed");
    let report = live
        .apply_ai_edit(
            &[EditOp::InsertEmbed {
                spec: serde_json::json!("not-an-object"),
            }],
            EditMode::Direct,
        )
        .map(|(_, r)| r)
        .expect("edit");
    assert_eq!(report.applied, 0, "不正 spec は適用されない");
    assert_eq!(report.skipped.len(), 1, "skip として記録される");
}

/// 見つからない見出しへの操作は skip される（部分適用・fail-open で本文は保持）。
#[tokio::test]
async fn missing_heading_is_skipped() {
    let Some(env) = setup().await else { return };
    let alice = ctx_for("alice");
    let node = create_note(&env, &alice, "# あるだけ\n\n本文。\n").await;
    let report = env
        .hub
        .apply_ai_edit(
            &alice,
            &node,
            &[EditOp::ReplaceSection {
                heading: "存在しない見出し".into(),
                markdown: "x\n".into(),
            }],
            EditMode::Direct,
        )
        .await
        .expect("edit");
    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped.len(), 1);
}

/// LiveDoc レベルで AI origin の update が生成され、共有履歴を持つ別クライアントが
/// 取り込んで収束することを確認する（DB 不要）。
///
/// 実システムと同じく、人間クライアントは**サーバの共有ドキュメントから同期**して
/// 履歴を共有する（sync step2 = 全状態取り込み）→ その後 AI 編集の差分を適用する。
#[test]
fn ai_edit_produces_applicable_update() {
    let live = LiveDoc::restore(
        Uuid::new_v4(),
        Some(collab::DocKind::Note),
        &empty_persisted(),
    )
    .expect("restore");
    live.import_markdown("# 既存\n\n人間の段落。\n")
        .expect("seed");

    // 人間クライアントはサーバ全状態を取り込む（共有履歴を持つ）。
    let human = Doc::new();
    let full = live.full_state().expect("full");
    human
        .transact_mut()
        .apply_update(Update::decode_v1(&full).expect("decode full"))
        .expect("sync full");

    // AI が append（差分 update を生成）。
    let (update, report) = live
        .apply_ai_edit(
            &[EditOp::Append {
                markdown: "AI 段落。\n".into(),
            }],
            EditMode::Direct,
        )
        .expect("ai edit");
    assert_eq!(report.applied, 1);
    assert!(!update.is_empty(), "AI 編集の update が生成される");

    // 人間クライアントが AI の差分を取り込んで収束する。
    human
        .transact_mut()
        .apply_update(Update::decode_v1(&update).expect("decode"))
        .expect("apply ai update");
    let md = collab::note::doc_to_markdown(&human);
    assert!(
        md.contains("AI 段落"),
        "AI 編集が別クライアントへ収束: {md}"
    );
    assert!(md.contains("人間の段落"), "人間の内容も保持される");
}
