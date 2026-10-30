//! アーティファクトの一覧を ListObjects の上限で切らない（BUG-149）の結合テスト（実 Postgres ＋実 OpenFGA）。
//!
//! 見られるアーティファクトが ListObjects の上限（1,000 件）を超えた人にも、
//! - 共有された一覧（ワークフローの一覧・アーティファクトの一覧）に共有された分が出る
//! - テーブルの保存ビュー（本文の参照で絞る一覧）に、共有されたビューが出る
//! - テーブルの状態の進め方の影響（本文にテーブルの id が書かれたワークフロー）に、共有されたワークフローが出る
//!
//! どれも見えないものは出ない。上限を超えるタプルは OpenFGA へ直接書く（DB に無いアーティファクトのタプル＝
//! 消したものの残りと同じ形）。最後に書いたタプル・作った行・ストアを片付ける（落ちても片付ける）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;

use artifact::{Artifact, ArtifactKind, ArtifactRole, ArtifactStore, NewArtifact};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, ObjectType, Principal, Relation};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

struct Env {
    store: ArtifactStore,
    authz: Arc<OpenFgaClient>,
    pool: PgPool,
    http: reqwest::Client,
    base_url: String,
}

impl Env {
    /// タプルを直接書く・消す（Write API は 1 回 100 件まで）。
    async fn write(&self, key: &str, tuples: &[(String, String)]) {
        for chunk in tuples.chunks(100) {
            let keys: Vec<serde_json::Value> = chunk
                .iter()
                .map(|(u, o)| json!({ "user": u, "relation": "viewer", "object": o }))
                .collect();
            let resp = self
                .http
                .post(format!(
                    "{}/stores/{}/write",
                    self.base_url,
                    self.authz.store_id()
                ))
                .json(&json!({
                    key: { "tuple_keys": keys },
                    "authorization_model_id": self.authz.model_id(),
                }))
                .send()
                .await
                .expect("OpenFGA write");
            assert!(
                resp.status().is_success(),
                "{key}: {}",
                resp.text().await.unwrap_or_default()
            );
        }
    }
}

async fn setup() -> Option<Env> {
    let (Ok(db_url), Ok(base_url)) = (
        std::env::var("STORAGE_TEST_DATABASE_URL"),
        std::env::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
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
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json"))
            .expect("model json");
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: base_url.clone(),
            store_name: format!("shiki-artifact-list-cap-{}", Uuid::new_v4()),
        },
        &model,
    )
    .await
    .expect("OpenFGA 接続");
    let authz = Arc::new(fga);
    let dyn_authz: Arc<dyn AuthzClient> = authz.clone();
    Some(Env {
        store: ArtifactStore::new(pool.clone(), dyn_authz),
        authz,
        pool,
        http: reqwest::Client::new(),
        base_url,
    })
}

fn ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        tenant.into(),
    )
}

/// 上限を超える件数。
const OVER_CAP: usize = authz::visible::LIST_OBJECTS_MAX_RESULTS + 50;

/// DB に無いアーティファクトへの viewer のタプル（id は小さい順に並び、OpenFGA が先に読む側を埋める）。
fn leftovers(who: &AuthContext) -> Vec<(String, String)> {
    (0..OVER_CAP)
        .map(|i| {
            let id = format!("00000000-0000-4000-8000-{i:012}");
            (
                who.subject().as_str().to_string(),
                who.ns().artifact(&id).as_str().to_string(),
            )
        })
        .collect()
}

async fn make(
    env: &Env,
    owner: &AuthContext,
    kind: ArtifactKind,
    name: &str,
    body: serde_json::Value,
    share_to: Option<&str>,
) -> Artifact {
    let a = env
        .store
        .create(
            owner,
            NewArtifact {
                kind,
                name: name.into(),
                body,
            },
            None,
        )
        .await
        .expect("create");
    if let Some(who) = share_to {
        env.store
            .share(
                owner,
                a.id,
                &storage::ShareTarget::User { id: who.into() },
                ArtifactRole::Viewer,
                None,
            )
            .await
            .expect("share");
    }
    a
}

fn names(items: &[Artifact]) -> Vec<&str> {
    items.iter().map(|a| a.name.as_str()).collect()
}

