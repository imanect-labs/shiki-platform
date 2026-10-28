//! ユーザー設定（設定 → チャット／表示・#952）の保存と、既定の承認モードの適用の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。読み書きが本人の
//! (tenant_id, org, user_id) に閉じること、全自動の既定が org キャップに従うこと、既定の承認モードを
//! サーバが勝手にスレッドへ適用しないこと、変更が監査に残ることを確かめる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::preferences::{DefaultModel, SubmitKey, TimeFormat};
use chat::{AutonomousMode, ChatError, ChatStore, UserPreferences};
use sqlx::{postgres::PgPoolOptions, PgPool};

/// スレッド作成の owner タプルを書けるよう全許可（設定の読み書き自体は ReBAC を引かない）。
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

async fn setup() -> Option<(PgPool, ChatStore)> {
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
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    Some((pool, store))
}

fn ctx(tenant: &str, org: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        org.into(),
        tenant.into(),
    )
}

fn new_tenant() -> String {
    format!("t-{}", uuid::Uuid::new_v4())
}

/// org で全自動を許可するか（tenant 行を作る・未登録は許可扱い）。
async fn set_bypass_allowed(pool: &PgPool, tenant: &str, allowed: bool) {
    sqlx::query(
        "INSERT INTO tenant (tenant_id, org, display_name, allow_autonomous_bypass) \
         VALUES ($1, 'acme', 'Acme', $2) \
         ON CONFLICT (tenant_id) DO UPDATE SET allow_autonomous_bypass = EXCLUDED.allow_autonomous_bypass",
    )
    .bind(tenant)
    .bind(allowed)
    .execute(pool)
    .await
    .unwrap();
}

fn with_mode(mode: AutonomousMode) -> UserPreferences {
    UserPreferences {
        default_autonomous_mode: mode,
        ..UserPreferences::default()
    }
}

/// 保存→読み出しが本人の分に閉じる（同じ user_id でも別テナント・別 org・別ユーザーからは見えない）。
#[tokio::test]
async fn saved_preferences_are_scoped_to_the_owner() {
    let Some((_pool, store)) = setup().await else {
        return;
    };
    let tenant = new_tenant();
    let alice = ctx(&tenant, "acme", "alice");
    assert_eq!(
        store.get_preferences(&alice).await.unwrap(),
        UserPreferences::default(),
        "未登録は既定値"
    );
    let input = UserPreferences {
        submit_key: SubmitKey::ModEnter,
        default_model: Some(DefaultModel {
            model: Some("  claude-sonnet ".into()),
            effort: llm_gateway::Effort::High,
        }),
        web_search_by_default: true,
        default_autonomous_mode: AutonomousMode::Auto,
        expand_reasoning: true,
        time_format: TimeFormat::Relative,
    };
    let saved = store
        .put_preferences(&alice, input.clone(), Some("trace-1"))
        .await
        .unwrap();
    assert_eq!(
        saved.default_model.as_ref().unwrap().model.as_deref(),
        Some("claude-sonnet"),
        "正規化した値を保存して返す"
    );
    assert_eq!(store.get_preferences(&alice).await.unwrap(), saved);

    let other_tenant = new_tenant();
    for (label, other) in [
        ("別ユーザー", ctx(&tenant, "acme", "bob")),
        ("別 org", ctx(&tenant, "other-org", "alice")),
        ("別テナント", ctx(&other_tenant, "acme", "alice")),
    ] {
        assert_eq!(
            store.get_preferences(&other).await.unwrap(),
            UserPreferences::default(),
            "{label}からは見えない"
        );
    }

    // 上書き（置き換え）。
    let updated = store
        .put_preferences(&alice, UserPreferences::default(), None)
        .await
        .unwrap();
    assert_eq!(store.get_preferences(&alice).await.unwrap(), updated);
}

