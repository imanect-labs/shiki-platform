//! ⑮ ListObjects の応答の上限（1,000 件）と行の読取の材料（BUG-161・PIT-18）。
//!
//! 個別共有の実効集合（`data_record` の viewer）と所属ロールの実効集合は、ListObjects の応答の上限で黙って切れる。
//! 上限を超えるタプルを実 OpenFGA に積み、
//! - 上限の外の個別共有（本人へ・部署へ）も、一覧・1 件の取得・SQL のどれでも見える（切り詰めの通知は立たない）。
//!   解除はすぐ効く。共有していない行は見えないまま
//! - 所属ロールの実効集合も上限に届いたら、部署への共有は見えない方向に倒して切り詰めを知らせ、`$user.roles` を
//!   使う行の決まりは fail-closed で断る
//!
//! を確かめる。各テストは自分の OpenFGA のストアに閉じ、最後にテナントの行とストアを消す（落ちても消す）。

use super::*;

use authz::visible::LIST_OBJECTS_MAX_RESULTS;
use storage::ShareTarget;

/// 上限を超えるタプルを直接書くための OpenFGA の口（Write API は 1 回 100 件まで）と、テストのストアの片付け。
struct Fga {
    http: reqwest::Client,
    base_url: String,
    store_id: String,
    model_id: String,
}

impl Fga {
    async fn write(&self, tuples: &[(String, &str, String)]) {
        for chunk in tuples.chunks(100) {
            let keys: Vec<serde_json::Value> = chunk
                .iter()
                .map(|(u, r, o)| json!({ "user": u, "relation": r, "object": o }))
                .collect();
            let resp = self
                .http
                .post(format!("{}/stores/{}/write", self.base_url, self.store_id))
                .json(&json!({
                    "writes": { "tuple_keys": keys },
                    "authorization_model_id": self.model_id,
                }))
                .send()
                .await
                .expect("OpenFGA write");
            assert!(
                resp.status().is_success(),
                "write: {}",
                resp.text().await.unwrap_or_default()
            );
        }
    }

    async fn delete_store(&self) {
        let resp = self
            .http
            .delete(format!("{}/stores/{}", self.base_url, self.store_id))
            .send()
            .await
            .expect("OpenFGA delete store");
        assert!(
            resp.status().is_success(),
            "delete store: {}",
            resp.status()
        );
    }
}

/// 自分のストアを作る（[`setup`] と同じ接続で、ストアを消す口も返す）。
async fn setup_own_store() -> Option<(PgPool, Arc<dyn AuthzClient>, Fga)> {
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
    let model: serde_json::Value = serde_json::from_str(include_str!(
        "../../../authz/model/authorization-model.json"
    ))
    .expect("model json");
    let client = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: base_url.clone(),
            store_name: format!("shiki-data-share-cap-{}", Uuid::new_v4()),
        },
        &model,
    )
    .await
    .expect("OpenFGA 接続");
    let fga = Fga {
        http: reqwest::Client::new(),
        base_url,
        store_id: client.store_id().to_string(),
        model_id: client.model_id().to_string(),
    };
    Some((pool, Arc::new(client), fga))
}