#[tokio::test]
async fn lists_show_shared_artifacts_past_the_list_objects_cap() {
    let Some(env) = setup().await else { return };
    let env = Arc::new(env);
    let tenant = format!("t-{}", Uuid::new_v4());
    let written = leftovers(&ctx(&tenant, "bob"));
    env.write("writes", &written).await;
    let outcome = tokio::spawn(past_the_cap(Arc::clone(&env), tenant.clone())).await;
    // 後片付け（落ちても）: 直接書いたタプル・作ったアーティファクトのタプルと行・ストア。
    env.write("deletes", &written).await;
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM artifact WHERE tenant_id = $1")
        .bind(&tenant)
        .fetch_all(&env.pool)
        .await
        .unwrap();
    let alice = ctx(&tenant, "alice");
    let ns = alice.ns();
    for id in ids {
        env.authz
            .delete_object_tuples(&ns.artifact(&id.to_string()))
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM artifact_version WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&env.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM artifact WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&env.pool)
        .await
        .unwrap();
    let resp = env
        .http
        .delete(format!("{}/stores/{}", env.base_url, env.authz.store_id()))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "delete store: {}",
        resp.status()
    );
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

async fn past_the_cap(env: Arc<Env>, tenant: String) {
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let cut = env
        .authz
        .list_objects(&bob.subject(), Relation::Viewer, ObjectType::Artifact)
        .await
        .unwrap();
    assert!(
        authz::visible::list_may_be_cut(cut.len()),
        "前提: bob の ListObjects が上限に届いている（{} 件）",
        cut.len()
    );
    let table = Uuid::new_v4();
    let wf = ArtifactKind::Workflow;
    let view = ArtifactKind::DataView;
    let uses =
        |n: &str| json!({ "display_name": n, "nodes": [{ "config": { "table_id": table } }] });
    let w_shared = make(&env, &alice, wf, "wf-共有", json!({}), Some("bob")).await;
    make(&env, &alice, wf, "wf-秘密", json!({}), None).await;
    let w_uses = make(&env, &alice, wf, "wf-使う-共有", uses("a"), Some("bob")).await;
    make(&env, &alice, wf, "wf-使う-秘密", uses("b"), None).await;
    let v_shared = make(
        &env,
        &alice,
        view,
        "view-共有",
        json!({ "table_id": table }),
        Some("bob"),
    )
    .await;
    make(
        &env,
        &alice,
        view,
        "view-秘密",
        json!({ "table_id": table }),
        None,
    )
    .await;
    let v_mine = make(
        &env,
        &bob,
        view,
        "view-自分",
        json!({ "table_id": table }),
        None,
    )
    .await;

    // 共有された一覧（ワークフローの一覧の共有分・アーティファクトの一覧）。
    let shared = env
        .store
        .list_shared_with_me(&bob, Some(wf), 50)
        .await
        .unwrap();
    assert_eq!(names(&shared), vec!["wf-使う-共有", "wf-共有"]);
    let all = env.store.list_shared_with_me(&bob, None, 50).await.unwrap();
    assert_eq!(names(&all), vec!["view-共有", "wf-使う-共有", "wf-共有"]);
    assert!(
        all.iter().all(|a| a.id != v_mine.id),
        "自分のものは共有された一覧に出ない"
    );
    let one = env.store.list_shared_with_me(&bob, None, 1).await.unwrap();
    assert_eq!(names(&one), vec!["view-共有"]);

    // テーブルの保存ビュー（自分のものと共有されたもの・新しい順）。
    let views = env
        .store
        .list_visible_with_body_ref(&bob, view, "table_id", &table.to_string(), 100)
        .await
        .unwrap();
    assert_eq!(
        views.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![v_mine.id, v_shared.id]
    );

    // テーブルを使うワークフロー（状態の進め方の影響）。本文も返す。
    let using = env
        .store
        .list_visible_mentioning(&bob, wf, table, 50)
        .await
        .unwrap();
    assert_eq!(using.len(), 1);
    assert_eq!(using[0].0.id, w_uses.id);
    assert_eq!(using[0].1["display_name"], "a");
    // 持ち主の alice には両方（見えないものが無いので OpenFGA に問わない）。
    let mine = env
        .store
        .list_visible_mentioning(&alice, wf, table, 50)
        .await
        .unwrap();
    assert_eq!(mine.len(), 2);
    assert!(shared.iter().any(|a| a.id == w_shared.id));
}
