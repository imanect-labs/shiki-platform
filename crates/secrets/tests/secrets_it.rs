//! シークレット管理の結合テスト（Task 10.9 受け入れ条件）。
//!
//! - 実 Postgres: 登録・ローテ・削除・一覧・**平文が DB/一覧/メタに現れない**（write-only）。
//! - 実 OpenFGA（併設時）: can_use を持たない実行主体の解決拒否・剥奪即時反映。

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
use secrets::{LocalKeyFileProvider, NewSecret, SecretError, SecretStore};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

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

/// タプルを記録し、can_use = 直接付与 or owner を再現する authz スタブ（grant 検証用）。
#[derive(Default)]
struct RecordingAuthz {
    tuples: std::sync::Mutex<std::collections::HashSet<(String, String, String)>>,
}

#[async_trait]
impl AuthzClient for RecordingAuthz {
    async fn check(
        &self,
        s: &Subject,
        rel: Relation,
        obj: &FgaObject,
        _: Consistency,
    ) -> Result<bool, AuthzError> {
        let t = self.tuples.lock().unwrap();
        let has = |r: &str| t.contains(&(s.to_string(), r.to_string(), obj.to_string()));
        // FGA モデル: can_use は直接付与 or owner。
        Ok(has(rel.as_str()) || (rel == Relation::CanUse && has("owner")))
    }
    async fn write_tuple(
        &self,
        s: &Subject,
        rel: Relation,
        obj: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.tuples.lock().unwrap().insert((
            s.to_string(),
            rel.as_str().to_string(),
            obj.to_string(),
        ));
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
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    Some(pool)
}

/// 全テナントを走査する一括再暗号化と「旧版行の状態」を観測するテストを直列化する。
/// nextest はテストを別プロセスで並列に走らせ DB を共有するため、一括再暗号化が他テストの
/// 旧版行を先に移行すると観測が崩れる。専用接続のセッション advisory lock で排他し、
/// 返した接続を drop（＝切断）した時点で解放する。
async fn legacy_scan_guard() -> sqlx::PgConnection {
    use sqlx::Connection;
    let db_url = std::env::var("STORAGE_TEST_DATABASE_URL").expect("setup 済み");
    let mut conn = sqlx::PgConnection::connect(&db_url)
        .await
        .expect("connect guard");
    sqlx::query("SELECT pg_advisory_lock(814)")
        .execute(&mut conn)
        .await
        .expect("advisory lock");
    conn
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

fn store(pool: PgPool, authz: Arc<dyn AuthzClient>) -> SecretStore {
    let provider = Arc::new(LocalKeyFileProvider::from_bytes([42u8; 32]));
    SecretStore::new(pool, authz, provider)
}

fn tenant() -> String {
    format!("t-{}", uuid::Uuid::new_v4())
}

#[tokio::test]
async fn create_resolve_and_plaintext_never_read_back() {
    let Some(pool) = setup().await else { return };
    let s = store(pool.clone(), Arc::new(AllowAll));
    let t = tenant();
    let c = ctx(&t, "alice");

    let meta = s
        .create(
            &c,
            NewSecret {
                name: "slack-token".into(),
                plaintext: b"xoxb-super-secret".to_vec(),
                allowed_hosts: vec!["api.slack.com".into()],
            },
            None,
        )
        .await
        .expect("create");
    assert_eq!(meta.name, "slack-token");
    assert_eq!(meta.allowed_hosts, vec!["api.slack.com".to_string()]);

    // メタ・一覧に平文が現れない（write-only）。
    let got = s.get_meta(&c, meta.id, None).await.expect("meta");
    let meta_json = serde_json::to_string(&got).unwrap();
    assert!(!meta_json.contains("xoxb"), "メタに平文が現れてはならない");
    let list = s.list_mine(&c).await.expect("list");
    assert!(!serde_json::to_string(&list).unwrap().contains("xoxb"));

    // DB を直接見ても平文列が無い（暗号文のみ）。
    let (ct, plaintext_col_exists): (Vec<u8>, Option<String>) = sqlx::query_as(
        "SELECT ciphertext, (SELECT string_agg(column_name, ',') FROM information_schema.columns \
         WHERE table_name = 'secret' AND column_name = 'plaintext') \
         FROM secret WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&t)
    .bind(meta.id)
    .fetch_one(&pool)
    .await
    .expect("row");
    assert!(
        plaintext_col_exists.is_none(),
        "secret に plaintext 列が存在してはならない"
    );
    assert!(
        !ct.windows(4).any(|w| w == b"xoxb"),
        "暗号文に平文が漏れてはならない"
    );

    // resolve は平文を返す（能力ゲートウェイ経路）。
    let resolved = s
        .resolve(&c, "slack-token", None, None)
        .await
        .expect("resolve");
    assert_eq!(resolved.plaintext, b"xoxb-super-secret");
    assert!(resolved.binding.allows("api.slack.com"));
    assert!(!resolved.binding.allows("evil.com"));

    // destination_host を渡すと復号前に宛先束縛を検証し、許可外は DestinationDenied（平文を出さない）。
    let allowed = s
        .resolve(&c, "slack-token", Some("api.slack.com"), None)
        .await
        .expect("allowed host resolves");
    assert_eq!(allowed.plaintext, b"xoxb-super-secret");
    assert!(matches!(
        s.resolve(&c, "slack-token", Some("evil.com"), None).await,
        Err(secrets::SecretError::DestinationDenied(_))
    ));
}

#[tokio::test]
async fn short_secret_rejected_at_write() {
    let Some(pool) = setup().await else { return };
    let s = store(pool, Arc::new(AllowAll));
    let t = tenant();
    let c = ctx(&t, "alice");
    // 自動レダクト不能な短い値（< MIN_LEN=4）は create で拒否される。
    let res = s
        .create(
            &c,
            NewSecret {
                name: "pin".into(),
                plaintext: b"12".to_vec(),
                allowed_hosts: vec![],
            },
            None,
        )
        .await;
    assert!(
        matches!(res, Err(secrets::SecretError::Invalid(_))),
        "{res:?}"
    );
}

#[tokio::test]
async fn rotate_changes_ciphertext_keeps_resolvable() {
    let Some(pool) = setup().await else { return };
    let s = store(pool, Arc::new(AllowAll));
    let t = tenant();
    let c = ctx(&t, "alice");
    let meta = s
        .create(
            &c,
            NewSecret {
                name: "api-key".into(),
                plaintext: b"v1-secret".to_vec(),
                allowed_hosts: vec![],
            },
            None,
        )
        .await
        .expect("create");
    let rotated = s
        .rotate(&c, meta.id, b"v2-secret".to_vec(), None)
        .await
        .expect("rotate");
    assert_eq!(rotated.version, 2);
    let resolved = s.resolve(&c, "api-key", None, None).await.expect("resolve");
    assert_eq!(resolved.plaintext, b"v2-secret");
}

#[tokio::test]
async fn invalid_binding_hosts_rejected() {
    let Some(pool) = setup().await else { return };
    let s = store(pool, Arc::new(AllowAll));
    let c = ctx(&tenant(), "alice");
    let bad = s
        .create(
            &c,
            NewSecret {
                name: "bad-binding".into(),
                plaintext: b"x".to_vec(),
                allowed_hosts: vec!["https://api.slack.com/webhook".into()],
            },
            None,
        )
        .await;
    assert!(matches!(bad, Err(SecretError::Invalid(_))), "{bad:?}");
}

/// can_use を持たない実行主体の解決が拒否される（live OpenFGA）。
#[tokio::test]
async fn resolve_denied_without_can_use() {
    let Some(pool) = setup().await else { return };
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return;
    };
    use authz::client::{OpenFgaClient, OpenFgaConfig};
    let model: serde_json::Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json")).unwrap();
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-secret-test-{}", uuid::Uuid::new_v4()),
    };
    let fga = Arc::new(
        OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
            .await
            .unwrap(),
    );
    let s = store(pool, fga.clone());
    let t = tenant();
    let alice = ctx(&t, "alice");
    let bob = ctx(&t, "bob");

    let meta = s
        .create(
            &alice,
            NewSecret {
                name: "shared-token".into(),
                plaintext: b"top-secret".to_vec(),
                allowed_hosts: vec!["api.slack.com".into()],
            },
            None,
        )
        .await
        .expect("create");

    // owner(alice) は解決できる。
    s.resolve(&alice, "shared-token", None, None)
        .await
        .expect("alice resolves");
    // bob は can_use が無いので解決拒否（Forbidden）。
    assert!(matches!(
        s.resolve(&bob, "shared-token", None, None).await,
        Err(SecretError::Forbidden)
    ));
    assert!(matches!(
        s.get_meta(&bob, meta.id, None).await,
        Err(SecretError::Forbidden)
    ));

    // can_use 付与 → bob が解決できる。
    let obj = alice.ns().secret(&meta.id.to_string());
    fga.write_tuple(&bob.subject(), Relation::CanUse, &obj)
        .await
        .unwrap();
    let resolved = s
        .resolve(&bob, "shared-token", None, None)
        .await
        .expect("bob resolves after grant");
    assert_eq!(resolved.plaintext, b"top-secret");

    // 剥奪 → 即時に解決不可（HigherConsistency）。
    fga.delete_tuple(&bob.subject(), Relation::CanUse, &obj)
        .await
        .unwrap();
    assert!(matches!(
        s.resolve(&bob, "shared-token", None, None).await,
        Err(SecretError::Forbidden)
    ));
}

/// grant_can_use: owner が別プリンシパル（miniapp）へ can_use を付与すると解決できる。
/// アプリ所有の資格情報（B2 client secret）を installer 以外・サービス起動で使うための授権（Task 9.12）。
#[tokio::test]
async fn grant_can_use_lets_miniapp_resolve() {
    let Some(pool) = setup().await else { return };
    let s = store(pool.clone(), Arc::new(RecordingAuthz::default()));
    let t = tenant();
    let owner = ctx(&t, "installer");

    let meta = s
        .create(
            &owner,
            NewSecret {
                name: "miniapp-b2-x".into(),
                plaintext: b"confidential-client-secret".to_vec(),
                allowed_hosts: vec!["kc.example".into()],
            },
            None,
        )
        .await
        .expect("create");

    // 付与前: アプリ（miniapp）identity は can_use を持たず解決不可。
    let app_ctx = AuthContext::for_miniapp(t.clone(), "acme".into(), "app-1");
    assert!(matches!(
        s.resolve(&app_ctx, "miniapp-b2-x", None, None).await,
        Err(SecretError::Forbidden)
    ));

    // owner が miniapp へ can_use を付与 → 解決できる。
    s.grant_can_use(&owner, meta.id, &app_ctx.subject(), None)
        .await
        .expect("grant");
    let resolved = s
        .resolve(&app_ctx, "miniapp-b2-x", None, None)
        .await
        .expect("resolve after grant");
    assert_eq!(resolved.plaintext, b"confidential-client-secret");

    // 無関係なユーザーは付与されず解決不可（越境しない）。
    let other = ctx(&t, "other-user");
    assert!(matches!(
        s.resolve(&other, "miniapp-b2-x", None, None).await,
        Err(SecretError::Forbidden)
    ));
}

// --- 暗号文の行束縛（AES-256-GCM の AAD・#814） ---

/// 暗号素材（平文の暗号文・nonce・包んだ DEK 一式・版）を `from` 行から `to` 行へ複製する
/// （DB 書込権限を持つ攻撃者の再現）。
async fn copy_cipher_material(pool: &PgPool, from: (&str, uuid::Uuid), to: (&str, uuid::Uuid)) {
    let done = sqlx::query(
        "UPDATE secret d SET ciphertext = s.ciphertext, nonce = s.nonce, \
         encrypted_dek = s.encrypted_dek, dek_nonce = s.dek_nonce, \
         key_provider = s.key_provider, enc_version = s.enc_version \
         FROM secret s WHERE s.tenant_id = $1 AND s.id = $2 AND d.tenant_id = $3 AND d.id = $4",
    )
    .bind(from.0)
    .bind(from.1)
    .bind(to.0)
    .bind(to.1)
    .execute(pool)
    .await
    .expect("copy");
    assert_eq!(done.rows_affected(), 1);
}

async fn create_named(s: &SecretStore, c: &AuthContext, name: &str, value: &[u8]) -> uuid::Uuid {
    s.create(
        c,
        NewSecret {
            name: name.into(),
            plaintext: value.to_vec(),
            allowed_hosts: vec![],
        },
        None,
    )
    .await
    .expect("create")
    .id
}

fn assert_decrypt_failed(res: Result<secrets::ResolvedSecret, SecretError>) {
    assert!(
        matches!(res, Err(SecretError::Crypto(secrets::CryptoError::Decrypt))),
        "複製・改竄された暗号文が復号できてはならない: {:?}",
        res.map(|r| r.name)
    );
}

async fn enc_state(pool: &PgPool, tenant: &str, id: uuid::Uuid) -> (i16, Vec<u8>, Vec<u8>) {
    sqlx::query_as("SELECT enc_version, ciphertext, encrypted_dek FROM secret WHERE tenant_id = $1 AND id = $2")
        .bind(tenant)
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("enc state")
}

/// #814 以前の書き方（平文を AAD なしで暗号化・enc_version 列を書かない＝既定 0）で行を作る。
async fn insert_legacy(
    pool: &PgPool,
    provider: &LocalKeyFileProvider,
    tenant: &str,
    name: &str,
    value: &[u8],
) -> uuid::Uuid {
    use aes_gcm::aead::{Aead, KeyInit};
    use rand::RngCore;
    use secrets::KeyProvider;
    let mut dek = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut dek);
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let cipher = aes_gcm::Aes256Gcm::new(aes_gcm::Key::<aes_gcm::Aes256Gcm>::from_slice(&dek));
    let ciphertext = cipher
        .encrypt(aes_gcm::Nonce::from_slice(&nonce), value)
        .expect("legacy encrypt");
    let wrapped = provider.wrap(&dek).await.expect("wrap");
    sqlx::query_scalar(
        "INSERT INTO secret (tenant_id, org, name, owner, ciphertext, nonce, encrypted_dek, \
         dek_nonce, key_provider) VALUES ($1, 'acme', $2, 'alice', $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(tenant)
    .bind(name)
    .bind(&ciphertext)
    .bind(nonce.as_slice())
    .bind(&wrapped.encrypted_dek)
    .bind(&wrapped.nonce)
    .bind(&wrapped.provider_id)
    .fetch_one(pool)
    .await
    .expect("insert legacy")
}

fn provider() -> LocalKeyFileProvider {
    // store() と同じマスターキー。
    LocalKeyFileProvider::from_bytes([42u8; 32])
}

/// 暗号素材を別テナント・同テナント別 secret の行へ複製しても復号できない（越境の負のテスト）。
#[tokio::test]
async fn ciphertext_copied_to_other_row_fails_to_decrypt() {
    let Some(pool) = setup().await else { return };
    let s = store(pool.clone(), Arc::new(AllowAll));
    let (ta, tb) = (tenant(), tenant());
    let (a, b) = (ctx(&ta, "alice"), ctx(&tb, "mallory"));
    let victim = create_named(&s, &a, "tok", b"victim-secret").await;
    let a_other = create_named(&s, &a, "tok2", b"other-secret").await;
    let attacker = create_named(&s, &b, "tok", b"attacker-own").await;
    assert_eq!(enc_state(&pool, &ta, victim).await.0, 1, "新規書込は現行版");

    // 別テナントの行へ複製 → 攻撃者テナントからの解決は認証タグ不一致で失敗する。
    copy_cipher_material(&pool, (&ta, victim), (&tb, attacker)).await;
    assert_decrypt_failed(s.resolve(&b, "tok", None, None).await);

    // 同一テナント内の別 secret の行へ複製しても失敗する（secret_id 束縛）。
    copy_cipher_material(&pool, (&ta, victim), (&ta, a_other)).await;
    assert_decrypt_failed(s.resolve(&a, "tok2", None, None).await);

    // 元の行は引き続き解決できる。
    let ok = s
        .resolve(&a, "tok", None, None)
        .await
        .expect("victim resolves");
    assert_eq!(ok.plaintext, b"victim-secret");
}

/// 行の org を書き換えて別 org へ移しても復号できない（org 束縛）。
#[tokio::test]
async fn ciphertext_moved_to_other_org_fails_to_decrypt() {
    let Some(pool) = setup().await else { return };
    let s = store(pool.clone(), Arc::new(AllowAll));
    let t = tenant();
    let id = create_named(&s, &ctx(&t, "alice"), "org-tok", b"acme-secret").await;
    sqlx::query("UPDATE secret SET org = 'evil-org' WHERE tenant_id = $1 AND id = $2")
        .bind(&t)
        .bind(id)
        .execute(&pool)
        .await
        .expect("move org");
    let evil = AuthContext::new(
        ctx(&t, "mallory").principal.clone(),
        "evil-org".into(),
        t.clone(),
    );
    assert_decrypt_failed(s.resolve(&evil, "org-tok", None, None).await);
}

/// 現行版の enc_version を旧版（AAD なし）や未知の値へ書き換える格下げは失敗する。
#[tokio::test]
async fn enc_version_downgrade_or_unknown_fails() {
    let Some(pool) = setup().await else { return };
    let s = store(pool.clone(), Arc::new(AllowAll));
    let t = tenant();
    let c = ctx(&t, "alice");
    let id = create_named(&s, &c, "ver-tok", b"bound-secret").await;
    for v in [0_i16, 2, 99] {
        sqlx::query("UPDATE secret SET enc_version = $3 WHERE tenant_id = $1 AND id = $2")
            .bind(&t)
            .bind(id)
            .bind(v)
            .execute(&pool)
            .await
            .expect("set version");
        assert_decrypt_failed(s.resolve(&c, "ver-tok", None, None).await);
    }
}

/// 旧版行は従来どおり解決でき、初回解決で現行版へ遅延再暗号化される（DEK は再利用）。
#[tokio::test]
async fn legacy_row_resolves_and_is_lazily_reencrypted() {
    let Some(pool) = setup().await else { return };
    let _guard = legacy_scan_guard().await;
    let s = store(pool.clone(), Arc::new(AllowAll));
    let (t, tb) = (tenant(), tenant());
    let c = ctx(&t, "alice");
    let id = insert_legacy(&pool, &provider(), &t, "legacy-tok", b"legacy-secret").await;
    let (v0, ct0, dek0) = enc_state(&pool, &t, id).await;
    assert_eq!(v0, 0, "migration 既定は旧版");

    let resolved = s
        .resolve(&c, "legacy-tok", None, None)
        .await
        .expect("legacy resolves");
    assert_eq!(resolved.plaintext, b"legacy-secret");
    let (v1, ct1, dek1) = enc_state(&pool, &t, id).await;
    assert_eq!(v1, 1, "初回解決で現行版へ移行する");
    assert_ne!(ct1, ct0);
    assert_eq!(dek1, dek0, "DEK は再利用（encrypted_dek は不変）");
    let again = s
        .resolve(&c, "legacy-tok", None, None)
        .await
        .expect("resolves after upgrade");
    assert_eq!(again.plaintext, b"legacy-secret");

    // 移行後の暗号文は行に束縛されている（別テナントへ複製しても開けない）。
    let attacker = create_named(&s, &ctx(&tb, "mallory"), "legacy-tok", b"attacker-own").await;
    copy_cipher_material(&pool, (&t, id), (&tb, attacker)).await;
    assert_decrypt_failed(
        s.resolve(&ctx(&tb, "mallory"), "legacy-tok", None, None)
            .await,
    );
}

/// 旧版行のローテーションは現行版で書き直す。
#[tokio::test]
async fn rotate_writes_current_version() {
    let Some(pool) = setup().await else { return };
    let s = store(pool.clone(), Arc::new(AllowAll));
    let t = tenant();
    let c = ctx(&t, "alice");
    let id = insert_legacy(&pool, &provider(), &t, "rot-legacy", b"old-value").await;
    s.rotate(&c, id, b"new-value".to_vec(), None)
        .await
        .expect("rotate");
    assert_eq!(enc_state(&pool, &t, id).await.0, 1);
    let r = s
        .resolve(&c, "rot-legacy", None, None)
        .await
        .expect("resolve");
    assert_eq!(r.plaintext, b"new-value");
}

/// 一括再暗号化（shiki-admin secrets-reencrypt）: 旧版行を現行版へ移し、復号できない行は残す。
#[tokio::test]
async fn bulk_reencrypt_upgrades_legacy_rows_and_keeps_broken_ones() {
    let Some(pool) = setup().await else { return };
    let _guard = legacy_scan_guard().await;
    let s = store(pool.clone(), Arc::new(AllowAll));
    let kp = provider();
    let t = tenant();
    let c = ctx(&t, "alice");
    let mut ids = Vec::new();
    for i in 0..3 {
        ids.push(
            insert_legacy(
                &pool,
                &kp,
                &t,
                &format!("bulk-{i}"),
                format!("bulk-secret-{i}").as_bytes(),
            )
            .await,
        );
    }
    // 改竄行（暗号文の 1 バイト反転）は復号できない → 書き換えず残す。
    let broken = insert_legacy(&pool, &kp, &t, "bulk-broken", b"broken-secret").await;
    sqlx::query(
        "UPDATE secret SET ciphertext = set_byte(ciphertext, 0, get_byte(ciphertext, 0) # 255) \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&t)
    .bind(broken)
    .execute(&pool)
    .await
    .expect("tamper");
    assert!(secrets::count_legacy_secrets(&pool).await.expect("count") >= 4);

    // batch=2 で複数ページを跨ぐ。並行テストの旧版行も対象になり得るので下限で検査する。
    let report = secrets::reencrypt_legacy_secrets(&pool, &kp, 2)
        .await
        .expect("reencrypt");
    assert!(report.scanned >= 4, "{report:?}");
    assert!(report.failed >= 1, "{report:?}");
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(enc_state(&pool, &t, *id).await.0, 1, "bulk-{i} は現行版");
        let r = s
            .resolve(&c, &format!("bulk-{i}"), None, None)
            .await
            .expect("resolve");
        assert_eq!(r.plaintext, format!("bulk-secret-{i}").as_bytes());
    }
    assert_eq!(
        enc_state(&pool, &t, broken).await.0,
        0,
        "復号できない行は変更しない"
    );

    // 再実行は冪等（自テナントの行は既に現行版・改竄行は再び failed）。
    let again = secrets::reencrypt_legacy_secrets(&pool, &kp, 100)
        .await
        .expect("again");
    assert!(again.failed >= 1, "{again:?}");
    assert_eq!(enc_state(&pool, &t, ids[0]).await.0, 1);

    // 後片付け（改竄行を残すと後続の実行で failed が積み上がる）。
    sqlx::query("DELETE FROM secret WHERE tenant_id = $1")
        .bind(&t)
        .execute(&pool)
        .await
        .expect("cleanup");
}
