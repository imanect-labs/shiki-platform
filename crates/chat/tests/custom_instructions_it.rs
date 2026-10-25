//! カスタム指示（設定 → パーソナライズ・#900）の保存の結合テスト。
//!
//! `STORAGE_TEST_DATABASE_URL` が設定されている時のみ実行。読み書きが本人の
//! (tenant_id, org, user_id) に閉じること、正規化・上限・空での削除、監査に本文を残さないことを確かめる。

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
use chat::custom_instructions::{CustomInstructionTone, MAX_CHARS};
use chat::{ChatError, ChatStore, CustomInstructions};
use sqlx::{postgres::PgPoolOptions, PgPool};

/// カスタム指示は ReBAC を引かない。呼ばれたら失敗させ、authz に依存しないことも確かめる。
struct DenyAll;

#[async_trait]
impl AuthzClient for DenyAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(false)
    }
    async fn write_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(false)
    }
    async fn delete_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(false)
    }
    async fn read_tuples(
        &self,
        _o: &FgaObject,
        _r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        Ok(vec![])
    }
    async fn list_objects(
        &self,
        _s: &Subject,
        _r: Relation,
        _t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        Ok(vec![])
    }
    async fn delete_object_tuples(&self, _o: &FgaObject) -> Result<u32, AuthzError> {
        Ok(0)
    }
    async fn read_subject_objects(
        &self,
        _s: &Subject,
        _t: ObjectType,
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

fn instructions(about: &str, style: &str, tone: CustomInstructionTone) -> CustomInstructions {
    CustomInstructions {
        about: about.into(),
        style: style.into(),
        tone,
    }
}

/// 保存→読み出しが本人の分に閉じる（同じ user_id でも別テナント・別 org・別ユーザーからは見えない）。
#[tokio::test]
async fn saved_instructions_are_scoped_to_the_owner() {
    let Some(pool) = setup().await else { return };
    let store = ChatStore::connect(pool.clone(), Arc::new(DenyAll), None)
        .await
        .unwrap();
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");

    assert_eq!(
        store.get_custom_instructions(&alice).await.unwrap(),
        CustomInstructions::default(),
        "未登録は空"
    );
    let saved = store
        .put_custom_instructions(
            &alice,
            instructions(
                "  法人営業。\r\n提案書が多い。  ",
                "結論を先に。",
                CustomInstructionTone::Formal,
            ),
            Some("trace-1"),
        )
        .await
        .unwrap();
    assert_eq!(
        saved,
        instructions(
            "法人営業。\n提案書が多い。",
            "結論を先に。",
            CustomInstructionTone::Formal
        ),
        "正規化した値を保存して返す"
    );
    assert_eq!(store.get_custom_instructions(&alice).await.unwrap(), saved);

    let other_tenant = format!("t-{}", uuid::Uuid::new_v4());
    for (label, other) in [
        ("別ユーザー", ctx(&tenant, "acme", "bob")),
        ("別 org", ctx(&tenant, "other-org", "alice")),
        ("別テナント", ctx(&other_tenant, "acme", "alice")),
    ] {
        assert_eq!(
            store.get_custom_instructions(&other).await.unwrap(),
            CustomInstructions::default(),
            "{label}からは見えない"
        );
    }

    // 上書き。
    let updated = store
        .put_custom_instructions(
            &alice,
            instructions("", "箇条書きで。", CustomInstructionTone::Concise),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        store.get_custom_instructions(&alice).await.unwrap(),
        updated
    );
}

/// すべて空にすると行が消える（未設定に戻る）。
#[tokio::test]
async fn clearing_everything_deletes_the_row() {
    let Some(pool) = setup().await else { return };
    let store = ChatStore::connect(pool.clone(), Arc::new(DenyAll), None)
        .await
        .unwrap();
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");
    store
        .put_custom_instructions(
            &alice,
            instructions("営業", "", CustomInstructionTone::Default),
            None,
        )
        .await
        .unwrap();
    let rows = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM user_custom_instruction WHERE tenant_id = $1",
        )
        .bind(&tenant)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(rows().await, 1);
    let cleared = store
        .put_custom_instructions(
            &alice,
            instructions("  ", "\n", CustomInstructionTone::Default),
            None,
        )
        .await
        .unwrap();
    assert_eq!(cleared, CustomInstructions::default());
    assert_eq!(rows().await, 0, "空の設定は行を持たない");
    // 口調だけでも設定として残る。
    store
        .put_custom_instructions(
            &alice,
            instructions("", "", CustomInstructionTone::Casual),
            None,
        )
        .await
        .unwrap();
    assert_eq!(rows().await, 1);
}

/// 上限を超えたら保存せずに拒否する（既存の値は変わらない）。
#[tokio::test]
async fn over_limit_is_rejected_and_keeps_previous_value() {
    let Some(pool) = setup().await else { return };
    let store = ChatStore::connect(pool.clone(), Arc::new(DenyAll), None)
        .await
        .unwrap();
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");
    let before = store
        .put_custom_instructions(
            &alice,
            instructions(&"あ".repeat(MAX_CHARS), "", CustomInstructionTone::Default),
            None,
        )
        .await
        .expect("上限ちょうどは通る");
    let err = store
        .put_custom_instructions(
            &alice,
            instructions(
                &"あ".repeat(MAX_CHARS + 1),
                "",
                CustomInstructionTone::Default,
            ),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ChatError::Invalid(_)), "{err:?}");
    assert_eq!(store.get_custom_instructions(&alice).await.unwrap(), before);
}

/// 変更は監査に残るが、本文は残さない（文字数と口調だけ）。
#[tokio::test]
async fn audit_records_lengths_but_not_the_text() {
    let Some(pool) = setup().await else { return };
    let store = ChatStore::connect(pool.clone(), Arc::new(DenyAll), None)
        .await
        .unwrap();
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let alice = ctx(&tenant, "acme", "alice");
    store
        .put_custom_instructions(
            &alice,
            instructions(
                "秘密のプロジェクト担当",
                "敬語で",
                CustomInstructionTone::Formal,
            ),
            None,
        )
        .await
        .unwrap();
    let (action, metadata, chained): (String, serde_json::Value, bool) = sqlx::query_as(
        "SELECT action, metadata, chained FROM audit_log WHERE tenant_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(action, "me.custom_instructions.update");
    assert!(chained, "設定の変更は改竄検知のチェーンに載る");
    assert_eq!(
        metadata,
        serde_json::json!({ "about_chars": 11, "style_chars": 3, "tone": "formal" })
    );
    assert!(!metadata.to_string().contains("秘密"), "{metadata}");
}
