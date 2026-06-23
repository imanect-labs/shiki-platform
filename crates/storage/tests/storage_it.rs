//! StorageService の結合テスト（実 Postgres + MinIO + OpenFGA が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行し、
//! 未設定なら early-return でスキップする（素の `cargo test` を壊さない）。CI の
//! coverage ジョブで postgres/minio/openfga を立てて実走する。
//!
//! 検証: 二相アップロード（presigned PUT→finalize）・content-addressing・org スコープ
//! dedup（PIT-14）・closure を保つ move（PIT-16）・rename/delete/restore・viewer 認可・
//! deny の監査記録・ハッシュチェーン監査ログ。

use std::{sync::Arc, time::Duration};

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, AuthzClient, FgaObject, Principal, Relation, Subject,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{
    content_address::sha256_hex, object_store::S3Config, Node, ObjectStore, S3ObjectStore,
    StorageError, StorageService,
};
use uuid::Uuid;

struct Ctx {
    service: StorageService,
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    http: reqwest::Client,
}

async fn setup() -> Option<Ctx> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(openfga_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let s3_endpoint = std::env::var("STORAGE_TEST_S3_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:9000".into());
    let access_key =
        std::env::var("STORAGE_TEST_S3_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".into());
    let secret_key =
        std::env::var("STORAGE_TEST_S3_SECRET_KEY").unwrap_or_else(|_| "minioadmin".into());

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");

    let http = reqwest::Client::new();
    let fga = OpenFgaClient::connect(
        http.clone(),
        &OpenFgaConfig {
            base_url: openfga_url,
            store_name: format!("shiki-storage-it-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続できること");
    let authz: Arc<dyn AuthzClient> = Arc::new(fga);

    let s3 = S3Config {
        internal_endpoint: s3_endpoint.clone(),
        public_endpoint: s3_endpoint,
        bucket: "shiki-it-blobs".into(),
        access_key,
        secret_key,
        region: "us-east-1".into(),
        presign_get_ttl_secs: 300,
        presign_put_ttl_secs: 900,
        cors_allowed_origins: vec![],
    };
    let store = Arc::new(S3ObjectStore::new(&s3));
    store.ensure_bucket().await.expect("バケット準備");

    let service = StorageService::new(
        pool.clone(),
        store,
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
    );
    Some(Ctx {
        service,
        pool,
        authz,
        http,
    })
}

fn make_ctx(org: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            id: uid.into(),
            email: None,
            groups: vec![],
            dept: None,
            tenant_id: None,
        },
        org.into(),
        "default".into(),
    )
}

/// declare → presigned PUT → finalize の一連を実行してノードを返す（所持証明込み）。
async fn upload(
    service: &StorageService,
    http: &reqwest::Client,
    ctx: &AuthContext,
    parent: Option<Uuid>,
    name: &str,
    content: &[u8],
) -> Result<Node, StorageError> {
    let sha = sha256_hex(content);
    let ticket = service
        .begin_upload(
            ctx,
            parent,
            name,
            "text/plain",
            &sha,
            content.len() as i64,
            None,
        )
        .await?;
    let resp = http
        .put(&ticket.upload_url)
        .body(content.to_vec())
        .send()
        .await
        .expect("presigned PUT");
    assert!(resp.status().is_success(), "PUT status: {}", resp.status());
    service.finalize_upload(ctx, ticket.upload_id, None).await
}

async fn blob_refcount(pool: &PgPool, org: &str, sha: &str) -> i64 {
    sqlx::query_scalar("SELECT refcount FROM blob WHERE org = $1 AND sha256 = $2")
        .bind(org)
        .bind(sha)
        .fetch_one(pool)
        .await
        .expect("blob 行")
}

async fn audit_count(pool: &PgPool, org: &str, action: &str, decision: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE org = $1 AND action = $2 AND decision = $3",
    )
    .bind(org)
    .bind(action)
    .bind(decision)
    .fetch_one(pool)
    .await
    .expect("audit count")
}

