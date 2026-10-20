//! AI 活用分析（#751）の結合テスト: ロールアップ・集計・少人数の秘匿・個人別の既定オフ・評価。
//!
//! `STORAGE_TEST_DATABASE_URL` がある時のみ実行（未設定なら early-return skip）。会話・評価は
//! 実 `ChatStore` で作り、会計（`llm_usage`）・監査（`audit_log`）は記録済みの行として入れる。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::sync::Arc;

use api::insights::{report::ReportError, rollup, settings::InsightSettingsPatch};
use api::insights::{InsightsService, Range};
use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, AuthzError, Consistency, FgaObject, ObjectType, Principal,
    ReadTupleKey, Relation, Subject,
};
use chat::ChatStore;
use chrono::{Duration, NaiveDate, Utc};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::directory::DirectoryStore;

struct AllowAll;

#[async_trait]
impl AuthzClient for AllowAll {
    async fn check(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
        _c: Consistency,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn write_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
    }
    async fn delete_tuple(
        &self,
        _s: &Subject,
        _r: Relation,
        _o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        Ok(true)
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
        .expect("connect test DB");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
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

fn today_jst() -> NaiveDate {
    (Utc::now() + Duration::hours(9)).date_naive()
}

fn range(dept: Option<&str>) -> Range {
    Range {
        from: today_jst(),
        to: today_jst(),
        dept: dept.map(str::to_string),
    }
}

/// 7 人（eng 6 人・solo 1 人）のうち 4 人が今日使った組織を作る。
struct Fixture {
    tenant: String,
    eng: String,
    solo: String,
    /// u0 の 1 本目（完了・文書検索と文書カードつき・用途 writing・👍）。
    thread_done: uuid::Uuid,
}

async fn seed(pool: &PgPool) -> Fixture {
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let eng = format!("eng-{}", uuid::Uuid::new_v4());
    let solo = format!("solo-{}", uuid::Uuid::new_v4());
    let dir = DirectoryStore::new(pool.clone());
    dir.upsert_role(&eng, &tenant, "acme", "/acme/eng")
        .await
        .unwrap();
    dir.upsert_role(&solo, &tenant, "acme", "/acme/solo")
        .await
        .unwrap();
    for i in 0..7 {
        let u = format!("u{i}");
        dir.upsert_user(&u, &tenant, "acme", &format!("{u}@example.com"), &u)
            .await
            .unwrap();
        let role = if i < 6 { &eng } else { &solo };
        dir.replace_user_roles(&tenant, "acme", &u, &[(role.clone(), true)])
            .await
            .unwrap();
    }

    let chat = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .expect("chat store");
    let mut thread_done = None;
    for (user, threads) in [("u0", 2), ("u1", 1), ("u2", 1), ("u3", 1)] {
        let c = ctx(&tenant, user);
        for n in 0..threads {
            let t = chat
                .create_thread(&c, "会話", false, None, None)
                .await
                .unwrap();
            let posted = chat
                .post_message(
                    &c,
                    t.id,
                    "議事録を書いて",
                    &[],
                    None,
                    Some(false),
                    false,
                    &[],
                    None,
                )
                .await
                .unwrap();
            let status = if user == "u1" { "failed" } else { "done" };
            sqlx::query("UPDATE generation_run SET status = $2 WHERE run_id = $1")
                .bind(posted.run_id)
                .bind(status)
                .execute(pool)
                .await
                .unwrap();
            if user == "u0" && n == 0 {
                sqlx::query("UPDATE message SET content = $2 WHERE id = $1")
                    .bind(posted.assistant_message_id)
                    .bind(serde_json::json!([
                        {"type": "tool_call", "id": "c1", "name": "doc_search", "args": {}},
                        {"type": "document_ref", "node_id": uuid::Uuid::new_v4(), "title": "議事録"},
                        {"type": "text", "text": "書きました"}
                    ]))
                    .execute(pool)
                    .await
                    .unwrap();
                chat.set_feedback(&c, t.id, posted.assistant_message_id, 1, None, None)
                    .await
                    .unwrap();
                thread_done = Some(t.id);
            }
            if user == "u1" {
                chat.set_feedback(
                    &c,
                    t.id,
                    posted.assistant_message_id,
                    -1,
                    Some("not_found"),
                    None,
                )
                .await
                .unwrap();
            }
        }
    }
    // 会計: u0 の呼び出しと、利用者に数えない分類器の呼び出し。
    // ワークフロー（予約起動は主体がワークフロー ID）の呼び出しも、利用者には数えない。
    for (key, user) in [
        ("k-u0", "u0"),
        ("k-sys", "system:insights"),
        ("k-wf", "6f1c1a52-0000-4000-8000-000000000751"),
    ] {
        sqlx::query(
            "INSERT INTO llm_usage (tenant_id, org, idempotency_key, provider, model, \
                                    prompt_tokens, completion_tokens, cost_usd_micros, user_sub, \
                                    source) \
             VALUES ($1, 'acme', $2, 'stub', 'm', 100, 50, 1000, $3, 'chat')",
        )
        .bind(&tenant)
        .bind(key)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    }
    // 監査: u2 のスキル適用（allow）と、数えない deny。
    for decision in ["allow", "deny"] {
        sqlx::query(
            "INSERT INTO audit_log (tenant_id, org, actor, action, object_type, object_id, \
                                    decision) \
             VALUES ($1, 'acme', 'u2', 'skill.apply', 'artifact', 'x', $2)",
        )
        .bind(&tenant)
        .bind(decision)
        .execute(pool)
        .await
        .unwrap();
    }
    // roles claim 由来のロール（部署ではない）。6 人いても絞り込みには使えない。
    for i in 0..6 {
        sqlx::query(
            "INSERT INTO directory_user_role (tenant_id, org, user_id, role_id, source) \
             VALUES ($1, 'acme', $2, 'admins', 'role')",
        )
        .bind(&tenant)
        .bind(format!("u{i}"))
        .execute(pool)
        .await
        .unwrap();
    }
    Fixture {
        tenant,
        eng,
        solo,
        thread_done: thread_done.unwrap(),
    }
}

/// 1 日分を作り直す: 人ごとの数が材料と合い、何度流しても同じになり、system 主体は数えない。
#[tokio::test]
async fn rollup_counts_per_user_and_is_idempotent() {
    let Some(pool) = setup().await else { return };
    let f = seed(&pool).await;
    let day = today_jst();
    let n = rollup::rollup_day(&pool, &f.tenant, "acme", day)
        .await
        .unwrap();
    assert_eq!(n, 4, "使った 4 人だけ（分類器・ワークフローの主体は除く）");
    let again = rollup::rollup_day(&pool, &f.tenant, "acme", day)
        .await
        .unwrap();
    assert_eq!(again, 4, "流し直しても同じ行数");

    let row: (i32, i32, i32, i64, i32, i32, i32) = sqlx::query_as(
        "SELECT runs, threads, calls, tokens, doc_searches, documents, failed_runs \
         FROM ai_activity_daily WHERE tenant_id = $1 AND org = 'acme' AND day = $2 \
           AND user_id = 'u0'",
    )
    .bind(&f.tenant)
    .bind(day)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (2, 2, 1, 150, 1, 1, 0));
    let (failed, skills): (i32, i32) = sqlx::query_as(
        "SELECT sum(failed_runs)::int, sum(skill_uses)::int FROM ai_activity_daily \
         WHERE tenant_id = $1 AND day = $2",
    )
    .bind(&f.tenant)
    .bind(day)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failed, 1, "u1 の失敗");
    assert_eq!(skills, 1, "deny は数えない");
}

