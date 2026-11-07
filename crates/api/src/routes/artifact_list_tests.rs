//! 自分のものと共有されたものを合わせた一覧の続き（[`super::list_page`]・`GET /artifacts`・`GET /workflows`）の結合
//! テスト（実 Postgres ＋実 OpenFGA）。
//!
//! `STORAGE_TEST_DATABASE_URL`・`OPENFGA_TEST_URL` のどちらかが無ければスキップする。テナントと OpenFGA のストアは
//! テストごとに作り、作った行とストアはテストの最後に消す（落ちても消す）。
//!
//! 1. 先頭のページは自分のものと共有されたものを合わせて返し、続きの位置はそれぞれ別に返す。合わせたページの最後が
//!    共有されたものでも、自分のものの続きの位置は自分のものの最後の 1 件になる（BUG-185。最後の 1 件を位置に使うと、
//!    それより新しい自分のものを読み飛ばす）。
//! 2. 自分のものの続きは自分のものだけを、共有された分の続きは共有された分だけを、重ならずに返す（BUG-186・BUG-162）。
//! 3. ちょうど `limit` 件で終わる一覧には、どちらの続きの位置も返さない。
//! 4. 両方の続きの位置を同時には渡せない。読めない共有された分の位置は断る。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use artifact::{Artifact, ArtifactKind, ArtifactRole, ArtifactStore, NewArtifact};
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, PrincipalKind};
use data::{DataStore, RefResolver};
use futures::FutureExt as _;
use storage::ShareTarget;
use uuid::Uuid;

use super::{list_page, ListPage, ListPageRequest};

/// 参照の確かめ（一覧は使わない）。
struct NoRefs;

#[async_trait::async_trait]
impl RefResolver for NoRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(false)
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(false)
    }
}

fn ctx(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "acme".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 1 つの筋書き（作った行は呼ぶ側が消す）。
type Scenario =
    for<'a> fn(&'a ArtifactStore, &'a DataStore, &'a str) -> futures::future::BoxFuture<'a, ()>;