#[tokio::test]
async fn storage_end_to_end() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
    } = ctx;

    // org/ユーザーをテスト毎にユニーク化し、行を隔離する。
    let org = format!("itorg{}", Uuid::new_v4().simple());
    let uid = format!("ituser{}", Uuid::new_v4().simple());
    let actx = make_ctx(&org, &uid);

    // org メンバーとして seed（ルート直下アップロードの認可に必要）。
    authz
        .write_tuple(
            &Subject::user(&uid),
            Relation::Member,
            &FgaObject::organization(&org),
        )
        .await
        .expect("member tuple seed");

    let content = b"hello shiki storage";
    let sha = sha256_hex(content);
    let size = content.len() as i64;

    // --- 二相アップロード（declare → presigned PUT → finalize） ---
    let file = upload(&service, &http, &actx, None, "hello.txt", content)
        .await
        .expect("upload");
    assert_eq!(file.name, "hello.txt");
    assert_eq!(file.blob_sha256.as_deref(), Some(sha.as_str()));
    assert_eq!(file.size_bytes, Some(size));
    assert_eq!(blob_refcount(&pool, &org, &sha).await, 1);

    // --- メタ取得・ダウンロード（presigned GET でバイト一致） ---
    let meta = service
        .get_metadata(&actx, file.id, None)
        .await
        .expect("get_metadata");
    assert_eq!(meta.name, "hello.txt");

    let ticket = service
        .issue_download_url(&actx, file.id, None)
        .await
        .expect("download url");
    let got = http
        .get(&ticket.url)
        .send()
        .await
        .expect("presigned GET")
        .bytes()
        .await
        .expect("body");
    assert_eq!(got.as_ref(), content, "DL バイトが一致すること");

    // --- org スコープ dedup（同 org・同内容＝finalize 時に dedup・refcount 2） ---
    let file2 = upload(&service, &http, &actx, None, "copy.txt", content)
        .await
        .expect("dedup upload");
    assert_ne!(file2.id, file.id);
    assert_eq!(
        blob_refcount(&pool, &org, &sha).await,
        2,
        "同一内容は finalize で dedup され refcount が増える"
    );

    // 同一フォルダ内の同名（生存）への作成は finalize で Conflict（部分ユニーク制約）。
    let dup = upload(&service, &http, &actx, None, "copy.txt", content).await;
    assert!(matches!(dup, Err(StorageError::Conflict)), "{dup:?}");
    assert_eq!(
        blob_refcount(&pool, &org, &sha).await,
        2,
        "Conflict 時は refcount を増やさない（txn ロールバック）"
    );

    // --- 別 org では blob 名前空間が分かれ dedup されない（PIT-14） ---
    let org_b = format!("itorg{}", Uuid::new_v4().simple());
    let uid_b = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org_b, &uid_b);
    authz
        .write_tuple(
            &Subject::user(&uid_b),
            Relation::Member,
            &FgaObject::organization(&org_b),
        )
        .await
        .unwrap();
    upload(&service, &http, &bctx, None, "hello.txt", content)
        .await
        .expect("upload other org");
    assert_eq!(
        blob_refcount(&pool, &org_b, &sha).await,
        1,
        "別 org は独立した blob 行（refcount 1）"
    );
    assert_eq!(
        blob_refcount(&pool, &org, &sha).await,
        2,
        "元 org の refcount は別 org の影響を受けない"
    );

    // --- P2-4: presigned PUT は宣言サイズに束縛される（過少申告で巨大 PUT は弾かれる） ---
    {
        let ticket = service
            .begin_upload(
                &actx,
                None,
                "wrong-size.txt",
                "text/plain",
                &sha,
                size + 100,
                None,
            )
            .await
            .expect("begin_upload wrong size");
        // 署名は content-length=size+100 だが本文は size バイト → MinIO が拒否する。
        let resp = http
            .put(&ticket.upload_url)
            .body(content.to_vec())
            .send()
            .await
            .expect("PUT send");
        assert!(
            resp.status().is_client_error() || resp.status().is_server_error(),
            "サイズ不一致の PUT は拒否される: {}",
            resp.status()
        );
    }

    // --- P2-3: finalize は宣言した本人のみ（upload_id を知る別ユーザーは横取り不可） ---
    {
        let uid_c = format!("ituser{}", Uuid::new_v4().simple());
        let cctx = make_ctx(&org, &uid_c);
        authz
            .write_tuple(
                &Subject::user(&uid_c),
                Relation::Member,
                &FgaObject::organization(&org),
            )
            .await
            .unwrap();
        let other = b"steal me bytes";
        let other_sha = sha256_hex(other);
        let ticket = service
            .begin_upload(
                &actx,
                None,
                "secret.txt",
                "text/plain",
                &other_sha,
                other.len() as i64,
                None,
            )
            .await
            .expect("declare by actx");
        http.put(&ticket.upload_url)
            .body(other.to_vec())
            .send()
            .await
            .expect("PUT")
            .error_for_status()
            .expect("PUT ok");
        // 別ユーザー（uid_c）が finalize → created_by 不一致で NotFound。
        let stolen = service.finalize_upload(&cctx, ticket.upload_id, None).await;
        assert!(matches!(stolen, Err(StorageError::NotFound)), "{stolen:?}");
        // 本人なら finalize できる。
        service
            .finalize_upload(&actx, ticket.upload_id, None)
            .await
            .expect("owner finalize");
    }

    // --- P2-6: viewer 権限の無い同 org ユーザーには存在を秘匿（403 でなく NotFound） ---
    {
        let uid_d = format!("ituser{}", Uuid::new_v4().simple());
        let dctx = make_ctx(&org, &uid_d);
        authz
            .write_tuple(
                &Subject::user(&uid_d),
                Relation::Member,
                &FgaObject::organization(&org),
            )
            .await
            .unwrap();
        let hidden = service.get_metadata(&dctx, file.id, None).await;
        assert!(matches!(hidden, Err(StorageError::NotFound)), "{hidden:?}");
        let hidden_dl = service.issue_download_url(&dctx, file.id, None).await;
        assert!(
            matches!(hidden_dl, Err(StorageError::NotFound)),
            "{hidden_dl:?}"
        );
    }

    // --- move（フォルダを直接用意し、closure を検証） ---
    let folder_id: Uuid = sqlx::query_scalar(
        "INSERT INTO node (org, tenant_id, kind, name, created_by) \
         VALUES ($1, 'default', 'folder', 'myfolder', $2) RETURNING id",
    )
    .bind(&org)
    .bind(&uid)
    .fetch_one(&pool)
    .await
    .expect("folder insert");
    sqlx::query(
        "INSERT INTO node_closure (org, ancestor, descendant, depth) VALUES ($1, $2, $2, 0)",
    )
    .bind(&org)
    .bind(folder_id)
    .execute(&pool)
    .await
    .unwrap();
    // フォルダ owner を付与（editor@folder を通すため）。
    authz
        .write_tuple(
            &Subject::user(&uid),
            Relation::Owner,
            &FgaObject::folder(&folder_id.to_string()),
        )
        .await
        .unwrap();

    let moved = service
        .move_file(&actx, file.id, Some(folder_id), None)
        .await
        .expect("move_file");
    assert_eq!(moved.parent_id, Some(folder_id));
    assert!(moved.version > file.version, "move で version が上がること");
    let depth: i32 = sqlx::query_scalar(
        "SELECT depth FROM node_closure WHERE ancestor = $1 AND descendant = $2",
    )
    .bind(folder_id)
    .bind(file.id)
    .fetch_one(&pool)
    .await
    .expect("closure folder->file");
    assert_eq!(depth, 1, "move で closure に親子(depth 1)が張られること");

    // --- rename ---
    let renamed = service
        .rename_file(&actx, file.id, "renamed.txt", None)
        .await
        .expect("rename");
    assert_eq!(renamed.name, "renamed.txt");

    // --- soft delete → 取得不可 → restore ---
    service
        .soft_delete_file(&actx, file.id, None)
        .await
        .expect("delete");
    assert_eq!(
        blob_refcount(&pool, &org, &sha).await,
        1,
        "削除で refcount 減"
    );
    assert!(
        matches!(
            service.get_metadata(&actx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "削除後は NotFound であること"
    );
    let restored = service
        .restore_file(&actx, file.id, None)
        .await
        .expect("restore");
    assert_eq!(restored.name, "renamed.txt");
    assert_eq!(
        blob_refcount(&pool, &org, &sha).await,
        2,
        "復元で refcount 戻る"
    );

    // --- deny: 非メンバーのアップロードは Forbidden かつ deny 監査が残る ---
    let stranger = make_ctx(&org, &format!("stranger{}", Uuid::new_v4().simple()));
    let denied = service
        .begin_upload(
            &stranger,
            None,
            "x.txt",
            "text/plain",
            &sha,
            size,
            Some("trace-deny"),
        )
        .await;
    assert!(matches!(denied, Err(storage::StorageError::Forbidden)));
    assert!(
        audit_count(&pool, &org, "file.upload_url.issue", "deny").await >= 1,
        "deny が監査される"
    );

    // --- 監査: finalize の allow が記録される ---
    assert!(
        audit_count(&pool, &org, "file.upload.finalize", "allow").await >= 1,
        "finalize の allow が監査される"
    );

    // --- 監査ハッシュチェーン: prev_hash が直前の entry_hash と連結する ---
    let chain_ok: Option<bool> = sqlx::query_scalar(
        "SELECT bool_and(prev_hash IS NOT DISTINCT FROM lag_hash) FROM ( \
            SELECT prev_hash, lag(entry_hash) OVER (ORDER BY id) AS lag_hash \
            FROM audit_log WHERE org = $1 \
         ) t WHERE lag_hash IS NOT NULL",
    )
    .bind(&org)
    .fetch_one(&pool)
    .await
    .expect("chain check");
    assert_eq!(
        chain_ok,
        Some(true),
        "監査ログの prev_hash が直前 entry_hash と一致"
    );
}