/// 管理画面の集計: 利用率・部署・満足度・効果。少人数の部署は伏せ、絞り込みも断る。
#[tokio::test]
async fn reports_respect_small_groups_and_individual_view() {
    let Some(pool) = setup().await else { return };
    let f = seed(&pool).await;
    rollup::rollup_day(&pool, &f.tenant, "acme", today_jst())
        .await
        .unwrap();
    let svc = InsightsService::new(pool.clone());
    let admin = ctx(&f.tenant, "admin");

    // 用途分類をオンにし、u0 の完了した会話に用途を付ける（分類器の結果の代わり）。
    let patch = InsightSettingsPatch {
        classify_enabled: Some(true),
        hourly_wage_jpy: Some(Some(3000)),
        ..Default::default()
    };
    svc.update_settings(&admin, &patch).await.unwrap().unwrap();
    sqlx::query(
        "INSERT INTO thread_insight (tenant_id, org, thread_id, owner, category, messages_seen) \
         VALUES ($1, 'acme', $2, 'u0', 'writing', 1)",
    )
    .bind(&f.tenant)
    .bind(f.thread_done)
    .execute(&pool)
    .await
    .unwrap();

    let o = svc
        .overview(&admin, &range(None), Some(15_000))
        .await
        .unwrap();
    assert_eq!(o.population, 7);
    assert_eq!(o.active_users, 4);
    assert_eq!(o.ratings, 2);
    assert_eq!(o.satisfaction, Some(0.5));
    let eng = o.departments.iter().find(|d| d.role_id == f.eng).unwrap();
    assert_eq!(eng.members, 6);
    assert!((eng.rate.unwrap() - 4.0 / 6.0).abs() < 1e-9);
    let solo = o.departments.iter().find(|d| d.role_id == f.solo);
    assert!(solo.is_none_or(|d| d.rate.is_none()), "1 人の部署は伏せる");

    // 部署まるごとの絞り込み: 5 人以上は通り、少人数は断る。
    let o = svc
        .overview(&admin, &range(Some(&f.eng)), None)
        .await
        .unwrap();
    assert_eq!(o.population, 6);
    assert!(matches!(
        svc.overview(&admin, &range(Some(&f.solo)), None).await,
        Err(ReportError::GroupTooSmall)
    ));

    assert!(
        matches!(
            svc.overview(&admin, &range(Some("admins")), None).await,
            Err(ReportError::GroupTooSmall)
        ),
        "部署でないロールでは絞れない"
    );

    // 個人別は既定オフ。オンにすると使った人が並ぶ。
    assert!(svc.people(&admin, &range(None)).await.unwrap().is_none());
    let a = svc.adoption(&admin, &range(None)).await.unwrap();
    assert!(!a.individual_view);
    assert_eq!(a.segments.population, 7);
    let patch = InsightSettingsPatch {
        individual_view: Some(true),
        ..Default::default()
    };
    svc.update_settings(&admin, &patch).await.unwrap().unwrap();
    let people = svc.people(&admin, &range(None)).await.unwrap().unwrap();
    assert_eq!(people.len(), 4);
    let u0 = people.iter().find(|p| p.user_id == "u0").unwrap();
    assert_eq!((u0.runs, u0.threads), (2, 2));

    // 用途と効果: 完了し 👎 の付いていない会話 × 前提分数。👎 の理由が数えられる。
    let u = svc.use_cases(&admin, &range(None)).await.unwrap();
    assert_eq!(u.total_threads, 1);
    assert_eq!(
        u.categories
            .iter()
            .find(|c| c.key == "writing")
            .unwrap()
            .threads,
        1
    );
    let doc = u.features.iter().find(|x| x.key == "doc_search").unwrap();
    assert_eq!((doc.users, doc.count), (1, 1));
    let i = svc
        .impact(&admin, &range(None), Some(15_000))
        .await
        .unwrap();
    let writing = i.savings.iter().find(|r| r.key == "writing").unwrap();
    assert_eq!(writing.completed, 1);
    assert!((i.total_hours - 0.25).abs() < 1e-9, "15 分 = 0.25 時間");
    assert_eq!(i.yen_saved, Some(750));
    assert_eq!(
        i.reasons
            .iter()
            .find(|r| r.key == "not_found")
            .unwrap()
            .count,
        1
    );
    assert_eq!(i.outputs.documents, 1);
    assert_eq!(
        i.cost_usd_micros, 3000,
        "利用料は利用量画面と同じ会計（分類器・ワークフローの分も含む）"
    );

    // 部署の一覧（絞り込みの選択肢）。
    let depts = svc.departments(&admin).await.unwrap();
    assert!(depts.iter().any(|(id, _, n)| *id == f.eng && *n == 6));

    // 別テナントからは何も見えない。
    let other = ctx(&format!("t-{}", uuid::Uuid::new_v4()), "admin");
    let o = svc.overview(&other, &range(None), None).await.unwrap();
    assert_eq!((o.population, o.active_users, o.ratings), (0, 0, 0));
}