/// テナントと OpenFGA のストアを作って筋書きを走らせ、落ちても作った行とストアを消す。
async fn run(scenario: Scenario) {
    let (Ok(db_url), Ok(fga_url)) = (
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL"),
        shiki_testenv::var("OPENFGA_TEST_URL"),
    ) else {
        eprintln!("STORAGE_TEST_DATABASE_URL / OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let client = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url.clone(),
            store_name: format!("shiki-artifact-list-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("fga");
    let store_id = client.store_id().to_string();
    let fga: Arc<dyn AuthzClient> = Arc::new(client);
    let artifacts = ArtifactStore::new(pool.clone(), Arc::clone(&fga));
    let data = DataStore::new(pool.clone(), Arc::clone(&fga), Arc::new(NoRefs));
    let tenant = format!("t-list-{}", Uuid::new_v4().simple());

    let outcome = AssertUnwindSafe(scenario(&artifacts, &data, &tenant))
        .catch_unwind()
        .await;

    // 後片付け（落ちても）: 作った行とストア。
    for table in ["artifact_version", "artifact", "audit_log"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
            .bind(&tenant)
            .execute(&pool)
            .await
            .expect("cleanup");
    }
    let deleted = reqwest::Client::new()
        .delete(format!("{fga_url}/stores/{store_id}"))
        .send()
        .await
        .expect("delete store");
    assert!(deleted.status().is_success(), "{}", deleted.status());
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn owned_and_shared_continuations_are_read_separately() {
    run(|artifacts, data, tenant| Box::pin(scenario(artifacts, data, tenant))).await;
}

/// 共有されたミニアプリを使う人（editor でない viewer）には、一覧でも取得でも公開した版だけを見せる（下書きの番号・
/// 前に公開していた版・下書きの保存の時刻を見せない・BUG-213）。作る人（持ち主・editor）には今までどおり見せる。
#[tokio::test]
async fn users_of_a_shared_app_see_only_the_published_version() {
    run(|artifacts, data, tenant| Box::pin(drafts_scenario(artifacts, data, tenant))).await;
}

async fn drafts_scenario(artifacts: &ArtifactStore, data: &DataStore, tenant: &str) {
    let alice = ctx(tenant, &format!("alice-{}", Uuid::new_v4().simple()));
    let bob = ctx(tenant, &format!("bob-{}", Uuid::new_v4().simple()));
    let carol = ctx(tenant, &format!("carol-{}", Uuid::new_v4().simple()));
    let app = artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::MiniApp,
                name: "expense-app".into(),
                body: serde_json::json!({ "v": 1 }),
            },
            None,
        )
        .await
        .expect("create");
    for (who, role) in [(&bob, ArtifactRole::Viewer), (&carol, ArtifactRole::Editor)] {
        let to = ShareTarget::User {
            id: who.principal().id.clone(),
        };
        artifacts
            .share(&alice, app.id, &to, role, None)
            .await
            .expect("share");
    }
    let alice_ref = &alice;
    let save = |v: i64| async move {
        artifacts
            .append_version(
                alice_ref,
                app.id,
                serde_json::json!({ "v": v }),
                Some(v - 1),
                None,
            )
            .await
            .expect("save");
    };
    let listed = |who: AuthContext| async move {
        let page = list_page(
            artifacts,
            data,
            &who,
            ListPageRequest {
                kind: Some(ArtifactKind::MiniApp),
                before: None,
                limit: 10,
                shared_cursor: None,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("list_page: {e:?}"));
        let got = artifacts
            .get_as_seen(&who, app.id, None)
            .await
            .expect("get");
        assert_eq!(page.items.len(), 1);
        let item = page.items.into_iter().next().unwrap();
        // 一覧と取得は同じ見え方。
        assert_eq!(
            (
                item.current_version,
                item.previous_published_version,
                item.updated_at
            ),
            (
                got.current_version,
                got.previous_published_version,
                got.updated_at
            )
        );
        got
    };

    // まだ公開していない: 使う人には版 0・作った時刻。
    let seen = listed(bob.clone()).await;
    assert_eq!((seen.current_version, seen.published_version), (0, None));
    assert_eq!(seen.updated_at, app.created_at);

    // v1 を公開し、v2・v3 を下書きで保存した後に v2 を公開し、v3 を下書きで保存する。
    artifacts
        .publish(&alice, app.id, 1, None)
        .await
        .expect("publish v1");
    save(2).await;
    artifacts
        .publish(&alice, app.id, 2, None)
        .await
        .expect("publish v2");
    save(3).await;

    let seen = listed(bob.clone()).await;
    assert_eq!(
        (
            seen.current_version,
            seen.published_version,
            seen.previous_published_version
        ),
        (2, Some(2), None),
        "使う人には下書きの番号と前に公開していた版を見せない"
    );
    assert_eq!(
        Some(seen.updated_at),
        seen.published_at,
        "使う人の更新の時刻は公開した時刻（下書きの保存で進まない）"
    );
    for maker in [carol.clone(), alice.clone()] {
        let seen = listed(maker).await;
        assert_eq!(
            (seen.current_version, seen.previous_published_version),
            (3, Some(1)),
            "作る人には今までどおり見せる"
        );
        assert!(seen.updated_at > seen.published_at.unwrap());
    }
}

fn names(items: &[Artifact]) -> Vec<String> {
    items.iter().map(|a| a.name.clone()).collect()
}

async fn page(
    artifacts: &ArtifactStore,
    data: &DataStore,
    who: &AuthContext,
    before: Option<artifact::ArtifactPosition>,
    shared_cursor: Option<&str>,
    limit: i64,
) -> ListPage {
    list_page(
        artifacts,
        data,
        who,
        ListPageRequest {
            kind: Some(ArtifactKind::Workflow),
            before,
            limit,
            shared_cursor,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("list_page: {e:?}"))
}

async fn scenario(artifacts: &ArtifactStore, data: &DataStore, tenant: &str) {
    let alice = ctx(tenant, &format!("alice-{}", Uuid::new_v4().simple()));
    let bob = ctx(tenant, &format!("bob-{}", Uuid::new_v4().simple()));
    let to_alice = ShareTarget::User {
        id: alice.principal().id.clone(),
    };
    // 作る順（古い順）: b1・b2・a1・a2・a3・b3。共有された b1〜b3 は alice が閲覧できる。
    let mut ids = std::collections::HashMap::new();
    for (who, name) in [
        (&bob, "b1"),
        (&bob, "b2"),
        (&alice, "a1"),
        (&alice, "a2"),
        (&alice, "a3"),
        (&bob, "b3"),
    ] {
        let a = artifacts
            .create(
                who,
                NewArtifact {
                    kind: ArtifactKind::Workflow,
                    name: name.into(),
                    body: serde_json::json!({}),
                },
                None,
            )
            .await
            .expect("create");
        if who.principal().id == bob.principal().id {
            artifacts
                .share(&bob, a.id, &to_alice, ArtifactRole::Viewer, None)
                .await
                .expect("share");
        }
        ids.insert(name, a);
    }
    // ワークフローでないもの（スキル）は出ない。
    artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::Skill,
                name: "skill-a".into(),
                body: serde_json::json!({}),
            },
            None,
        )
        .await
        .expect("skill");

    // 1. 先頭のページ（2 件ずつ）: 自分のもの a3・a2 と共有された b3・b2 を合わせる。最後は共有された b2。
    let first = page(artifacts, data, &alice, None, None, 2).await;
    assert_eq!(names(&first.items), vec!["b3", "a3", "a2", "b2"]);
    let a2 = &ids["a2"];
    let owned_next = first.owned_next.expect("自分のものに続きがある");
    assert_eq!(
        (owned_next.before_updated_at, owned_next.before_id),
        (a2.updated_at, a2.id),
        "自分のものの続きの位置は、自分のものの最後の 1 件"
    );
    // 合わせたページの最後（b2）を位置にすると、それより新しい a1 を読み飛ばす（BUG-185 の形）。
    let b2 = &ids["b2"];
    let skipped = artifacts
        .list_mine_page(
            &alice,
            Some(ArtifactKind::Workflow),
            Some((b2.updated_at, b2.id)),
            2,
        )
        .await
        .unwrap();
    assert!(skipped.items.is_empty(), "{:?}", names(&skipped.items));
    let shared_cursor = first.shared_next_cursor.expect("共有された分に続きがある");
    for a in ids.values() {
        assert!(!shared_cursor.contains(&a.id.to_string()), "位置は不透明");
    }

    // 2. 自分のものの続きは自分のものだけ（a1）。読み切ったので、どちらの位置も無い。
    let owned = page(
        artifacts,
        data,
        &alice,
        Some((owned_next.before_updated_at, owned_next.before_id)),
        None,
        2,
    )
    .await;
    assert_eq!(names(&owned.items), vec!["a1"]);
    assert_eq!(owned.owned_next, None);
    assert_eq!(owned.shared_next_cursor, None);
    // 共有された分の続きは共有された分だけ（b1）。読み切ったので、どちらの位置も無い。
    let shared = page(artifacts, data, &alice, None, Some(&shared_cursor), 2).await;
    assert_eq!(names(&shared.items), vec!["b1"]);
    assert_eq!(shared.owned_next, None);
    assert_eq!(shared.shared_next_cursor, None);

    // 3. ちょうど limit 件で終わる一覧には、続きの位置を返さない。
    let exact = page(artifacts, data, &alice, None, None, 3).await;
    assert_eq!(
        names(&exact.items),
        vec!["b3", "a3", "a2", "a1", "b2", "b1"]
    );
    assert_eq!(exact.owned_next, None);
    assert_eq!(exact.shared_next_cursor, None);

    // 4. 両方の位置は同時に渡せない。読めない位置・別の人の位置の形でない値は断る。
    let both = list_page(
        artifacts,
        data,
        &alice,
        ListPageRequest {
            kind: Some(ArtifactKind::Workflow),
            before: Some((owned_next.before_updated_at, owned_next.before_id)),
            limit: 2,
            shared_cursor: Some(&shared_cursor),
        },
    )
    .await;
    assert!(both.is_err());
    let broken = list_page(
        artifacts,
        data,
        &alice,
        ListPageRequest {
            kind: Some(ArtifactKind::Workflow),
            before: None,
            limit: 2,
            shared_cursor: Some(&format!("{shared_cursor}x")),
        },
    )
    .await;
    assert!(broken.is_err());
    // 別の種類の一覧（スキル・絞らない一覧）には、同じ位置を渡せない（その位置より新しいものを読み飛ばさない）。
    for other_kind in [Some(ArtifactKind::Skill), None] {
        let crossed = list_page(
            artifacts,
            data,
            &alice,
            ListPageRequest {
                kind: other_kind,
                before: None,
                limit: 2,
                shared_cursor: Some(&shared_cursor),
            },
        )
        .await;
        assert!(crossed.is_err(), "{other_kind:?}");
    }
    // 別のテナントでは、同じ位置を読めない。
    let other = ctx(&format!("{tenant}-other"), &alice.principal().id);
    let elsewhere = list_page(
        artifacts,
        data,
        &other,
        ListPageRequest {
            kind: Some(ArtifactKind::Workflow),
            before: None,
            limit: 2,
            shared_cursor: Some(&shared_cursor),
        },
    )
    .await;
    assert!(elsewhere.is_err());
}
