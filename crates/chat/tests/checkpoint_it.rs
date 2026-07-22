//! チェックポイント永続化の durable 配線テスト（#351）。
//!
//! ワーカーを spawn せず store プリミティブで検証する（jobq を他テストと奪い合わない）:
//! - claim した run へ fencing 一致でのみ checkpoint を書ける（ゾンビ書込拒否）。
//! - リース失効後の takeover（再 claim）で checkpoint が `ClaimedRun` に載って戻る（resume 材料）。
//! - finalize（端末確定）で checkpoint が NULL に落ちる。
//!
//! 実 Postgres が必要（`STORAGE_TEST_DATABASE_URL`）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::{ChatStore, RunStatus, StreamEventKind};
use sqlx::{postgres::PgPoolOptions, PgPool};

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

/// claim → fenced checkpoint 保存 → リース失効 takeover で checkpoint が resume 材料として
/// 引き回され、finalize で消える（#351 の durable 配線）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_survives_takeover_and_clears_on_finalize() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let store = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let c = ctx(&tenant);
    let thread = store
        .create_thread(&c, "resume", false, None, None)
        .await
        .unwrap();
    let res = store
        .post_message(&c, thread.id, "goal", &[], None, None, true, None)
        .await
        .unwrap();

    // worker-a が claim（短いリース）。checkpoint はまだ無い。
    let a = store
        .claim_run(res.run_id, "worker-a", 1)
        .await
        .unwrap()
        .expect("claim できる");
    assert!(a.checkpoint.is_none(), "初回 claim に checkpoint は無い");
    assert_eq!(
        a.autonomous_mode, "require_approval",
        "既定モードが run へスナップショットされる"
    );

    // fencing 一致でのみ保存できる（ゾンビ書込拒否）。
    let cp = serde_json::json!({ "step": 2, "spent": { "steps": 2 } });
    assert!(
        store
            .save_checkpoint(res.run_id, a.fencing_token, &cp)
            .await
            .unwrap(),
        "現リース保持者は保存できる"
    );
    assert!(
        !store
            .save_checkpoint(res.run_id, a.fencing_token - 1, &cp)
            .await
            .unwrap(),
        "旧 fencing（ゾンビ）は保存できない"
    );

    // リース失効後、worker-b が takeover → checkpoint が claim 結果に載る（resume の材料）。
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let b = store
        .claim_run(res.run_id, "worker-b", 30)
        .await
        .unwrap()
        .expect("リース失効後は takeover できる");
    assert_eq!(b.fencing_token, a.fencing_token + 1, "fencing が進む");
    assert_eq!(
        b.checkpoint.as_ref().map(|j| j.0.clone()),
        Some(cp.clone()),
        "takeover した run は保存済み checkpoint を受け取る"
    );
    // takeover 後は旧ワーカーの保存が弾かれる。
    assert!(!store
        .save_checkpoint(res.run_id, a.fencing_token, &cp)
        .await
        .unwrap());

    // 端末確定で checkpoint は NULL に落ちる（再開対象でなくなる）。
    assert!(store
        .finalize_run(
            res.run_id,
            b.fencing_token,
            RunStatus::Done,
            &[],
            None,
            Some(&StreamEventKind::Done {
                message_id: res.assistant_message_id,
            }),
        )
        .await
        .unwrap());
    let cleared: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT checkpoint FROM generation_run WHERE run_id = $1")
            .bind(res.run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(cleared.is_none(), "finalize で checkpoint がクリアされる");
}