/// テストの本体を走らせ、落ちてもテナントの行とストアを消してから結果を返す。
async fn run_isolated<F, Fut>(body: F)
where
    F: FnOnce(Fixture, Arc<Fga>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let Some((pool, authz, fga)) = setup_own_store().await else {
        return;
    };
    let fga = Arc::new(fga);
    let f = fixture(pool.clone(), authz).await;
    let tenant = f.alice.tenant_id().to_string();
    let purge = DataStore::new(pool, Arc::clone(&f.authz), Arc::new(AllowRefs));
    let outcome = tokio::spawn(body(f, Arc::clone(&fga))).await;
    purge.purge_tenant(&tenant).await.expect("テナントの撤去");
    fga.delete_store().await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

/// bob の viewer の個別共有（DB に無いレコード）を `n` 件（上限を超えるための詰め物）。
fn filler_shares(f: &Fixture, n: usize) -> Vec<(String, &'static str, String)> {
    let ns = f.bob.ns();
    (0..n)
        .map(|_| {
            (
                f.bob.subject().to_string(),
                "viewer",
                ns.data_record(&Uuid::new_v4().to_string())
                    .as_str()
                    .to_string(),
            )
        })
        .collect()
}

/// alice が作る（bob には行の決まりで見えない）レコード。
async fn alice_record(f: &Fixture, memo: &str) -> Uuid {
    f.store
        .create_record(
            &f.alice,
            f.table_id,
            json!({"applicant": "alice", "amount": 300, "status": "draft", "memo": memo}),
            None,
        )
        .await
        .unwrap()
        .id
}

async fn share(f: &Fixture, rec: Uuid, target: ShareTarget) {
    f.store
        .share_record(
            &f.alice,
            f.table_id,
            rec,
            &target,
            RecordShareRole::Viewer,
            None,
        )
        .await
        .expect("share");
}

/// bob に見える行の id（一覧の全ページ）。
async fn bob_sees(f: &Fixture) -> std::collections::HashSet<Uuid> {
    ir_rows(&f.store, &f.bob, f.table_id, &["amount".to_string()])
        .await
        .into_iter()
        .map(|(id, _)| Uuid::parse_str(&id).unwrap())
        .collect()
}

#[tokio::test]
async fn record_shares_beyond_the_list_objects_cap_stay_visible() {
    run_isolated(|f, fga| async move {
        // 詰め物を先に積み、本物の共有は後から張る（ListObjects が返す 1,000 件に入らない位置）。
        fga.write(&filler_shares(&f, LIST_OBJECTS_MAX_RESULTS + 100))
            .await;
        let direct: Vec<Uuid> = {
            let mut v = Vec::new();
            for i in 0..5 {
                v.push(alice_record(&f, &format!("本人へ-{i}")).await);
            }
            v
        };
        for rec in &direct {
            share(&f, *rec, ShareTarget::User { id: "bob".into() }).await;
        }
        let via_role = alice_record(&f, "部署へ").await;
        join_role(&f.authz, &f.bob, "sales").await;
        share(&f, via_role, ShareTarget::Role { id: "sales".into() }).await;
        let not_shared = alice_record(&f, "共有しない").await;

        let page = f
            .store
            .list_records(&f.bob, f.table_id, &ListRecordsOptions::default(), None)
            .await
            .unwrap();
        assert!(!page.shares_truncated, "上限の外の共有も読み直せている");
        let seen = bob_sees(&f).await;
        for rec in direct.iter().chain([&via_role]) {
            assert!(seen.contains(rec), "上限の外の共有 {rec} が一覧に出る");
            f.store
                .get_record(&f.bob, f.table_id, *rec, None)
                .await
                .unwrap_or_else(|e| panic!("上限の外の共有 {rec} を開ける: {e:?}"));
        }
        assert!(seen.contains(&f.bob_rec), "本人の行");
        for hidden in [not_shared, f.alice_rec] {
            assert!(!seen.contains(&hidden), "共有していない行は見えない");
            assert!(matches!(
                f.store.get_record(&f.bob, f.table_id, hidden, None).await,
                Err(DataError::NotFound)
            ));
        }
        assert_eq!(seen.len(), direct.len() + 2);
        // SQL でも同じ行が見え、IR と食い違わない。
        let rows = sql_rows(&f.store, &f.bob, "SELECT _id FROM expense").await;
        let by_sql: std::collections::HashSet<Uuid> = rows
            .iter()
            .map(|r| Uuid::parse_str(r[0].as_str().unwrap()).unwrap())
            .collect();
        assert_eq!(by_sql, seen);
        sql_mirror(&f.store, &f.bob, f.table_id, "expense").await;

        // 解除は次の読取からすぐ効く（読み直しの経路でも）。
        f.store
            .unshare_record(
                &f.alice,
                f.table_id,
                direct[0],
                &ShareTarget::User { id: "bob".into() },
                RecordShareRole::Viewer,
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            f.store
                .get_record(&f.bob, f.table_id, direct[0], None)
                .await,
            Err(DataError::NotFound)
        ));
        assert!(!bob_sees(&f).await.contains(&direct[0]));
    })
    .await;
}

/// `$user.roles`（配下込み）で部署の行を読む表。
fn dept_schema() -> TableSchema {
    let mut dept = field("dept", FieldType::RoleRef);
    dept.indexed = true;
    let mut schema = plain(vec![dept, field("memo", FieldType::Text)]);
    schema.row_policy = Some(RowPolicy {
        read: PolicyExpr::FieldCmp {
            field: "dept".into(),
            op: CmpOp::In,
            value: PolicyOperand::UserRoles { subtree: true },
        },
        write: None,
    });
    schema
}

#[tokio::test]
async fn role_sets_at_the_list_objects_cap_fail_closed() {
    run_isolated(|f, fga| async move {
        fga.write(&filler_shares(&f, LIST_OBJECTS_MAX_RESULTS + 10))
            .await;
        let direct = alice_record(&f, "本人へ").await;
        share(&f, direct, ShareTarget::User { id: "bob".into() }).await;
        // bob の所属ロールをちょうど上限まで積む（これまでは `> 1000` の検査が掛からず、黙って切れた）。
        let ns = f.bob.ns();
        let roles: Vec<(String, &str, String)> = (0..LIST_OBJECTS_MAX_RESULTS)
            .map(|i| {
                (
                    f.bob.subject().to_string(),
                    "member",
                    ns.role(&format!("r{i}")).as_str().to_string(),
                )
            })
            .collect();
        fga.write(&roles).await;

        let page = f
            .store
            .list_records(&f.bob, f.table_id, &ListRecordsOptions::default(), None)
            .await
            .unwrap();
        assert!(
            page.shares_truncated,
            "読み直せない部署の共有があるかもしれないことを知らせる"
        );
        let seen = bob_sees(&f).await;
        assert!(seen.contains(&direct), "本人への共有は読み直せる");
        assert!(!seen.contains(&f.alice_rec));

        // 部署の集合そのものを使う行の決まりは、上限ちょうどで断る（見えすぎ・見えなさすぎの不定を作らない）。
        let t = f
            .store
            .create_table(
                &f.alice,
                NewDataTable {
                    name: "dept".into(),
                    schema: dept_schema(),
                },
                None,
            )
            .await
            .unwrap();
        f.authz_grant_editor(&f.bob, t.id).await;
        let r = f
            .store
            .list_records(&f.bob, t.id, &ListRecordsOptions::default(), None)
            .await;
        assert!(matches!(r, Err(DataError::Invalid(_))), "{r:?}");
    })
    .await;
}
