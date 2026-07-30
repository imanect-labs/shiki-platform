//! first-party skill の**自動掲載**（install 不要）の結合テスト（#387）。
//!
//! 実 Postgres ＋ 実 OpenFGA が必要（未設定なら early-return でスキップ）。掲載だけを増やしても
//! 実行時に読めなければ意味がないため、**掲載（クエリ）と読取（ReBAC）を同じテストで**確かめる:
//!
//! 1. 署名 publish（first_party）で `organization#member → viewer` が張られる
//! 2. **一度も install していない別ユーザー**（org メンバー）のカタログに載る
//! 3. その別ユーザーが artifact チョークポイント経由で **body を読める**
//!    （`chat::AppliedSkill::resolve` と同じ経路。ここが通らないと run が fail-closed で落ちる）
//! 4. 別 org のメンバーには載らず、読めない（テナント/組織境界）
//! 5. yank すると掲載から消える（新規利用を止める意味を掲載側にも効かせる）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::pedantic
)]

use std::sync::Arc;

use app_platform::{
    registry_signing_digest, sign_digest, value_digest, Registry, SkillInstallService,
    TrustedKeyStore,
};
use artifact::{ArtifactKind, ArtifactStore, NewArtifact};
use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, AuthzClient, Principal, Relation,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use uuid::Uuid;

struct Env {
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    artifacts: Arc<ArtifactStore>,
    installs: SkillInstallService,
    keys: TrustedKeyStore,
}

async fn setup() -> Option<Env> {
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL")
        .ok()
        .or_else(|| {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            None
        })?;
    let fga_url = std::env::var("OPENFGA_TEST_URL").ok().or_else(|| {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        None
    })?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("pg");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url,
            store_name: format!("skill-catalog-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);
    let artifacts = Arc::new(ArtifactStore::new(pool.clone(), authz.clone()));
    let installs = SkillInstallService::new(
        pool.clone(),
        Registry::new(pool.clone()),
        TrustedKeyStore::new(pool.clone()),
        artifacts.clone(),
        authz.clone(),
    );
    Some(Env {
        keys: TrustedKeyStore::new(pool.clone()),
        pool,
        authz,
        artifacts,
        installs,
    })
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

/// skill body（検証を通る最小形＋コマンド宣言）。
fn skill_body(command: &str) -> serde_json::Value {
    serde_json::json!({
        "description": "公式スキル（テスト）",
        "instructions": "手順書。",
        "command": {
            "name": command,
            "variants": [{ "args": "", "summary": "既定" }]
        }
    })
}

/// org メンバーシップを張る（`organization#member` が効く前提を作る）。
async fn seed_member(env: &Env, c: &AuthContext) {
    env.authz
        .write_tuple(&c.subject(), Relation::Member, &c.ns().organization(&c.org))
        .await
        .expect("org member tuple");
}

#[tokio::test]
async fn first_party_skill_is_listed_and_readable_without_install() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let publisher = ctx(&tenant, "acme", "alice");
    let member = ctx(&tenant, "acme", "bob");
    let outsider = ctx(&tenant, "other-org", "carol");
    for c in [&publisher, &member, &outsider] {
        seed_member(&env, c).await;
    }

    // alice が skill を作って first_party として署名 publish する。
    let name = format!("deep-research-{}", Uuid::new_v4().simple());
    let body = skill_body("deep-research");
    let created = env
        .artifacts
        .create(
            &publisher,
            NewArtifact {
                kind: ArtifactKind::Skill,
                name: name.clone(),
                body: body.clone(),
            },
            None,
        )
        .await
        .expect("skill 作成");

    let secret = [7u8; 32];
    let public = {
        use ed25519_dalek::SigningKey;
        SigningKey::from_bytes(&secret).verifying_key().to_bytes()
    };
    env.keys
        .add(&publisher, "official", &public, Some("test"))
        .await
        .expect("信頼鍵登録");
    // 署名対象は name/version/body-digest に束縛される（別名での replay を防ぐ）。
    let signature = sign_digest(
        &registry_signing_digest(&name, "1", &value_digest(&body)),
        &secret,
    )
    .expect("署名");
    let entry = env
        .installs
        .publish(
            &publisher,
            created.id,
            Some("1"),
            "first_party",
            Some(&signature),
            None,
        )
        .await
        .expect("first_party publish");

    // ── ① 一度も install していない org メンバーのカタログに載る ──
    let listed = env
        .installs
        .list_first_party_summaries(&member)
        .await
        .expect("掲載一覧");
    let hit = listed
        .iter()
        .find(|s| s.skill_id == created.id)
        .expect("install なしで掲載される");
    assert_eq!(hit.trust_tier, "first_party");
    assert_eq!(hit.description, "公式スキル（テスト）");
    assert_eq!(
        hit.command
            .as_ref()
            .and_then(|c| c["name"].as_str())
            .unwrap_or_default(),
        "deep-research",
        "コマンド宣言が載る（スラッシュ補完の材料）"
    );

    // ── ② その org メンバーが body を読める（run の解決経路と同一） ──
    env.artifacts
        .get(&member, created.id, None)
        .await
        .expect("organization#member 経由で読める（これが無いと run が fail-closed）");
    env.artifacts
        .get_version(&member, created.id, hit.skill_version, None)
        .await
        .expect("版の body も読める");

    // ── ③ 別 org のメンバーには載らず、読めない ──
    let outsider_listed = env
        .installs
        .list_first_party_summaries(&outsider)
        .await
        .expect("別 org の掲載一覧");
    assert!(
        !outsider_listed.iter().any(|s| s.skill_id == created.id),
        "別 org には掲載しない"
    );
    assert!(
        env.artifacts
            .get(&outsider, created.id, None)
            .await
            .is_err(),
        "別 org のメンバーは読めない（組織境界）"
    );

    // ── ④ yank すると掲載から消える ──
    Registry::new(env.pool.clone())
        .yank(&publisher, entry.id)
        .await
        .expect("yank");
    let after_yank = env
        .installs
        .list_first_party_summaries(&member)
        .await
        .expect("yank 後の掲載一覧");
    assert!(
        !after_yank.iter().any(|s| s.skill_id == created.id),
        "yank 済みは掲載しない"
    );
}

/// in_house の publish では org 全員向けタプルを張らない（自動掲載は first_party に限る）。
#[tokio::test]
async fn in_house_publish_does_not_grant_org_wide_read() {
    let Some(env) = setup().await else { return };
    let tenant = format!("t-{}", Uuid::new_v4().simple());
    let publisher = ctx(&tenant, "acme", "alice");
    let member = ctx(&tenant, "acme", "bob");
    seed_member(&env, &publisher).await;
    seed_member(&env, &member).await;

    let name = format!("in-house-{}", Uuid::new_v4().simple());
    let created = env
        .artifacts
        .create(
            &publisher,
            NewArtifact {
                kind: ArtifactKind::Skill,
                name: name.clone(),
                body: skill_body("in-house-skill"),
            },
            None,
        )
        .await
        .expect("skill 作成");
    env.installs
        .publish(&publisher, created.id, Some("1"), "in_house", None, None)
        .await
        .expect("in_house publish");

    assert!(
        env.installs
            .list_first_party_summaries(&member)
            .await
            .expect("掲載一覧")
            .iter()
            .all(|s| s.skill_id != created.id),
        "in_house は自動掲載しない（従来どおり install が必要）"
    );
    assert!(
        env.artifacts.get(&member, created.id, None).await.is_err(),
        "in_house publish で org 全員が読めるようにはならない"
    );
}