/// org が全自動を禁止していると、既定の承認モードを全自動にはできない（既存の値は変わらない）。
#[tokio::test]
async fn bypass_default_is_rejected_when_org_forbids_it() {
    let Some((pool, store)) = setup().await else {
        return;
    };
    let tenant = new_tenant();
    let alice = ctx(&tenant, "acme", "alice");
    set_bypass_allowed(&pool, &tenant, false).await;
    let before = store
        .put_preferences(&alice, with_mode(AutonomousMode::Auto), None)
        .await
        .unwrap();
    let err = store
        .put_preferences(&alice, with_mode(AutonomousMode::Bypass), None)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatError::Invalid(_)), "{err:?}");
    assert_eq!(store.get_preferences(&alice).await.unwrap(), before);

    // 許可されていれば保存できる。
    set_bypass_allowed(&pool, &tenant, true).await;
    store
        .put_preferences(&alice, with_mode(AutonomousMode::Bypass), None)
        .await
        .unwrap();
}

/// 既定の承認モードはサーバが勝手にスレッドへ適用しない（スレッドは承認必須・設定者なしで作る）。
#[tokio::test]
async fn default_mode_is_not_applied_to_new_threads() {
    let Some((_pool, store)) = setup().await else {
        return;
    };
    let tenant = new_tenant();
    let alice = ctx(&tenant, "acme", "alice");
    store
        .put_preferences(&alice, with_mode(AutonomousMode::Bypass), None)
        .await
        .unwrap();
    let t = store
        .create_thread(&alice, "既定を持っていても", true, None, None)
        .await
        .unwrap();
    assert_eq!(t.autonomous_mode, AutonomousMode::RequireApproval);
    assert_eq!(
        store.thread_autonomous_mode(t.id, &tenant).await.unwrap(),
        (AutonomousMode::RequireApproval, None),
        "既定の承認モードは作成に効かない"
    );
}

/// 全自動を既定にした後で org が禁止しても、保存済みの全自動を持ったままほかの項目は保存できる
/// （画面は設定全体を送る）。一度外したら、禁止の間は全自動へ戻せない。
#[tokio::test]
async fn saved_bypass_does_not_block_other_settings_after_org_forbids_it() {
    let Some((pool, store)) = setup().await else {
        return;
    };
    let tenant = new_tenant();
    let alice = ctx(&tenant, "acme", "alice");
    store
        .put_preferences(&alice, with_mode(AutonomousMode::Bypass), None)
        .await
        .unwrap();
    set_bypass_allowed(&pool, &tenant, false).await;

    let saved = store
        .put_preferences(
            &alice,
            UserPreferences {
                submit_key: SubmitKey::ModEnter,
                ..with_mode(AutonomousMode::Bypass)
            },
            None,
        )
        .await
        .expect("全自動を新しく選んだのでなければ拒否しない");
    assert_eq!(saved.submit_key, SubmitKey::ModEnter);
    assert_eq!(saved.default_autonomous_mode, AutonomousMode::Bypass);

    store
        .put_preferences(&alice, with_mode(AutonomousMode::Auto), None)
        .await
        .unwrap();
    let err = store
        .put_preferences(&alice, with_mode(AutonomousMode::Bypass), None)
        .await
        .unwrap_err();
    assert!(matches!(err, ChatError::Invalid(_)), "{err:?}");
}

/// 変更は改竄検知のチェーンに載る監査に残り、保存した値を記録する。
#[tokio::test]
async fn audit_records_the_saved_preferences() {
    let Some((pool, store)) = setup().await else {
        return;
    };
    let tenant = new_tenant();
    let alice = ctx(&tenant, "acme", "alice");
    store
        .put_preferences(&alice, with_mode(AutonomousMode::Auto), Some("trace-9"))
        .await
        .unwrap();
    let (action, metadata, chained): (String, serde_json::Value, bool) = sqlx::query_as(
        "SELECT action, metadata, chained FROM audit_log \
         WHERE tenant_id = $1 AND action = 'me.preferences.update' ORDER BY id DESC LIMIT 1",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(action, "me.preferences.update");
    assert!(chained, "設定の変更は改竄検知のチェーンに載る");
    assert_eq!(metadata["default_autonomous_mode"], "auto");
    assert_eq!(metadata["submit_key"], "enter");
}