/// 設定の検証（範囲外は弾き、保存済みの値は変えない）。
#[tokio::test]
async fn settings_reject_out_of_range_values() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let svc = InsightsService::new(pool.clone());
    let admin = ctx(&tenant, "admin");
    let s = svc.settings(&admin).await.unwrap();
    assert!(
        !s.classify_enabled && !s.individual_view,
        "既定はどちらもオフ"
    );
    let bad = InsightSettingsPatch {
        hourly_wage_jpy: Some(Some(0)),
        ..Default::default()
    };
    assert!(svc.update_settings(&admin, &bad).await.unwrap().is_err());
    let mut minutes = std::collections::BTreeMap::new();
    minutes.insert("unknown".to_string(), 10);
    let bad = InsightSettingsPatch {
        minutes_per_task: minutes,
        ..Default::default()
    };
    assert!(svc.update_settings(&admin, &bad).await.unwrap().is_err());
    let mut minutes = std::collections::BTreeMap::new();
    minutes.insert("writing".to_string(), 30);
    let ok = InsightSettingsPatch {
        minutes_per_task: minutes,
        workflow_minutes: Some(5),
        ..Default::default()
    };
    let s = svc.update_settings(&admin, &ok).await.unwrap().unwrap();
    assert_eq!(s.minutes_for("writing"), 30);
    assert_eq!(s.minutes_for("summarize"), 12, "未指定は既定のまま");
    assert_eq!(svc.settings(&admin).await.unwrap().workflow_minutes, 5);
}

/// 評価: 👍 に理由は付けられず、付け直しは上書き、取り消しで消える。他人の評価は見えない。
#[tokio::test]
async fn feedback_upsert_and_clear() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let chat = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    let c = ctx(&tenant, "u0");
    let t = chat
        .create_thread(&c, "会話", false, None, None)
        .await
        .unwrap();
    let posted = chat
        .post_message(&c, t.id, "質問", &[], None, Some(false), false, &[], None)
        .await
        .unwrap();
    let mid = posted.assistant_message_id;
    assert!(chat
        .set_feedback(&c, t.id, mid, 1, Some("slow"), None)
        .await
        .is_err());
    assert!(
        chat.set_feedback(&c, t.id, posted.user_message_id, 1, None, None)
            .await
            .is_err(),
        "利用者の発話には付けられない"
    );
    chat.set_feedback(&c, t.id, mid, 1, None, None)
        .await
        .unwrap();
    chat.set_feedback(&c, t.id, mid, -1, Some("slow"), None)
        .await
        .unwrap();
    let mine = chat.my_feedback(&c, t.id, None).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].rating, -1);
    assert_eq!(mine[0].category.as_deref(), Some("slow"));
    let other = ctx(&tenant, "u1");
    assert!(chat
        .my_feedback(&other, t.id, None)
        .await
        .unwrap()
        .is_empty());
    chat.clear_feedback(&c, t.id, mid, None).await.unwrap();
    assert!(chat.my_feedback(&c, t.id, None).await.unwrap().is_empty());
}

/// stub プロバイダの llm-gateway（決定的応答・会計 0 円）。
fn stub_gateway(pool: PgPool) -> llm_gateway::LlmGateway {
    use llm_gateway::{GatewayConfig, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind};
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
            ..Default::default()
        },
        catalog: ModelCatalog {
            default_model: "m".into(),
            models: vec![ModelEntry {
                id: "m".into(),
                ..Default::default()
            }],
        },
        ..Default::default()
    };
    llm_gateway::LlmGateway::build(pool, reqwest::Client::new(), config).expect("gateway")
}

/// 用途分類: 発話の抜粋だけを AI に渡してラベルを保存し、分類器の利用は利用者に数えない。
/// 分類済みの会話は（発話が増えるまで）分類し直さない。
#[tokio::test]
async fn classify_labels_threads_without_counting_the_classifier() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let chat = ChatStore::connect(pool.clone(), Arc::new(AllowAll), None)
        .await
        .unwrap();
    // 集計は在籍者だけを数えるので、ディレクトリに登録しておく。
    DirectoryStore::new(pool.clone())
        .upsert_user("u0", &tenant, "acme", "u0@example.com", "u0")
        .await
        .unwrap();
    let c = ctx(&tenant, "u0");
    let t = chat
        .create_thread(&c, "会話", false, None, None)
        .await
        .unwrap();
    let posted = chat
        .post_message(
            &c,
            t.id,
            "translation の依頼",
            &[],
            None,
            Some(false),
            false,
            &[],
            None,
        )
        .await
        .unwrap();
    // 応答が終わっていない会話は分類しない。
    let gw = stub_gateway(pool.clone());
    let none = api::insights::classify::classify_org(&pool, &gw, &tenant, "acme", 10, Some("m"))
        .await
        .unwrap();
    assert_eq!(none, 0);
    sqlx::query("UPDATE generation_run SET status = 'done' WHERE run_id = $1")
        .bind(posted.run_id)
        .execute(&pool)
        .await
        .unwrap();
    let n = api::insights::classify::classify_org(&pool, &gw, &tenant, "acme", 10, Some("m"))
        .await
        .unwrap();
    assert_eq!(n, 1);
    let (category, owner): (String, String) = sqlx::query_as(
        "SELECT category, owner FROM thread_insight WHERE tenant_id = $1 AND thread_id = $2",
    )
    .bind(&tenant)
    .bind(t.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owner, "u0");
    assert!(
        api::insights::CATEGORIES
            .iter()
            .any(|(k, _, _)| *k == category),
        "既知のラベル: {category}"
    );
    let again = api::insights::classify::classify_org(&pool, &gw, &tenant, "acme", 10, Some("m"))
        .await
        .unwrap();
    assert_eq!(again, 0, "分類済みは繰り返さない");

    let classifier_calls: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM llm_usage WHERE tenant_id = $1 AND user_sub = 'system:insights'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(classifier_calls, 1, "分類器の利用は会計に刻む");
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'insights.classify'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audited, 1, "本文を AI に渡したことを監査に残す");

    // 定期集計の 1 回分: 今日まで作り、分類器は利用者に数えない。
    rollup::rollup_org(&pool, &tenant, "acme", today_jst())
        .await
        .unwrap();
    let users: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT user_id FROM ai_activity_daily WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(users, vec!["u0".to_string()]);
}

/// ログインした人をディレクトリに載せる: 新規だけ true、表示名は上書きしない。
/// 新しく載ったら、その組織の集計を遡ってやり直させる（到達点を消す）。
#[tokio::test]
async fn login_registers_users_and_requests_backfill() {
    let Some(pool) = setup().await else { return };
    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let dir = DirectoryStore::new(pool.clone());
    assert!(dir
        .ensure_user("n1", &tenant, "acme", Some("taro@example.com"))
        .await
        .unwrap());
    assert!(!dir
        .ensure_user("n1", &tenant, "acme", Some("taro@example.com"))
        .await
        .unwrap());
    // 管理者が付けた表示名は、ログインで仮の名前に戻さない（email の変更は反映する）。
    sqlx::query("UPDATE directory_user SET display_name = '山田 太郎' WHERE tenant_id = $1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!dir
        .ensure_user("n1", &tenant, "acme", Some("yamada@example.com"))
        .await
        .unwrap());
    let (email, name): (String, String) = sqlx::query_as(
        "SELECT email, display_name FROM directory_user WHERE tenant_id = $1 AND user_id = 'n1'",
    )
    .bind(&tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (email.as_str(), name.as_str()),
        ("yamada@example.com", "山田 太郎")
    );
    // email の無い IdP ユーザーも載る（sub を仮に使う）。
    assert!(dir.ensure_user("n2", &tenant, "acme", None).await.unwrap());

    let today = today_jst();
    rollup::rollup_org(&pool, &tenant, "acme", today)
        .await
        .unwrap();
    // 2 回目は定常（直近の作り直し＋あとから終わったワークフローの作成日の確認）。
    rollup::rollup_org(&pool, &tenant, "acme", today)
        .await
        .unwrap();
    rollup::request_backfill(&pool, &tenant, "acme")
        .await
        .unwrap();
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ai_insight_rollup_state WHERE tenant_id = $1")
            .bind(&tenant)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 0, "次の実行は初回と同じく遡る");
}
