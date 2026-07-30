//! StorageService の結合テスト（実 Postgres + MinIO + OpenFGA が必要）。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が設定されている時のみ実行し、
//! 未設定なら early-return でスキップする（素の `cargo test` を壊さない）。CI の
//! coverage ジョブで postgres/minio/openfga を立てて実走する。
//!
//! 検証: 二相アップロード（presigned PUT→finalize）・content-addressing・org スコープ
//! dedup（PIT-14）・closure を保つ move（PIT-16）・rename/delete/restore・viewer 認可・
//! deny の監査記録・ハッシュチェーン監査ログ。

// テストコード: pedantic/安全系 lint は本番コードのみ厳格化する方針のため許容する。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::{sync::Arc, time::Duration};

use authz::{
    client::{OpenFgaClient, OpenFgaConfig},
    AuthContext, AuthzClient, Consistency, ObjectType, Principal, Relation,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{
    content_address::sha256_hex, object_store::S3Config, DirectoryStore, GeneralAccessLevel, Node,
    NodeKind, ObjectStore, S3ObjectStore, ShareRole, ShareTarget, StorageError, StorageService,
};
use uuid::Uuid;

struct Ctx {
    service: StorageService,
    pool: PgPool,
    authz: Arc<dyn AuthzClient>,
    http: reqwest::Client,
    store: Arc<dyn ObjectStore>,
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
    let store: Arc<dyn ObjectStore> = Arc::new(S3ObjectStore::new(&s3));
    store.ensure_bucket().await.expect("バケット準備");

    let service = StorageService::new(
        pool.clone(),
        store.clone(),
        authz.clone(),
        Duration::from_secs(300),
        Duration::from_secs(900),
        5 * 1024 * 1024 * 1024,
    );
    Some(Ctx {
        service,
        pool,
        authz,
        http,
        store,
    })
}

fn make_ctx(org: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: uid.into(),
            email: None,
            groups: vec![],
            roles: vec![],
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

/// 既存ファイルの内容を新版にアップロードする（target_node_id 経由）。
async fn upload_new_version(
    service: &StorageService,
    http: &reqwest::Client,
    ctx: &AuthContext,
    target: Uuid,
    content: &[u8],
) -> Result<Node, StorageError> {
    let sha = sha256_hex(content);
    let ticket = service
        .begin_upload(
            ctx,
            None,
            "",
            "text/plain",
            &sha,
            content.len() as i64,
            Some(target),
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

/// node_version の行数（版履歴の件数）。
async fn node_version_count(pool: &PgPool, node_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM node_version WHERE node_id = $1")
        .bind(node_id)
        .fetch_one(pool)
        .await
        .expect("node_version count")
}

/// 指定ノード・op の outbox イベント件数。
async fn outbox_count(pool: &PgPool, node_id: Uuid, op: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM storage_event_outbox WHERE node_id = $1 AND op = $2")
        .bind(node_id)
        .bind(op)
        .fetch_one(pool)
        .await
        .expect("outbox count")
}

/// org メンバーとして seed する（ルート作成の認可に必要）。
/// 識別子は実行時と同じ `AuthContext::ns()` 経由で tenant 名前空間化する（SAAS.1）。
async fn seed_org_member(authz: &Arc<dyn AuthzClient>, org: &str, uid: &str) {
    let ctx = make_ctx(org, uid);
    authz
        .write_tuple(
            &ctx.subject(),
            Relation::Member,
            &ctx.ns().organization(org),
        )
        .await
        .expect("member tuple seed");
}

/// closure の depth を引く（無ければ None）。
async fn closure_depth(pool: &PgPool, ancestor: Uuid, descendant: Uuid) -> Option<i32> {
    sqlx::query_scalar("SELECT depth FROM node_closure WHERE ancestor = $1 AND descendant = $2")
        .bind(ancestor)
        .bind(descendant)
        .fetch_optional(pool)
        .await
        .expect("closure query")
}

async fn blob_refcount(pool: &PgPool, org: &str, sha: &str) -> i64 {
    // これらのテストは全て tenant "default"（make_ctx）。blob PK は (tenant_id, org, sha256)。
    sqlx::query_scalar(
        "SELECT refcount FROM blob WHERE tenant_id = 'default' AND org = $1 AND sha256 = $2",
    )
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

/// 共有リンクの node 直列化 advisory lock のキー。**`share_link.rs::lock_node` と同じ式**
/// （変えたら両方を直す）。レーステストが別コネクションから同じロックを掴むために使う。
async fn share_link_lock_key(pool: &PgPool, node_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT hashtextextended('share_link:' || $1, 0)")
        .bind(node_id.to_string())
        .fetch_one(pool)
        .await
        .expect("advisory lock key")
}

/// 指定キーの advisory lock を**待たされている**セッションが現れるまで待つ。
///
/// これがレーステストの決定的な同期点。sleep で「たぶん進んだだろう」に賭けず、対象の処理が
/// 検証を終えてロック取得でブロックしたことを `pg_locks` で確認してから次の操作へ進む。
/// 現れなければ panic するので、直列化が入っていないコード（ロックを取らない redeem）では
/// **確実に落ちる**（false-pass しない）。
///
/// `classid = key >> 32` / `objid = key & 0xFFFFFFFF` / `objsubid = 1` は
/// `pg_advisory_xact_lock(bigint)` の `pg_locks` 表現。キーで絞るので他テストと並行しても誤検知しない。
async fn await_advisory_wait(pool: &PgPool, key: i64) {
    for _ in 0..200 {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_locks \
             WHERE locktype = 'advisory' AND NOT granted AND objsubid = 1 \
               AND classid = (($1::bigint >> 32) & 4294967295)::oid \
               AND objid = ($1::bigint & 4294967295)::oid)",
        )
        .bind(key)
        .fetch_one(pool)
        .await
        .expect("pg_locks query");
        if waiting {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("advisory lock でブロックしているセッションが現れませんでした（node 単位の直列化が効いていない・#376）");
}

/// grant 行の `revoked_at` を引く。`None` = 行が無い / `Some(None)` = live / `Some(Some(_))` = 取消済み。
#[allow(clippy::option_option)]
async fn grant_revoked_at(
    pool: &PgPool,
    link_id: Uuid,
    user_id: &str,
) -> Option<Option<chrono::DateTime<chrono::Utc>>> {
    sqlx::query_scalar(
        "SELECT revoked_at FROM node_share_link_grant WHERE link_id = $1 AND user_id = $2",
    )
    .bind(link_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .expect("grant revoked_at")
}

#[tokio::test]
async fn storage_end_to_end() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    // org/ユーザーをテスト毎にユニーク化し、行を隔離する。
    let org = format!("itorg{}", Uuid::new_v4().simple());
    let uid = format!("ituser{}", Uuid::new_v4().simple());
    let actx = make_ctx(&org, &uid);

    // org メンバーとして seed（ルート直下アップロードの認可に必要）。
    authz
        .write_tuple(
            &actx.subject(),
            Relation::Member,
            &actx.ns().organization(&org),
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
            &bctx.subject(),
            Relation::Member,
            &bctx.ns().organization(&org_b),
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
                &cctx.subject(),
                Relation::Member,
                &cctx.ns().organization(&org),
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
                &dctx.subject(),
                Relation::Member,
                &dctx.ns().organization(&org),
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

    // --- Major-2: 宣言サイズが上限（既定 5 GiB）超なら declare を拒否（容量ガード） ---
    {
        let too_big = service
            .begin_upload(
                &actx,
                None,
                "huge.bin",
                "application/octet-stream",
                &sha256_hex(b"x"),
                6 * 1024 * 1024 * 1024,
                None,
                None,
            )
            .await;
        assert!(
            matches!(too_big, Err(StorageError::Invalid(_))),
            "{too_big:?}"
        );
    }

    // --- move（フォルダを直接用意し、closure を検証） ---
    let folder_id: Uuid = sqlx::query_scalar(
        "INSERT INTO node (org, tenant_id, kind, name, created_by, updated_by) \
         VALUES ($1, 'default', 'folder', 'myfolder', $2, $2) RETURNING id",
    )
    .bind(&org)
    .bind(&uid)
    .fetch_one(&pool)
    .await
    .expect("folder insert");
    sqlx::query(
        "INSERT INTO node_closure (tenant_id, org, ancestor, descendant, depth) VALUES ('default', $1, $2, $2, 0)",
    )
    .bind(&org)
    .bind(folder_id)
    .execute(&pool)
    .await
    .unwrap();
    // フォルダ owner を付与（editor@folder を通すため）。
    authz
        .write_tuple(
            &actx.subject(),
            Relation::Owner,
            &actx.ns().folder(&folder_id.to_string()),
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
    // 論理削除では refcount を変えない（復元可能な間は本体を参照し続ける＝GC で消されない・LbvQZ）。
    assert_eq!(
        blob_refcount(&pool, &org, &sha).await,
        2,
        "論理削除では refcount を減らさない"
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
        "復元でも refcount は不変（削除で減らしていないため）"
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
            None,
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

    // --- 監査ハッシュチェーン: chained 行のみで prev_hash が直前の chained entry_hash に連結する ---
    let chain_ok: Option<bool> = sqlx::query_scalar(
        "SELECT bool_and(prev_hash IS NOT DISTINCT FROM lag_hash) FROM ( \
            SELECT prev_hash, lag(entry_hash) OVER (ORDER BY id) AS lag_hash \
            FROM audit_log WHERE org = $1 AND chained \
         ) t WHERE lag_hash IS NOT NULL",
    )
    .bind(&org)
    .fetch_one(&pool)
    .await
    .expect("chain check");
    assert_eq!(
        chain_ok,
        Some(true),
        "chained 監査ログの prev_hash が直前 chained entry_hash と一致"
    );
    // 読取/deny は未チェーン（prev_hash=NULL）であることを確認（Major-3: 読取を直列化しない）。
    let unchained_reads: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log \
         WHERE org = $1 AND action = 'file.metadata.read' AND chained = false",
    )
    .bind(&org)
    .fetch_one(&pool)
    .await
    .expect("unchained reads");
    assert!(unchained_reads >= 1, "読取監査は未チェーンで記録される");
}

/// Task 1.5: フォルダ作成/深い move（closure 整合）/循環拒否/権限フィルタ子一覧/パンくず。
#[tokio::test]
async fn folder_hierarchy_end_to_end() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let uid = format!("ituser{}", Uuid::new_v4().simple());
    let actx = make_ctx(&org, &uid);
    seed_org_member(&authz, &org, &uid).await;

    // root/folderA/sub1/sub2 を作る（作成者は各フォルダの owner ＝ editor も含意）。
    let folder_a = service
        .create_folder(&actx, None, "folderA", None)
        .await
        .expect("create folderA");
    assert_eq!(folder_a.kind, NodeKind::Folder);
    let sub1 = service
        .create_folder(&actx, Some(folder_a.id), "sub1", None)
        .await
        .expect("create sub1");
    let sub2 = service
        .create_folder(&actx, Some(sub1.id), "sub2", None)
        .await
        .expect("create sub2");
    // sub2 配下にファイル（深い階層）。
    let deep_file = upload(&service, &http, &actx, Some(sub2.id), "deep.txt", b"deep")
        .await
        .expect("deep upload");

    // 深い階層の closure: folderA -> deep_file は depth 3。
    assert_eq!(
        closure_depth(&pool, folder_a.id, deep_file.id).await,
        Some(3),
        "folderA から深いファイルまで depth 3"
    );

    // 循環拒否: folderA を自身の子孫（sub2）配下へは移動できない。
    let cyclic = service
        .move_folder(&actx, folder_a.id, Some(sub2.id), None)
        .await;
    assert!(
        matches!(cyclic, Err(StorageError::Invalid(_))),
        "{cyclic:?}"
    );

    // 深い move: folderB を作り、sub1 をサブツリーごと folderB 配下へ移す。
    let folder_b = service
        .create_folder(&actx, None, "folderB", None)
        .await
        .expect("create folderB");
    service
        .move_folder(&actx, sub1.id, Some(folder_b.id), None)
        .await
        .expect("move sub1 under folderB");

    // closure 整合: folderB -> sub1(1) / sub2(2) / deep_file(3)。旧祖先 folderA は切れている。
    assert_eq!(closure_depth(&pool, folder_b.id, sub1.id).await, Some(1));
    assert_eq!(closure_depth(&pool, folder_b.id, sub2.id).await, Some(2));
    assert_eq!(
        closure_depth(&pool, folder_b.id, deep_file.id).await,
        Some(3)
    );
    assert_eq!(
        closure_depth(&pool, folder_a.id, deep_file.id).await,
        None,
        "旧祖先 folderA からのリンクはサブツリーごと消えている"
    );

    // パンくず（root→自身）: folderB / sub1 / sub2 / deep.txt。
    let crumbs = service
        .breadcrumb(&actx, deep_file.id, None)
        .await
        .expect("breadcrumb");
    let names: Vec<&str> = crumbs.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["folderB", "sub1", "sub2", "deep.txt"]);

    // --- 権限フィルタ子一覧（root 直下） ---
    // uid の root 直下には folderA / folderB がある。別 org メンバー C は読めない。
    let uid_c = format!("ituser{}", Uuid::new_v4().simple());
    let cctx = make_ctx(&org, &uid_c);
    seed_org_member(&authz, &org, &uid_c).await;

    // C は root の何も読めない（owner でも共有先でもない）→ 空ページ。
    let page_c = service
        .list_children(&cctx, None, Default::default(), None, 50, None)
        .await
        .expect("C list root");
    assert!(page_c.items.is_empty(), "C は読めるルート子が無い");

    // folderA を C に viewer 共有 → C のルート一覧に folderA だけ現れる（folderB は出ない）。
    service
        .share_node(
            &actx,
            folder_a.id,
            &ShareTarget::User { id: uid_c.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("share folderA to C");
    let page_c2 = service
        .list_children(&cctx, None, Default::default(), None, 50, None)
        .await
        .expect("C list root after share");
    let ids: Vec<Uuid> = page_c2.items.iter().map(|n| n.id).collect();
    assert_eq!(ids, vec![folder_a.id], "共有された folderA のみ見える");

    // --- ページング（uid のルート子＝folderA/folderB を limit 1 で 2 ページ） ---
    let p1 = service
        .list_children(&actx, None, Default::default(), None, 1, None)
        .await
        .expect("page1");
    assert_eq!(p1.items.len(), 1, "1 ページ目は 1 件");
    assert!(p1.next_cursor.is_some(), "続きがある");
    let p2 = service
        .list_children(
            &actx,
            None,
            Default::default(),
            p1.next_cursor.as_deref(),
            1,
            None,
        )
        .await
        .expect("page2");
    assert_eq!(p2.items.len(), 1, "2 ページ目も 1 件");
    // 2 ページで folderA/folderB を重複なく網羅する（name 昇順なので folderA→folderB）。
    let mut seen: Vec<Uuid> = vec![p1.items[0].id, p2.items[0].id];
    seen.sort();
    let mut want = vec![folder_a.id, folder_b.id];
    want.sort();
    assert_eq!(seen, want, "ページ跨ぎで重複なく全件");

    // --- breadcrumb の権限境界: leaf だけ直接共有された場合、祖先名は漏れない ---
    // deep_file だけを uid_e に viewer 共有（祖先 folderB/sub1/sub2 は未共有）。
    let uid_e = format!("ituser{}", Uuid::new_v4().simple());
    let ectx = make_ctx(&org, &uid_e);
    service
        .share_node(
            &actx,
            deep_file.id,
            &ShareTarget::User { id: uid_e.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("share deep_file to E");
    let e_crumbs = service
        .breadcrumb(&ectx, deep_file.id, None)
        .await
        .expect("E breadcrumb");
    // 読める接尾のみ＝自身だけ。祖先フォルダ名（folderB/sub1/sub2）は出ない。
    let e_names: Vec<&str> = e_crumbs.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(e_names, vec!["deep.txt"], "未読の祖先名は漏れない");

    // --- フォルダ削除（サブツリーごと論理削除）→ 配下が読めなくなる ---
    service
        .soft_delete_folder(&actx, folder_b.id, None)
        .await
        .expect("delete folderB");
    assert!(
        matches!(
            service.get_metadata(&actx, deep_file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "サブツリー配下のファイルも論理削除される"
    );
}

/// Task 1.6: user 共有で継承アクセス / 共有解除で即時不可 / 共有相手・共有された一覧。
/// （role/部署共有は #76 で defer。本テストは user 共有のみ）
#[tokio::test]
async fn sharing_end_to_end() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;

    // bob（共有される個人）と、共有されない別ユーザー。
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;
    let other = format!("ituser{}", Uuid::new_v4().simple());
    let octx_other = make_ctx(&org, &other);
    seed_org_member(&authz, &org, &other).await;

    // owner が root にファイルを作る。
    let file = upload(&service, &http, &octx, None, "shared.txt", b"share me")
        .await
        .expect("upload");

    // 共有前: bob は読めない（存在秘匿の NotFound）。
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "共有前は読めない"
    );

    // bob へ viewer 共有 → bob は読めるようになる。
    service
        .share_node(
            &octx,
            file.id,
            &ShareTarget::User { id: bob.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("share to bob");
    let seen = service
        .get_metadata(&bctx, file.id, None)
        .await
        .expect("bob reads via share");
    assert_eq!(seen.name, "shared.txt");

    // 共有は対象ユーザーに限定: 共有されていない別ユーザーは読めない。
    assert!(
        matches!(
            service.get_metadata(&octx_other, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "共有は対象ユーザーのみ（他ユーザーへは漏れない）"
    );

    // 既共有の再共有は冪等（補償ロールバックの逆破壊が起きないこと＝再共有後も bob は読める）。
    service
        .share_node(
            &octx,
            file.id,
            &ShareTarget::User { id: bob.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("re-share is idempotent");
    assert!(
        service.get_metadata(&bctx, file.id, None).await.is_ok(),
        "再共有後も bob は読める（冪等 no-op が既存共有を壊さない）"
    );

    // 共有相手一覧に user(bob)/viewer が出る。
    let shares = service
        .list_shares(&octx, file.id, None)
        .await
        .expect("list shares");
    assert!(
        shares.iter().any(|e| {
            matches!(&e.target, ShareTarget::User { id } if id == &bob)
                && matches!(e.role, ShareRole::Viewer)
        }),
        "共有相手に user viewer が現れる: {shares:?}"
    );

    // bob の「共有された一覧」に file が出る（自分が作成したものではない）。
    let inbox = service
        .list_shared_with_me(&bctx, None, 50, None)
        .await
        .expect("shared with me");
    assert!(
        inbox.items.iter().any(|n| n.id == file.id),
        "共有された一覧に現れる"
    );
    // owner の「共有された一覧」には自作 file は出ない（作成者除外）。
    let owner_inbox = service
        .list_shared_with_me(&octx, None, 50, None)
        .await
        .expect("owner inbox");
    assert!(
        !owner_inbox.items.iter().any(|n| n.id == file.id),
        "作成者本人のファイルは共有された一覧に出ない"
    );

    // 共有解除 → PIT-11（HIGHER_CONSISTENCY）で即時にアクセス不可。
    service
        .unshare_node(
            &octx,
            file.id,
            &ShareTarget::User { id: bob.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("unshare");
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "共有解除で即時にアクセス不可"
    );

    // owner でない bob は共有管理（list_shares）できない（存在秘匿でなく Forbidden）。
    let denied = service.list_shares(&bctx, file.id, None).await;
    assert!(matches!(denied, Err(StorageError::Forbidden)), "{denied:?}");
}

/// SAAS.1: authz タプルが tenant 境界を越えないこと。
///
/// **同一 org 文字列・同一 uid** を 2 つの tenant で共有しても、authz の識別子名前空間化
/// （`<type>:<tenant>|<local>`）により membership も共有も越境しないことを、authz レベル
/// （raw check）と storage レベル（共有ファイルの不可視）の両面で実証する
/// （受け入れ条件「あるテナントのデータが他テナントの取得に一切現れない・authz タプルも境界を越えない」）。
#[tokio::test]
async fn authz_tuples_do_not_cross_tenant() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    // org 文字列と uid を 2 tenant で意図的に一致させる（DB 行分離だけでなく authz 名前空間で
    // 隔離されることを示すため）。
    let org = format!("itorg{}", Uuid::new_v4().simple());
    let uid = format!("ituser{}", Uuid::new_v4().simple());
    let ta = format!("ta{}", Uuid::new_v4().simple());
    let tb = format!("tb{}", Uuid::new_v4().simple());
    let ctx_a = make_ctx_tenant(&org, &ta, &uid);
    let ctx_b = make_ctx_tenant(&org, &tb, &uid);

    // tenant A でのみ org member タプルを付与する。
    authz
        .write_tuple(
            &ctx_a.subject(),
            Relation::Member,
            &ctx_a.ns().organization(&org),
        )
        .await
        .expect("seed member in tenant A");

    // authz レベル: 同一 (org, uid) でも tenant B は member ではない（タプルが越境しない）。
    assert!(
        authz
            .check(
                &ctx_a.subject(),
                Relation::Member,
                &ctx_a.ns().organization(&org),
                Consistency::HigherConsistency,
            )
            .await
            .unwrap(),
        "tenant A の member 判定は true"
    );
    assert!(
        !authz
            .check(
                &ctx_b.subject(),
                Relation::Member,
                &ctx_b.ns().organization(&org),
                Consistency::HigherConsistency,
            )
            .await
            .unwrap(),
        "同一 org・同一 uid でも別 tenant は member にならない（authz タプルが越境しない）"
    );

    // storage レベル: tenant A が作ったファイルを bob へ共有 → tenant B の同一 uid の bob には
    // 一切見えない。対照として tenant A の bob には見える。
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let ctx_a_bob = make_ctx_tenant(&org, &ta, &bob);
    authz
        .write_tuple(
            &ctx_a_bob.subject(),
            Relation::Member,
            &ctx_a_bob.ns().organization(&org),
        )
        .await
        .expect("seed bob in tenant A");

    let file = upload(
        &service,
        &http,
        &ctx_a,
        None,
        "a-secret.txt",
        b"tenant A only",
    )
    .await
    .expect("upload in tenant A");
    service
        .share_node(
            &ctx_a,
            file.id,
            &ShareTarget::User { id: bob.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("share to bob in tenant A");

    // 対照: tenant A の bob には共有ファイルが見える。
    let inbox_a = service
        .list_shared_with_me(&ctx_a_bob, None, 50, None)
        .await
        .expect("inbox A");
    assert!(
        inbox_a.items.iter().any(|n| n.id == file.id),
        "同一 tenant の共有相手には見える"
    );

    // 本命: tenant B の同一 uid の bob には共有ファイルが漏れない（shared-with-me / 直接取得の両方）。
    let ctx_b_bob = make_ctx_tenant(&org, &tb, &bob);
    let inbox_b = service
        .list_shared_with_me(&ctx_b_bob, None, 50, None)
        .await
        .expect("inbox B");
    assert!(
        !inbox_b.items.iter().any(|n| n.id == file.id),
        "別 tenant には共有ファイルが一切現れない"
    );
    assert!(
        matches!(
            service.get_metadata(&ctx_b_bob, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "別 tenant からは直接取得もできない（存在秘匿）"
    );
}

/// #76: role/部署共有。ロールのメンバー（provisioning されたタプル）が共有経由で読め、
/// 非メンバーは読めず、list_shares に Role ターゲットが出て、unshare で即時不可になること。
#[tokio::test]
async fn role_sharing_end_to_end() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;

    // 部署ロール dept と、そのメンバー bob（role provisioning を模した member タプル付与）。
    let dept = format!("dept-{}", Uuid::new_v4().simple());
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;
    authz
        .write_tuple(&bctx.subject(), Relation::Member, &bctx.ns().role(&dept))
        .await
        .expect("provision bob into dept role");
    // dept に属さない別ユーザー。
    let outsider = format!("ituser{}", Uuid::new_v4().simple());
    let octx_out = make_ctx(&org, &outsider);
    seed_org_member(&authz, &org, &outsider).await;

    // owner がファイル作成。共有前は dept メンバーの bob も読めない。
    let file = upload(&service, &http, &octx, None, "dept.txt", b"dept only")
        .await
        .expect("upload");
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "共有前は dept メンバーでも読めない"
    );

    // dept ロールへ viewer 共有 → dept メンバーの bob は role 経由で読める。
    service
        .share_node(
            &octx,
            file.id,
            &ShareTarget::Role { id: dept.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("share to dept role");
    assert_eq!(
        service
            .get_metadata(&bctx, file.id, None)
            .await
            .expect("dept member reads via role share")
            .name,
        "dept.txt"
    );
    // dept に属さないユーザーは読めない。
    assert!(
        matches!(
            service.get_metadata(&octx_out, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "role 非メンバーは読めない"
    );

    // 共有相手一覧に Role(dept)/viewer が出る。
    let shares = service
        .list_shares(&octx, file.id, None)
        .await
        .expect("list shares");
    assert!(
        shares.iter().any(|e| {
            matches!(&e.target, ShareTarget::Role { id } if id == &dept)
                && matches!(e.role, ShareRole::Viewer)
        }),
        "共有相手に role viewer が現れる: {shares:?}"
    );

    // bob の shared-with-me に file が出る（role 経由の viewer 実効集合）。
    let inbox = service
        .list_shared_with_me(&bctx, None, 50, None)
        .await
        .expect("shared with me");
    assert!(
        inbox.items.iter().any(|n| n.id == file.id),
        "role メンバーの共有一覧に現れる"
    );

    // 共有解除 → dept メンバーでも即時にアクセス不可。
    service
        .unshare_node(
            &octx,
            file.id,
            &ShareTarget::Role { id: dept.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("unshare role");
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "role 共有解除で即時にアクセス不可"
    );
}

/// updated_by が更新の実行主体を追跡する（Task 11P.10）: 別主体（AI 相当）が editor として
/// 内容更新すると updated_by が切り替わり、created_by は作成者のまま不変。csv.patch・AI 共同編集の
/// 保存も同じ `update_file_content_internal` 経路＝同じ名義記録になる。
#[tokio::test]
async fn updated_by_tracks_editor() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let alice = format!("ituser{}", Uuid::new_v4().simple());
    seed_org_member(&authz, &org, &alice).await;
    let actx = make_ctx(&org, &alice);

    let file = upload(&service, &http, &actx, None, "shared.txt", b"v1")
        .await
        .expect("upload v1");
    assert_eq!(file.created_by, alice);
    assert_eq!(file.updated_by, alice);

    // AI 相当の別主体へ editor を共有し、その主体で内容更新する。
    let ai = format!("ai-agent-{}", Uuid::new_v4().simple());
    let ai_ctx = make_ctx(&org, &ai);
    service
        .share_node(
            &actx,
            file.id,
            &storage::ShareTarget::User { id: ai.clone() },
            storage::ShareRole::Editor,
            None,
        )
        .await
        .expect("share editor to ai");
    let edited = service
        .update_file_content_internal(&ai_ctx, file.id, b"edited by ai", "text/plain", None)
        .await
        .expect("ai content update");
    assert_eq!(edited.updated_by, ai, "更新者が AI 主体名義になる");
    assert_eq!(edited.created_by, alice, "作成者は不変");
    // 版の author も更新主体（既存 FileVersion.author）。
    let (history, _) = service
        .list_versions(&actx, file.id, None, 50, None)
        .await
        .expect("list versions");
    assert_eq!(history[0].author, ai, "最新版の author は AI 主体");
    assert_eq!(history[1].author, alice, "初版の author は作成者");
}

#[tokio::test]
async fn versioning_end_to_end() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let alice = format!("ituser{}", Uuid::new_v4().simple());
    seed_org_member(&authz, &org, &alice).await;
    let actx = make_ctx(&org, &alice);

    // 初版アップロード（version 1・履歴 1 件）。
    let v1_bytes = b"version one contents";
    let sha_v1 = sha256_hex(v1_bytes);
    let file = upload(&service, &http, &actx, None, "doc.txt", v1_bytes)
        .await
        .expect("upload v1");
    assert_eq!(file.version, 1);
    // 作成時は updated_by = 作成者（Task 11P.10）。
    assert_eq!(file.updated_by, alice);
    assert_eq!(file.created_by, alice);
    assert_eq!(node_version_count(&pool, file.id).await, 1);
    assert_eq!(blob_refcount(&pool, &org, &sha_v1).await, 1);

    // 内容更新（version 2・履歴 2 件・新 blob）。
    let v2_bytes = b"version two has different contents";
    let sha_v2 = sha256_hex(v2_bytes);
    let updated = upload_new_version(&service, &http, &actx, file.id, v2_bytes)
        .await
        .expect("upload v2");
    assert_eq!(updated.version, 2);
    assert_eq!(updated.blob_sha256.as_deref(), Some(sha_v2.as_str()));
    // 同一主体の更新なので updated_by は据え置き、created_by は不変（Task 11P.10）。
    assert_eq!(updated.updated_by, alice);
    assert_eq!(updated.created_by, alice);
    assert_eq!(node_version_count(&pool, file.id).await, 2);
    // 旧版の blob は減らさない（履歴＝安全網のため download/restore 可能）。
    assert_eq!(blob_refcount(&pool, &org, &sha_v1).await, 1);
    assert_eq!(blob_refcount(&pool, &org, &sha_v2).await, 1);

    // 履歴一覧は新しい順（v2, v1）。
    let (history, _) = service
        .list_versions(&actx, file.id, None, 50, None)
        .await
        .expect("list versions");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].version, 2);
    assert_eq!(history[1].version, 1);
    assert_eq!(history[1].blob_sha256, sha_v1);

    // 特定版の DL URL が各版の実体を返す。
    let url_v1 = service
        .issue_version_download_url(&actx, file.id, 1, None)
        .await
        .expect("v1 url");
    let got_v1 = http
        .get(&url_v1.url)
        .send()
        .await
        .expect("GET v1")
        .bytes()
        .await
        .expect("v1 bytes");
    assert_eq!(got_v1.as_ref(), v1_bytes);
    let url_v2 = service
        .issue_version_download_url(&actx, file.id, 2, None)
        .await
        .expect("v2 url");
    let got_v2 = http
        .get(&url_v2.url)
        .send()
        .await
        .expect("GET v2")
        .bytes()
        .await
        .expect("v2 bytes");
    assert_eq!(got_v2.as_ref(), v2_bytes);

    // v1 を復元 → 新版 v3（履歴を壊さず追記・blob は v1 を共有）。
    let restored = service
        .restore_version(&actx, file.id, 1, None)
        .await
        .expect("restore v1");
    assert_eq!(restored.version, 3);
    assert_eq!(restored.blob_sha256.as_deref(), Some(sha_v1.as_str()));
    assert_eq!(node_version_count(&pool, file.id).await, 3);
    // v1 の blob は v1 行 + v3 行で参照され refcount=2。
    assert_eq!(blob_refcount(&pool, &org, &sha_v1).await, 2);
    // 履歴は v1/v2 とも残存（壊れない）。
    let (history2, _) = service
        .list_versions(&actx, file.id, None, 50, None)
        .await
        .expect("list versions 2");
    let versions: Vec<i64> = history2.iter().map(|v| v.version).collect();
    assert_eq!(versions, vec![3, 2, 1]);

    // 書込イベントが各操作で発行されている。
    assert_eq!(outbox_count(&pool, file.id, "create").await, 1);
    assert_eq!(outbox_count(&pool, file.id, "update").await, 1);
    assert_eq!(outbox_count(&pool, file.id, "restore").await, 1);
}

#[tokio::test]
async fn outbox_end_to_end() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let alice = format!("ituser{}", Uuid::new_v4().simple());
    seed_org_member(&authz, &org, &alice).await;
    let actx = make_ctx(&org, &alice);

    // create → update（内容）→ rename → move → delete → restore を順に実行する。
    let file = upload(&service, &http, &actx, None, "evt.txt", b"first")
        .await
        .expect("create");
    upload_new_version(&service, &http, &actx, file.id, b"second updated")
        .await
        .expect("content update");
    service
        .rename_file(&actx, file.id, "evt-renamed.txt", None)
        .await
        .expect("rename");
    let folder = service
        .create_folder(&actx, None, "evtfolder", None)
        .await
        .expect("folder");
    service
        .move_file(&actx, file.id, Some(folder.id), None)
        .await
        .expect("move");
    service
        .soft_delete_file(&actx, file.id, None)
        .await
        .expect("delete");
    let restored = service
        .restore_file(&actx, file.id, None)
        .await
        .expect("restore");

    // 各操作が書込と同一 txn で outbox に入っている（op ごとに 1 件）。
    assert_eq!(outbox_count(&pool, file.id, "create").await, 1);
    assert_eq!(outbox_count(&pool, file.id, "update").await, 1);
    assert_eq!(outbox_count(&pool, file.id, "rename").await, 1);
    assert_eq!(outbox_count(&pool, file.id, "move").await, 1);
    assert_eq!(outbox_count(&pool, file.id, "delete").await, 1);
    assert_eq!(outbox_count(&pool, file.id, "restore").await, 1);

    // フィールドの一例を検証（restore イベントは最新版を指す）。
    let (ev_org, ev_tenant, ev_actor, ev_version): (String, String, String, i64) = sqlx::query_as(
        "SELECT org, tenant_id, actor, version FROM storage_event_outbox \
             WHERE node_id = $1 AND op = 'restore'",
    )
    .bind(file.id)
    .fetch_one(&pool)
    .await
    .expect("restore event");
    assert_eq!(ev_org, org);
    assert_eq!(ev_tenant, "default");
    assert_eq!(ev_actor, alice);
    assert_eq!(ev_version, restored.version);

    // 本ノードの未処理イベント id を集める。
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM storage_event_outbox WHERE node_id = $1 AND processed_at IS NULL ORDER BY id",
    )
    .bind(file.id)
    .fetch_all(&pool)
    .await
    .expect("ids");
    assert_eq!(ids.len(), 6, "create/update/rename/move/delete/restore");

    // outbox は共有テーブルのため、判定は**本ノードにスコープ**して並行/残留イベントから隔離する。
    // claim はグローバルに未処理を引くので、未飽和（取り切れた）時のみ包含を検証する。
    const LIMIT: i64 = 10_000;

    // at-least-once: claim 後に commit せず rollback すると未処理のまま再配信される。
    {
        let mut tx = pool.begin().await.expect("tx1");
        let claimed = storage::event::claim(&mut tx, LIMIT).await.expect("claim");
        if claimed.len() < LIMIT as usize {
            let claimed_ids: std::collections::HashSet<i64> =
                claimed.iter().map(|e| e.id).collect();
            assert!(
                ids.iter().all(|id| claimed_ids.contains(id)),
                "未飽和の claim は本ノードの未処理イベントを全て含む"
            );
        }
        // commit しない（drop でロールバック）。
    }
    // 本ノードスコープの未処理件数は rollback で不変（再配信される）。
    let still_unprocessed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM storage_event_outbox WHERE node_id = $1 AND processed_at IS NULL",
    )
    .bind(file.id)
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(still_unprocessed, 6, "rollback で未処理のまま");

    // 本ノードの id を明示して mark_processed → commit で ack（claim 結果に依存しない）。
    {
        let mut tx = pool.begin().await.expect("tx2");
        storage::event::mark_processed(&mut tx, &ids)
            .await
            .expect("ack");
        tx.commit().await.expect("commit");
    }
    let after_ack: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM storage_event_outbox WHERE node_id = $1 AND processed_at IS NULL",
    )
    .bind(file.id)
    .fetch_one(&pool)
    .await
    .expect("count2");
    assert_eq!(after_ack, 0, "ack 後は未処理ゼロ");
}

#[tokio::test]
async fn name_search_filters_by_permission_and_neutralizes_wildcards() {
    let Some(cx) = setup().await else {
        return;
    };
    let org = format!("org-{}", Uuid::new_v4().simple());
    seed_org_member(&cx.authz, &org, "alice").await;
    seed_org_member(&cx.authz, &org, "bob").await;
    let alice = make_ctx(&org, "alice");
    let bob = make_ctx(&org, "bob");
    let sort = storage::ChildSort {
        key: storage::ChildSortKey::Name,
        desc: false,
    };

    // alice: フォルダ「営業企画」＋配下ファイル「月次_報告.txt」、ルートに「報告メモ.txt」
    let dept = cx
        .service
        .create_folder(&alice, None, "営業企画", None)
        .await
        .unwrap();
    let monthly = upload(
        &cx.service,
        &cx.http,
        &alice,
        Some(dept.id),
        "月次_報告.txt",
        b"m",
    )
    .await
    .unwrap();
    upload(&cx.service, &cx.http, &alice, None, "報告メモ.txt", b"n")
        .await
        .unwrap();

    // 空白区切りの AND 部分一致（フォルダ横断）。
    let page = cx
        .service
        .search_nodes_by_name(&alice, "月次 報告", sort, None, 50, None)
        .await
        .unwrap();
    assert_eq!(
        page.items
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>(),
        vec!["月次_報告.txt"],
        "全語一致のみヒットする（報告メモ は 月次 を含まないため除外）"
    );

    // ILIKE ワイルドカードは無害化される（"_" がワイルドカードとして解釈されない）。
    let page = cx
        .service
        .search_nodes_by_name(&alice, "月次_報告", sort, None, 50, None)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let page = cx
        .service
        .search_nodes_by_name(&alice, "月次%", sort, None, 50, None)
        .await
        .unwrap();
    assert!(page.items.is_empty(), "% は文字として扱われヒットしない");

    // 権限フィルタ: bob には共有前は何も見えない。共有後はフォルダ配下だけ見える。
    let page = cx
        .service
        .search_nodes_by_name(&bob, "報告", sort, None, 50, None)
        .await
        .unwrap();
    assert!(page.items.is_empty(), "未共有ノードは名前検索でも見えない");
    cx.service
        .share_node(
            &alice,
            dept.id,
            &ShareTarget::User { id: "bob".into() },
            ShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let page = cx
        .service
        .search_nodes_by_name(&bob, "報告", sort, None, 50, None)
        .await
        .unwrap();
    assert_eq!(
        page.items.iter().map(|n| n.id).collect::<Vec<_>>(),
        vec![monthly.id],
        "共有フォルダ配下のみヒット（ルートの 報告メモ は見えない）"
    );

    // テナント遮断: 非メンバーは 403（fail-closed）。メンバーでもデータは tenant 列で遮断。
    let outsider = make_ctx_tenant(&org, "other-tenant", "alice");
    let err = cx
        .service
        .search_nodes_by_name(&outsider, "報告", sort, None, 50, None)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Forbidden), "非メンバーは 403");
    cx.authz
        .write_tuple(
            &outsider.subject(),
            Relation::Member,
            &outsider.ns().organization(&org),
        )
        .await
        .unwrap();
    let page = cx
        .service
        .search_nodes_by_name(&outsider, "報告", sort, None, 50, None)
        .await
        .unwrap();
    assert!(
        page.items.is_empty(),
        "同名でも別テナントのデータはヒットしない"
    );
}

/// tenant_id ＋ org でスコープした AuthContext を作る（テナント分離検証用）。
fn make_ctx_tenant(org: &str, tenant: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: uid.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: None,
        },
        org.into(),
        tenant.into(),
    )
}

#[tokio::test]
async fn directory_search_is_tenant_scoped() {
    let Some(cx) = setup().await else {
        return;
    };
    let dir = DirectoryStore::new(cx.pool.clone());
    let s = Uuid::new_v4().simple().to_string();
    let (ta, tb) = (format!("ta-{s}"), format!("tb-{s}"));
    // org は alice/bob/dave/charlie で**共通**にし、分離が tenant_id のみで成立することを示す
    // （org 差分に依存しない＝真のテナント分離検証）。
    let org = format!("o-{s}");
    let alice = format!("alice-{s}");
    // 同テナントに 2 名（bob/dave）置き、charlie は同 org・別テナントに置く。
    let bob = format!("bob-{s}");
    let dave = format!("dave-{s}");
    let charlie = format!("charlie-{s}");
    dir.upsert_user(&alice, &ta, &org, &format!("{alice}@a.example"), "Alice")
        .await
        .expect("seed alice");
    dir.upsert_user(&bob, &ta, &org, &format!("{bob}@a.example"), "Bob")
        .await
        .expect("seed bob");
    dir.upsert_user(&dave, &ta, &org, &format!("{dave}@a.example"), "Dave")
        .await
        .expect("seed dave");
    // charlie は **alice と同じ org** だが **別テナント**（tb）。tenant_id だけで除外されること
    // を検証する（org は一致しているので org フィルタでは弾けない）。
    dir.upsert_user(
        &charlie,
        &tb,
        &org,
        &format!("{charlie}@b.example"),
        "Charlie",
    )
    .await
    .expect("seed charlie");

    let ctx = make_ctx_tenant(&org, &ta, &alice);
    // 空クエリは同テナント（かつ自分以外）を返す: bob/dave は出る、charlie は出ない、自分は除外。
    let page = dir.search(&ctx, "", None, 50).await.expect("search all");
    let ids: Vec<&str> = page.items.iter().map(|u| u.id.as_str()).collect();
    assert!(
        ids.contains(&bob.as_str()),
        "同テナントの bob が出る: {ids:?}"
    );
    assert!(
        ids.contains(&dave.as_str()),
        "同テナントの dave が出る: {ids:?}"
    );
    assert!(
        !ids.contains(&charlie.as_str()),
        "別テナントの charlie は出ない"
    );
    assert!(!ids.contains(&alice.as_str()), "自分自身は除外");

    // 別テナントのユーザーを名前で検索しても出ない（pre-filter が tenant_id で効く）。
    let page2 = dir
        .search(&ctx, &charlie, None, 50)
        .await
        .expect("search charlie");
    assert!(page2.items.is_empty(), "別テナント charlie は検索に出ない");

    // keyset ページング（limit 1。同テナントに 2 名いるので 2 ページに分かれる）。
    let p1 = dir.search(&ctx, "", None, 1).await.expect("page1");
    assert_eq!(p1.items.len(), 1);
    assert!(p1.next_cursor.is_some(), "続きがある");
    let p2 = dir
        .search(&ctx, "", p1.next_cursor.as_deref(), 1)
        .await
        .expect("page2");
    assert_eq!(p2.items.len(), 1, "2 ページ目に残りの 1 名");
    assert_ne!(p1.items[0].id, p2.items[0].id, "ページ跨ぎで重複しない");
}

/// 更新者/作者の表示名解決（Task 11P.10）: 同テナントは解決、別テナント・未登録は除外。
#[tokio::test]
async fn resolve_display_names_is_tenant_scoped() {
    let Some(cx) = setup().await else {
        return;
    };
    let dir = DirectoryStore::new(cx.pool.clone());
    let s = Uuid::new_v4().simple().to_string();
    let (ta, tb) = (format!("ta-{s}"), format!("tb-{s}"));
    let org = format!("o-{s}");
    let alice = format!("alice-{s}");
    let bob = format!("bob-{s}");
    let charlie = format!("charlie-{s}");
    dir.upsert_user(&alice, &ta, &org, &format!("{alice}@a.example"), "Alice")
        .await
        .expect("seed alice");
    dir.upsert_user(&bob, &ta, &org, &format!("{bob}@a.example"), "Bob")
        .await
        .expect("seed bob");
    // charlie は別テナント（tb）。同 org でも tenant_id で除外されること。
    dir.upsert_user(
        &charlie,
        &tb,
        &org,
        &format!("{charlie}@b.example"),
        "Charlie",
    )
    .await
    .expect("seed charlie");

    let ctx = make_ctx_tenant(&org, &ta, &alice);
    let unknown = format!("ai-agent-{s}");
    let names = dir
        .resolve_display_names(
            &ctx,
            &[alice.clone(), bob.clone(), charlie.clone(), unknown.clone()],
        )
        .await
        .expect("resolve");
    assert_eq!(names.get(&alice).map(String::as_str), Some("Alice"));
    assert_eq!(names.get(&bob).map(String::as_str), Some("Bob"));
    assert!(!names.contains_key(&charlie), "別テナントは解決しない");
    assert!(
        !names.contains_key(&unknown),
        "未登録 subject（AI 等）は解決しない"
    );

    // 空入力は空マップ（クエリを撃たない）。
    let empty = dir.resolve_display_names(&ctx, &[]).await.expect("empty");
    assert!(empty.is_empty());
}

#[tokio::test]
async fn trash_lists_roots_and_folder_restore_roundtrips() {
    let Some(cx) = setup().await else {
        return;
    };
    let s = Uuid::new_v4().simple().to_string();
    let org = format!("org-{s}");
    let uid = format!("u-{s}");
    seed_org_member(&cx.authz, &org, &uid).await;
    let ctx = make_ctx(&org, &uid);

    // 階層: 親フォルダ / 子フォルダ ＋ ファイル。
    let parent = cx
        .service
        .create_folder(&ctx, None, "親フォルダ", None)
        .await
        .expect("親作成");
    let child = cx
        .service
        .create_folder(&ctx, Some(parent.id), "子フォルダ", None)
        .await
        .expect("子作成");
    let file = upload(&cx.service, &cx.http, &ctx, Some(parent.id), "f.txt", b"hi")
        .await
        .expect("ファイル");

    // 親をサブツリーごと論理削除する。
    cx.service
        .soft_delete_folder(&ctx, parent.id, None)
        .await
        .expect("削除");

    // ゴミ箱には「削除の根」＝親だけが出る（配下の子/ファイルは出ない）。
    let trash = cx
        .service
        .list_trash(&ctx, None, 50, None)
        .await
        .expect("trash");
    let trash_ids: Vec<Uuid> = trash.items.iter().map(|n| n.id).collect();
    assert!(
        trash_ids.contains(&parent.id),
        "削除の根 親が出る: {trash_ids:?}"
    );
    assert!(
        !trash_ids.contains(&child.id),
        "配下の子は根でないので出ない"
    );
    assert!(!trash_ids.contains(&file.id), "配下のファイルは出ない");

    // フォルダ復元（同一削除バッチを subtree 復元）。
    cx.service
        .restore_folder(&ctx, parent.id, None)
        .await
        .expect("復元");
    let trash2 = cx
        .service
        .list_trash(&ctx, None, 50, None)
        .await
        .expect("trash2");
    assert!(
        trash2.items.iter().all(|n| n.id != parent.id),
        "復元後はゴミ箱から消える"
    );

    // 配下（子フォルダ・ファイル）も生存し、一覧で見える。
    let children = cx
        .service
        .list_children(&ctx, Some(parent.id), Default::default(), None, 50, None)
        .await
        .expect("子一覧");
    let cids: Vec<Uuid> = children.items.iter().map(|n| n.id).collect();
    assert!(cids.contains(&child.id), "子フォルダが復活");
    assert!(cids.contains(&file.id), "ファイルが復活");
}

/// SAAS.2（#87）: purge_tenant がテナントの DB 行・FGA タプル・オブジェクトを整合的に
/// 撤去し、同一 org slug を共有する別テナントには一切触れないこと。冪等（再実行成功）。
#[tokio::test]
async fn purge_tenant_end_to_end() {
    let Some(cx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        store,
    } = cx;

    // 同一 org slug を 2 テナントで共有し、org 単位でなく tenant 単位の撤去であることを示す。
    let org = format!("itorg{}", Uuid::new_v4().simple());
    let ta = format!("ta{}", Uuid::new_v4().simple());
    let tb = format!("tb{}", Uuid::new_v4().simple());
    let ua = format!("ituser{}", Uuid::new_v4().simple());
    let ub = format!("ituser{}", Uuid::new_v4().simple());
    let ctx_a = make_ctx_tenant(&org, &ta, &ua);
    let ctx_b = make_ctx_tenant(&org, &tb, &ub);
    for c in [&ctx_a, &ctx_b] {
        authz
            .write_tuple(&c.subject(), Relation::Member, &c.ns().organization(&org))
            .await
            .expect("org member seed");
    }
    // A/B 各テナントにファイルと role タプル・directory 行を用意する。
    let file_a = upload(&service, &http, &ctx_a, None, "a.txt", b"tenant A data")
        .await
        .expect("upload A");
    let file_b = upload(&service, &http, &ctx_b, None, "b.txt", b"tenant B data")
        .await
        .expect("upload B");
    let dir = DirectoryStore::new(pool.clone());
    dir.upsert_role("dept", &ta, &org, "部署A")
        .await
        .expect("role A");
    dir.upsert_role("dept", &tb, &org, "部署B")
        .await
        .expect("role B");
    authz
        .write_tuple(&ctx_a.subject(), Relation::Member, &ctx_a.ns().role("dept"))
        .await
        .expect("role member A");
    authz
        .write_tuple(&ctx_b.subject(), Relation::Member, &ctx_b.ns().role("dept"))
        .await
        .expect("role member B");

    // --- A を purge ---
    let (tuples, objects) = service
        .purge_tenant(&ta, &org, "provisioner:test")
        .await
        .expect("purge A");
    assert!(tuples > 0, "A のタプルが剥奪されること（{tuples}）");
    assert!(objects > 0, "A のオブジェクトが削除されること（{objects}）");

    // DB: A の行が消え、B は残る。
    let a_nodes: i64 = sqlx::query_scalar("SELECT count(*) FROM node WHERE tenant_id = $1")
        .bind(&ta)
        .fetch_one(&pool)
        .await
        .unwrap();
    let b_nodes: i64 = sqlx::query_scalar("SELECT count(*) FROM node WHERE tenant_id = $1")
        .bind(&tb)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(a_nodes, 0, "A の node が撤去される");
    assert_eq!(b_nodes, 1, "B の node は残る");
    let a_blobs: i64 = sqlx::query_scalar("SELECT count(*) FROM blob WHERE tenant_id = $1")
        .bind(&ta)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(a_blobs, 0, "A の blob 行が撤去される");
    let a_roles: i64 =
        sqlx::query_scalar("SELECT count(*) FROM directory_role WHERE tenant_id = $1")
            .bind(&ta)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(a_roles, 0, "A の directory_role が撤去される");

    // FGA: A の owner/member check が deny になり、B は生きている。
    assert!(
        !authz
            .check(
                &ctx_a.subject(),
                Relation::Owner,
                &ctx_a.ns().file(&file_a.id.to_string()),
                Consistency::HigherConsistency,
            )
            .await
            .unwrap(),
        "A の file owner タプルが剥奪される"
    );
    assert!(
        !authz
            .check(
                &ctx_a.subject(),
                Relation::Member,
                &ctx_a.ns().organization(&org),
                Consistency::HigherConsistency,
            )
            .await
            .unwrap(),
        "A の org member タプルが剥奪される"
    );
    assert!(
        authz
            .check(
                &ctx_b.subject(),
                Relation::Owner,
                &ctx_b.ns().file(&file_b.id.to_string()),
                Consistency::HigherConsistency,
            )
            .await
            .unwrap(),
        "B の owner タプルは残る"
    );
    assert!(
        authz
            .check(
                &ctx_b.subject(),
                Relation::Member,
                &ctx_b.ns().role("dept"),
                Consistency::HigherConsistency,
            )
            .await
            .unwrap(),
        "B の role member タプルは残る（同名 role でも tenant 名前空間で分離）"
    );

    // オブジェクトストア: A のオブジェクトは消え、B は残る。
    let sha_a = sha256_hex(b"tenant A data");
    let sha_b = sha256_hex(b"tenant B data");
    assert!(
        !store
            .exists(&storage::content_address::blob_object_key(
                &ta, &org, &sha_a
            ))
            .await
            .unwrap(),
        "A の blob オブジェクトが削除される"
    );
    assert!(
        store
            .exists(&storage::content_address::blob_object_key(
                &tb, &org, &sha_b
            ))
            .await
            .unwrap(),
        "B の blob オブジェクトは残る"
    );

    // B のサービス経路も無傷（メタ取得成功）。
    assert!(service.get_metadata(&ctx_b, file_b.id, None).await.is_ok());

    // audit は保持され、purge の証跡が残る。
    let purge_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'tenant.purge'",
    )
    .bind(&ta)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(purge_audits, 1, "purge の監査エントリが残る");

    // --- 冪等: 再実行しても成功し、追加削除は 0。 ---
    let (tuples2, objects2) = service
        .purge_tenant(&ta, &org, "provisioner:test")
        .await
        .expect("purge 再実行");
    assert_eq!(tuples2, 0, "再実行で剥奪対象なし");
    assert_eq!(objects2, 0, "再実行で削除対象なし");
}

/// 内部バイト直書き/読み戻し（Task 4.12 Stage A・サンドボックス成果物経路）。
///
/// presigned 経路と同一不変条件（content-addressing・dedup・監査・書込イベント・viewer 認可）を
/// write_file_internal / read_file_internal が保つことを検証する。
#[tokio::test]
async fn internal_io_end_to_end() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let uid = format!("ituser{}", Uuid::new_v4().simple());
    let actx = make_ctx(&org, &uid);
    seed_org_member(&authz, &org, &uid).await;

    // --- ルート直下への内部書き込み → メタ/内容の round-trip ---
    let content = b"col_a,col_b\n1,2\n";
    let sha = sha256_hex(content);
    let node = service
        .write_file_internal(&actx, None, "result.csv", content, "text/csv", None)
        .await
        .expect("write_file_internal");
    assert_eq!(node.name, "result.csv");
    assert_eq!(node.blob_sha256.as_deref(), Some(sha.as_str()));
    assert_eq!(node.size_bytes, Some(content.len() as i64));
    assert_eq!(blob_refcount(&pool, &org, &sha).await, 1);
    assert_eq!(
        node_version_count(&pool, node.id).await,
        1,
        "初版が記録される"
    );
    assert_eq!(
        outbox_count(&pool, node.id, "create").await,
        1,
        "書込イベント（RAG 索引トリガ）が発行される"
    );
    assert_eq!(
        audit_count(&pool, &org, "file.write.internal", "allow").await,
        1
    );

    let (meta, bytes) = service
        .read_file_internal(&actx, node.id, None)
        .await
        .expect("read_file_internal");
    assert_eq!(meta.id, node.id);
    assert_eq!(bytes, content, "書いたバイトと読み戻しが一致する");
    assert_eq!(
        audit_count(&pool, &org, "file.read.internal", "allow").await,
        1
    );

    // presigned GET（既存ダウンロード経路）でも同一バイトが取れる（blob 共有の整合）。
    let ticket = service
        .issue_download_url(&actx, node.id, None)
        .await
        .expect("download url");
    let got = http
        .get(&ticket.url)
        .send()
        .await
        .expect("GET")
        .bytes()
        .await
        .expect("body");
    assert_eq!(got.as_ref(), content);

    // --- dedup: 同一内容の 2 個目は blob を共有し refcount +1 ---
    let node2 = service
        .write_file_internal(&actx, None, "copy.csv", content, "text/csv", None)
        .await
        .expect("2 個目");
    assert_eq!(node2.blob_sha256.as_deref(), Some(sha.as_str()));
    assert_eq!(blob_refcount(&pool, &org, &sha).await, 2);

    // --- フォルダ配下への書き込み（editor@folder 経路・parent tuple）---
    let folder = service
        .create_folder(&actx, None, "成果物", None)
        .await
        .expect("folder");
    let in_folder = service
        .write_file_internal(&actx, Some(folder.id), "n.txt", b"x", "text/plain", None)
        .await
        .expect("folder 配下");
    assert_eq!(in_folder.parent_id, Some(folder.id));
    assert_eq!(
        closure_depth(&pool, folder.id, in_folder.id).await,
        Some(1),
        "closure が親子を保持する"
    );

    // --- 不正名（ゲスト由来・PIT-23）は拒否 ---
    for bad in ["../escape", "a/b", "bad\nname", ".", ""] {
        let err = service
            .write_file_internal(&actx, None, bad, b"x", "text/plain", None)
            .await
            .expect_err("不正名は拒否");
        assert!(matches!(err, StorageError::Invalid(_)), "{bad:?}");
    }

    // --- 認可: 他人（org 非メンバー）は書けず、viewer が無ければ読めない ---
    let outsider = make_ctx(&org, &format!("outsider{}", Uuid::new_v4().simple()));
    let err = outsider_write(&service, &outsider).await;
    assert!(
        matches!(err, StorageError::Forbidden),
        "非メンバーの書き込みは deny"
    );
    let err = service
        .read_file_internal(&outsider, node.id, None)
        .await
        .expect_err("viewer 無しの読み取りは deny");
    // 読取 deny は存在秘匿のため NotFound（require_read の規約・他の読取系と同一）。
    assert!(matches!(err, StorageError::NotFound));
}

/// 非メンバーの内部書き込み（deny 経路）。
async fn outsider_write(service: &StorageService, ctx: &AuthContext) -> StorageError {
    service
        .write_file_internal(ctx, None, "deny.txt", b"x", "text/plain", None)
        .await
        .expect_err("deny")
}

#[tokio::test]
async fn write_file_internal_idempotent_dedups_by_key() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        ..
    } = ctx;
    let org = format!("itorg{}", Uuid::new_v4().simple());
    let uid = format!("ituser{}", Uuid::new_v4().simple());
    let actx = make_ctx(&org, &uid);
    seed_org_member(&authz, &org, &uid).await;

    let key = format!("wf:{}:r:step", Uuid::new_v4().simple());
    let digest = "deadbeef";

    // 1 回目: 書き込み成功（version 1）。
    let s1 = service
        .write_file_internal_idempotent(
            &actx,
            None,
            "idem.txt",
            b"payload",
            "text/plain",
            &key,
            digest,
            None,
        )
        .await
        .expect("first write");
    let id1 = s1.get("id").and_then(|v| v.as_str()).unwrap().to_string();

    // 2 回目: 同一冪等キー → dedup（新規ノードを作らず同じ要約を返す）。
    let s2 = service
        .write_file_internal_idempotent(
            &actx,
            None,
            "idem.txt",
            b"payload",
            "text/plain",
            &key,
            digest,
            None,
        )
        .await
        .expect("second write (dedup)");
    assert_eq!(s1, s2, "同一冪等キーは同じ結果要約を返す");

    // org 直下に "idem.txt" のノードは 1 つだけ（高々 1 バージョン）。
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM node WHERE org = $1 AND name = 'idem.txt' AND deleted_at IS NULL",
    )
    .bind(&org)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1, "冪等キー再送で 2 バージョン作らない");

    // 別の冪等キー → 別ノードが作られる。
    let key2 = format!("wf:{}:r:step2", Uuid::new_v4().simple());
    let s3 = service
        .write_file_internal_idempotent(
            &actx,
            None,
            "idem2.txt",
            b"payload",
            "text/plain",
            &key2,
            digest,
            None,
        )
        .await
        .expect("third write (new key)");
    assert_ne!(
        s3.get("id").and_then(|v| v.as_str()).unwrap(),
        id1,
        "別キーは別ノード"
    );

    // effect_journal に結果要約が記録されている（本文は含まない）。
    let summary: serde_json::Value = sqlx::query_scalar(
        "SELECT result_summary FROM effect_journal WHERE tenant_id = $1 AND idempotency_key = $2",
    )
    .bind(&actx.tenant_id)
    .bind(&key)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(summary.get("id").is_some());
    assert!(summary.get("body").is_none(), "journal に本文を載せない");
}

/// Task 5.4/5.8: `write_file_at` の create→update（新版）と outbox（Create/Update/Delete）を検証する。
/// 自律エージェントのワークスペース書込が版管理され、書込イベント→再索引経路に乗ることの土台。
#[tokio::test]
async fn write_file_at_versions_and_emits_outbox() {
    let Some(cx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        ..
    } = cx;
    let org = format!("org-{}", Uuid::new_v4());
    let uid = "alice";
    seed_org_member(&authz, &org, uid).await;
    let actx = make_ctx(&org, uid);

    // ワークスペースフォルダ（thread ごとの Drive フォルダ相当）。
    let root = service
        .create_folder(&actx, None, "agent-workspace", None)
        .await
        .expect("create workspace folder");

    // 1) 新規作成 → created=true, version 1, outbox Create。
    let w1 = service
        .write_file_at(&actx, root.id, "notes.md", b"# v1", "text/markdown", None)
        .await
        .expect("write create");
    assert!(w1.created, "初回は新規作成");
    assert_eq!(w1.version, 1);
    assert_eq!(outbox_count(&pool, w1.node_id, "create").await, 1);

    // 2) 同名再書込 → created=false, version 2（新版）, outbox Update。
    let w2 = service
        .write_file_at(
            &actx,
            root.id,
            "notes.md",
            b"# v2 updated",
            "text/markdown",
            None,
        )
        .await
        .expect("write update");
    assert!(!w2.created, "既存は新版更新");
    assert_eq!(w2.node_id, w1.node_id, "同一ノードの新版");
    assert_eq!(w2.version, 2);
    assert_eq!(outbox_count(&pool, w1.node_id, "update").await, 1);

    // 3) 名前解決 → 最新内容が読める（read-after-write・PIT-5）。
    let resolved = service
        .resolve_child_file(&actx, root.id, "notes.md", None)
        .await
        .expect("resolve")
        .expect("exists");
    assert_eq!(resolved, w1.node_id);
    let (_, bytes) = service
        .read_file_internal(&actx, resolved, None)
        .await
        .expect("read");
    assert_eq!(bytes, b"# v2 updated");

    // 4) 削除 → outbox Delete、解決不能になる。
    service
        .soft_delete_file(&actx, resolved, None)
        .await
        .expect("delete");
    assert_eq!(outbox_count(&pool, w1.node_id, "delete").await, 1);
    let after = service
        .resolve_child_file(&actx, root.id, "notes.md", None)
        .await
        .expect("resolve after delete");
    assert!(after.is_none(), "削除後は解決不能");
}

// --- 共有リンク（複数発行・個別失効/延長・#342） ---------------------------

/// organization / anyone リンクの read ゲート、owner ゲート、明示共有がリンク失効で剥奪されないこと。
#[tokio::test]
async fn share_link_levels_end_to_end() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;

    let member = format!("ituser{}", Uuid::new_v4().simple());
    let mctx = make_ctx(&org, &member);
    seed_org_member(&authz, &org, &member).await;
    let nonmember = format!("ituser{}", Uuid::new_v4().simple());
    let nmctx = make_ctx(&org, &nonmember);
    let other_org = format!("itorg{}", Uuid::new_v4().simple());
    let outsider = format!("ituser{}", Uuid::new_v4().simple());
    let octx_out = make_ctx(&other_org, &outsider);
    seed_org_member(&authz, &other_org, &outsider).await;

    let file = upload(&service, &http, &octx, None, "ga.txt", b"share link")
        .await
        .expect("upload");

    // リンク未発行（restricted 相当）: member/nonmember とも読めない。
    for c in [&mctx, &nmctx] {
        assert!(
            matches!(
                service.get_metadata(c, file.id, None).await,
                Err(StorageError::NotFound)
            ),
            "リンク未発行では読めない"
        );
    }

    // organization/viewer リンク: member は読める、nonmember は読めない。
    let l_org = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create org link");
    assert!(
        service.get_metadata(&mctx, file.id, None).await.is_ok(),
        "組織内メンバーは読める"
    );
    assert!(
        matches!(
            service.get_metadata(&nmctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "非メンバーは organization リンクでは読めない"
    );

    // anyone/viewer リンク（別リンク・A-2 で organization へ縮退）。broad_subject を organization#member に
    // 寄せたため、非メンバーは anyone でも読めない（user:* は将来の authenticated 用に予約）。
    service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Anyone,
            ShareRole::Viewer,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create anyone link");
    assert!(
        matches!(
            service.get_metadata(&nmctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "A-2: anyone は organization へ縮退。非メンバーは読めない"
    );
    assert!(
        matches!(
            service.get_metadata(&octx_out, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "別 org へは越境しない（DB tenant/org スコープ）"
    );

    // 明示共有した相手はリンク失効で剥奪されない。
    let pinned = format!("ituser{}", Uuid::new_v4().simple());
    let pctx = make_ctx(&org, &pinned);
    seed_org_member(&authz, &org, &pinned).await;
    service
        .share_node(
            &octx,
            file.id,
            &ShareTarget::User { id: pinned.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("explicit share");

    // 発行済みリンクを全て失効する。
    for l in service
        .list_share_links(&octx, file.id, None)
        .await
        .expect("list")
    {
        service
            .revoke_share_link(&octx, l.link_id, None)
            .await
            .expect("revoke");
    }
    let _ = l_org;
    // 失効後: リンク由来（組織メンバー mctx）は読めない、明示共有（pinned）は残る。
    assert!(
        matches!(
            service.get_metadata(&mctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "全失効でリンク由来アクセス（組織メンバー）は消える"
    );
    assert!(
        service.get_metadata(&pctx, file.id, None).await.is_ok(),
        "明示共有はリンク失効で剥奪されない"
    );

    // owner でない者は発行できない（Forbidden）。
    assert!(
        matches!(
            service
                .create_share_link(
                    &mctx,
                    file.id,
                    GeneralAccessLevel::Anyone,
                    ShareRole::Viewer,
                    None,
                    None,
                    None,
                    None
                )
                .await,
            Err(StorageError::Forbidden)
        ),
        "owner でない者はリンクを発行できない"
    );
}

/// パスワード付きリンク: broad タプルを書かず、token+パスワード検証後に per-user タプルを発行する。
#[tokio::test]
async fn share_link_password_redeem() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let member = format!("ituser{}", Uuid::new_v4().simple());
    let mctx = make_ctx(&org, &member);
    seed_org_member(&authz, &org, &member).await;
    let nonmember = format!("ituser{}", Uuid::new_v4().simple());
    let nmctx = make_ctx(&org, &nonmember);

    let file = upload(&service, &http, &octx, None, "secret.txt", b"pw protected")
        .await
        .expect("upload");

    // anyone/editor + password: broad タプル無し → member 読めない。
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Anyone,
            ShareRole::Editor,
            None,
            Some("s3cret-pass"),
            None,
            None,
        )
        .await
        .expect("create anyone+password");
    assert!(
        matches!(
            service.get_metadata(&mctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "パスワード付きは redeem 前は読めない（broad タプル無し）"
    );

    // 誤パスワードは Forbidden（オラクルにしない）。
    assert!(
        matches!(
            service
                .redeem_share_link(&mctx, &l.token, Some("wrong"), None)
                .await,
            Err(StorageError::Forbidden)
        ),
        "誤パスワードは Forbidden"
    );
    assert!(matches!(
        service.get_metadata(&mctx, file.id, None).await,
        Err(StorageError::NotFound)
    ));

    // 正しいパスワードで redeem → per-user タプル発行 → 読める。
    service
        .redeem_share_link(&mctx, &l.token, Some("s3cret-pass"), None)
        .await
        .expect("redeem");
    assert!(
        service.get_metadata(&mctx, file.id, None).await.is_ok(),
        "redeem 後は読める"
    );

    // organization + password リンク: 非メンバーは正パスワードでも redeem 不可。
    let l2 = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            Some("p2-pass"),
            None,
            None,
        )
        .await
        .expect("create organization+password");
    assert!(
        matches!(
            service
                .redeem_share_link(&nmctx, &l2.token, Some("p2-pass"), None)
                .await,
            Err(StorageError::Forbidden)
        ),
        "organization リンクは非メンバーだと redeem できない"
    );
    service
        .redeem_share_link(&mctx, &l2.token, Some("p2-pass"), None)
        .await
        .expect("member redeem");
    assert!(service.get_metadata(&mctx, file.id, None).await.is_ok());
}

/// 有効期限: 遅延失効（get_metadata 前段）とイベント駆動タイマの明示剥奪、次回起床時刻。
#[tokio::test]
async fn share_link_expiry() {
    use chrono::Utc;
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        pool,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let member = format!("ituser{}", Uuid::new_v4().simple());
    let mctx = make_ctx(&org, &member);
    seed_org_member(&authz, &org, &member).await;

    let file = upload(&service, &http, &octx, None, "exp.txt", b"expiring")
        .await
        .expect("upload");

    // 未来の期限: member は読める。
    let future = Utc::now() + chrono::Duration::hours(1);
    service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            Some(future),
            None,
            None,
            None,
        )
        .await
        .expect("create future expiry");
    assert!(
        service.get_metadata(&mctx, file.id, None).await.is_ok(),
        "期限内は読める"
    );

    // 次回失効時刻はこの期限を返す。
    let next = service.next_share_link_expiry().await.expect("next expiry");
    assert!(next.is_some(), "期限付きリンクがあるので次回失効時刻がある");

    // タイマが「未来+1h」時点を処理 → 剥奪されて読めなくなる。
    let count = service
        .revoke_expired_share_links(future + chrono::Duration::hours(1))
        .await
        .expect("revoke expired");
    assert!(count >= 1, "期限切れを 1 件以上剥奪する");
    assert!(
        matches!(
            service.get_metadata(&mctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "期限切れ剥奪後は読めない"
    );

    // 遅延失効（get_metadata 前段）: 未来期限で作った後に DB 上で期限を過去へ倒し、read 時に即失効
    // することを確認する（create は過去日を弾く＝B-5 のため、期限切れ状態は DB 直更新で用意する）。
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            Some(Utc::now() + chrono::Duration::hours(1)),
            None,
            None,
            None,
        )
        .await
        .expect("create future");
    assert!(
        service.get_metadata(&mctx, file.id, None).await.is_ok(),
        "作成直後（未来期限）は読める"
    );
    sqlx::query("UPDATE node_share_link SET expires_at = $2 WHERE link_id = $1")
        .bind(l.link_id)
        .bind(Utc::now() - chrono::Duration::seconds(1))
        .execute(&pool)
        .await
        .expect("expire via db");
    assert!(
        matches!(
            service.get_metadata(&mctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "過去期限は遅延失効（get_metadata 前段）で即読めない"
    );
}

/// 明示共有を持つユーザーが redeem しても、その明示共有はリンク失効で誤剥奪されない（台帳ゲート）。
#[tokio::test]
async fn share_link_preserves_explicit_share() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let member = format!("ituser{}", Uuid::new_v4().simple());
    let mctx = make_ctx(&org, &member);
    seed_org_member(&authz, &org, &member).await;

    let file = upload(
        &service,
        &http,
        &octx,
        None,
        "explicit.txt",
        b"explicit + link",
    )
    .await
    .expect("upload");

    // owner が member に明示 viewer 共有 → member は読める。
    service
        .share_node(
            &octx,
            file.id,
            &ShareTarget::User { id: member.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("explicit share");
    assert!(service.get_metadata(&mctx, file.id, None).await.is_ok());

    // anyone/viewer + password リンク → member が redeem（既に viewer なので no-op 付与）。
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Anyone,
            ShareRole::Viewer,
            None,
            Some("pw-x"),
            None,
            None,
        )
        .await
        .expect("create anyone+password");
    service
        .redeem_share_link(&mctx, &l.token, Some("pw-x"), None)
        .await
        .expect("redeem (no-op grant)");
    assert!(service.get_metadata(&mctx, file.id, None).await.is_ok());

    // リンクを失効 → 明示 viewer は残る（redeem 台帳ゲートで誤剥奪されない）。
    service
        .revoke_share_link(&octx, l.link_id, None)
        .await
        .expect("revoke");
    assert!(
        service.get_metadata(&mctx, file.id, None).await.is_ok(),
        "明示共有はリンク失効を経ても残る（granted ゲートで台帳に載らないため）"
    );
}

/// 参照カウント: 同じ FGA タプルへ写る 2 本のリンクが、1 本失効しても他が生存すればアクセスを
/// 維持し、両方失効で初めてタプルが消える。
///
/// A-1（重複発行ブロック）により **同一 (audience, role)** の非パスワードリンクは 2 本作れない。
/// ここでは A-2 で `organization` と `anyone` がともに `organization#member` へ縮退することを利用し、
/// **異なる audience で同一 subject に写る** 2 本（organization/viewer と anyone/viewer）で参照カウントを
/// 検証する。UI からは同一 subject の重複も基本的に発生しないが、reconcile の参照カウントは
/// この経路（旧データ／API 直叩き）で依然として正しく振る舞う必要がある。
#[tokio::test]
async fn share_link_ref_count_keeps_tuple() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let member = format!("ituser{}", Uuid::new_v4().simple());
    let mctx = make_ctx(&org, &member);
    seed_org_member(&authz, &org, &member).await;

    let file = upload(&service, &http, &octx, None, "refcount.txt", b"ref count")
        .await
        .expect("upload");

    // 同一 subject（organization#member viewer）へ写る 2 本を、異なる audience で発行する
    // （A-2 で anyone は organization へ縮退。A-1 の重複ブロックは (audience, role) 単位なので通る）。
    let l1 = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("l1");
    let l2 = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Anyone,
            ShareRole::Viewer,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("l2");
    assert!(
        service.get_metadata(&mctx, file.id, None).await.is_ok(),
        "2 本発行で読める"
    );

    // L1 失効 → L2 が同じ organization#member viewer タプルを保持 → まだ読める。
    service
        .revoke_share_link(&octx, l1.link_id, None)
        .await
        .expect("revoke l1");
    assert!(
        service.get_metadata(&mctx, file.id, None).await.is_ok(),
        "同 audience の他リンクが生存 → 読める（参照カウント）"
    );

    // L2 も失効 → タプル消滅 → 読めない。
    service
        .revoke_share_link(&octx, l2.link_id, None)
        .await
        .expect("revoke l2");
    assert!(
        matches!(
            service.get_metadata(&mctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "全失効でタプル消滅"
    );
}

/// RAG completeness: broad/redeem 共有した file が `list_objects(Viewer, File)` に出る
/// （pre-filter が `organization#member` を展開して取りこぼさないことを実証）。A-2 で broad は
/// すべて organization#member へ寄せたため、非メンバーには出ない（`user:*` は張らない）。
#[tokio::test]
async fn share_link_list_objects_completeness() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let member = format!("ituser{}", Uuid::new_v4().simple());
    let mctx = make_ctx(&org, &member);
    seed_org_member(&authz, &org, &member).await;
    let nonmember = format!("ituser{}", Uuid::new_v4().simple());
    let nmctx = make_ctx(&org, &nonmember);

    let file = upload(&service, &http, &octx, None, "rag.txt", b"rag")
        .await
        .expect("upload");
    let file_obj = octx.ns().file(&file.id.to_string()).as_str().to_string();

    // organization リンク → member の list_objects に出る、nonmember には出ない。
    service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("org link");
    let m_objs = authz
        .list_objects(&mctx.subject(), Relation::Viewer, ObjectType::File)
        .await
        .expect("list member");
    assert!(
        m_objs.contains(&file_obj),
        "organization リンクの file が member の list_objects に出る（RAG pre-filter 完全性）"
    );
    let nm_objs = authz
        .list_objects(&nmctx.subject(), Relation::Viewer, ObjectType::File)
        .await
        .expect("list nonmember");
    assert!(
        !nm_objs.contains(&file_obj),
        "非メンバーには organization リンクの file は出ない"
    );

    // anyone リンク（A-2 で organization へ縮退）→ 非メンバーには依然出ない（user:* を張らないため）。
    // organization リンクは既に上で member に出ることを確認済み（broad の完全性はそれで担保）。
    service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Anyone,
            ShareRole::Viewer,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("anyone link");
    let nm_objs2 = authz
        .list_objects(&nmctx.subject(), Relation::Viewer, ObjectType::File)
        .await
        .expect("list nonmember 2");
    assert!(
        !nm_objs2.contains(&file_obj),
        "A-2: anyone は organization#member へ縮退。非メンバーの list_objects には出ない"
    );

    // パスワードリンク（別 file）: redeem 前は出ず、redeem 後に出る。
    let file2 = upload(&service, &http, &octx, None, "rag2.txt", b"pw rag")
        .await
        .expect("upload2");
    let file2_obj = octx.ns().file(&file2.id.to_string()).as_str().to_string();
    let lp = service
        .create_share_link(
            &octx,
            file2.id,
            GeneralAccessLevel::Anyone,
            ShareRole::Viewer,
            None,
            Some("rag-pw"),
            None,
            None,
        )
        .await
        .expect("pw link");
    let before = authz
        .list_objects(&mctx.subject(), Relation::Viewer, ObjectType::File)
        .await
        .expect("before redeem");
    assert!(
        !before.contains(&file2_obj),
        "パスワードリンクは redeem 前は list_objects に出ない"
    );
    service
        .redeem_share_link(&mctx, &lp.token, Some("rag-pw"), None)
        .await
        .expect("redeem");
    let after = authz
        .list_objects(&mctx.subject(), Relation::Viewer, ObjectType::File)
        .await
        .expect("after redeem");
    assert!(
        after.contains(&file2_obj),
        "redeem 後は list_objects に出る"
    );
}

/// redeem は link の org を跨げない（Codex P1 / B-4）: 別 org（同テナント）のユーザーは正しい
/// パスワードでも token を使えない。`anyone` audience でも org を跨がせない。
#[tokio::test]
async fn share_link_redeem_rejects_cross_org() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;

    // 別 org（同テナント default）のユーザー。
    let other_org = format!("itorg{}", Uuid::new_v4().simple());
    let outsider = format!("ituser{}", Uuid::new_v4().simple());
    let octx_out = make_ctx(&other_org, &outsider);
    seed_org_member(&authz, &other_org, &outsider).await;

    let file = upload(&service, &http, &octx, None, "xorg.txt", b"cross org")
        .await
        .expect("upload");

    // anyone + password リンク（org に属す）。
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Anyone,
            ShareRole::Viewer,
            None,
            Some("x-pass"),
            None,
            None,
        )
        .await
        .expect("create");

    // 別 org のユーザーは正しいパスワードでも redeem 不可（org 不一致で token が引けない）。
    assert!(
        matches!(
            service
                .redeem_share_link(&octx_out, &l.token, Some("x-pass"), None)
                .await,
            Err(StorageError::Forbidden)
        ),
        "別 org のユーザーは anyone リンクでも redeem できない（org スコープ・Codex P1）"
    );
    assert!(matches!(
        service.get_metadata(&octx_out, file.id, None).await,
        Err(StorageError::NotFound)
    ));
}

/// 過去日の期限は 400（B-5）: create / extend とも未来でない期限を拒否する。
#[tokio::test]
async fn share_link_rejects_past_expiry() {
    use chrono::Utc;
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;

    let file = upload(&service, &http, &octx, None, "pastexp.txt", b"past")
        .await
        .expect("upload");

    let past = Utc::now() - chrono::Duration::hours(1);
    assert!(
        matches!(
            service
                .create_share_link(
                    &octx,
                    file.id,
                    GeneralAccessLevel::Organization,
                    ShareRole::Viewer,
                    Some(past),
                    None,
                    None,
                    None
                )
                .await,
            Err(StorageError::Invalid(_))
        ),
        "過去日の create は 400"
    );

    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    assert!(
        matches!(
            service
                .extend_share_link(&octx, l.link_id, Some(past), None)
                .await,
            Err(StorageError::Invalid(_))
        ),
        "過去日の extend は 400"
    );
}

/// A-1: 同一 (audience, role) の非パスワードリンクは重複発行できない（409）。role 違い・
/// パスワード付きは capability として重複可。
#[tokio::test]
async fn share_link_blocks_duplicate_issuance() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;

    let file = upload(&service, &http, &octx, None, "dup.txt", b"dup")
        .await
        .expect("upload");

    service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("first");
    // 同一 (organization, viewer) の 2 本目は Conflict。
    assert!(
        matches!(
            service
                .create_share_link(
                    &octx,
                    file.id,
                    GeneralAccessLevel::Organization,
                    ShareRole::Viewer,
                    None,
                    None,
                    None,
                    None
                )
                .await,
            Err(StorageError::Conflict)
        ),
        "同一 (audience, role) の非パスワードリンクは重複発行不可（A-1）"
    );
    // role 違いは許可。
    service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Editor,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("role 違いは可");
    // パスワード付きは per-user capability として重複可（redeem 台帳で分離）。
    for pw in ["p1", "p2"] {
        service
            .create_share_link(
                &octx,
                file.id,
                GeneralAccessLevel::Organization,
                ShareRole::Viewer,
                None,
                Some(pw),
                None,
                None,
            )
            .await
            .expect("password 付きは重複可");
    }
}

/// B-2 根治（#366）: redeem 先行 → 明示共有 → リンク失効 の順でも明示共有は残る。redeem 由来は
/// via_link 専用 relation（viewer_via_link）で発行され、失効時の reconcile はそれのみを剥奪するため、
/// 明示共有の viewer タプル（別 relation）には決して触れない。
#[tokio::test]
async fn share_link_explicit_share_after_redeem_survives() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let member = format!("ituser{}", Uuid::new_v4().simple());
    let mctx = make_ctx(&org, &member);
    seed_org_member(&authz, &org, &member).await;

    let file = upload(
        &service,
        &http,
        &octx,
        None,
        "revorder.txt",
        b"reverse order",
    )
    .await
    .expect("upload");

    // 1) member が anyone+password リンクを redeem（viewer_via_link を発行・台帳に載る）。
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Anyone,
            ShareRole::Viewer,
            None,
            Some("pw-x"),
            None,
            None,
        )
        .await
        .expect("create");
    service
        .redeem_share_link(&mctx, &l.token, Some("pw-x"), None)
        .await
        .expect("redeem");
    // 2) owner が同じ member へ明示 viewer 共有（viewer タプル＝via_link とは別 relation を張る）。
    service
        .share_node(
            &octx,
            file.id,
            &ShareTarget::User { id: member.clone() },
            ShareRole::Viewer,
            None,
        )
        .await
        .expect("explicit share");
    // 3) リンク失効 → reconcile は via_link タプルのみ剥奪。明示 viewer タプルは別 relation で残る。
    service
        .revoke_share_link(&octx, l.link_id, None)
        .await
        .expect("revoke");
    assert!(
        service.get_metadata(&mctx, file.id, None).await.is_ok(),
        "明示共有は redeem 先行順でもリンク失効で消えてはならない（B-2 根治・#366）"
    );
}

/// C-3（#369・可視化専用）: redeem 済み user の一覧と `redeem_count` 集計。owner ゲートを検証する。
/// per-user の個別取り消しは durable な deny 台帳を要するため別 issue（本テストは可視化のみ）。
#[tokio::test]
async fn share_link_grant_list_and_count() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let m1 = format!("ituser{}", Uuid::new_v4().simple());
    let m1ctx = make_ctx(&org, &m1);
    seed_org_member(&authz, &org, &m1).await;
    let m2 = format!("ituser{}", Uuid::new_v4().simple());
    let m2ctx = make_ctx(&org, &m2);
    seed_org_member(&authz, &org, &m2).await;

    let file = upload(&service, &http, &octx, None, "grants.txt", b"g")
        .await
        .expect("upload");

    // password リンクを 2 人が redeem。
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            Some("pw-y"),
            None,
            None,
        )
        .await
        .expect("create");
    service
        .redeem_share_link(&m1ctx, &l.token, Some("pw-y"), None)
        .await
        .expect("m1 redeem");
    service
        .redeem_share_link(&m2ctx, &l.token, Some("pw-y"), None)
        .await
        .expect("m2 redeem");

    // 一覧に 2 人・redeem_count == 2。
    let grants = service
        .list_share_link_grants(&octx, l.link_id, None)
        .await
        .expect("list grants");
    assert_eq!(grants.len(), 2, "2 人が解錠済み");
    let mut listed: Vec<&str> = grants.iter().map(|g| g.user_id.as_str()).collect();
    listed.sort_unstable();
    let mut want = [m1.as_str(), m2.as_str()];
    want.sort_unstable();
    assert_eq!(listed, want, "解錠した 2 人が一覧に出る");

    let links = service
        .list_share_links(&octx, file.id, None)
        .await
        .expect("list links");
    assert_eq!(
        links
            .iter()
            .find(|x| x.link_id == l.link_id)
            .unwrap()
            .redeem_count,
        2,
        "redeem_count が 2"
    );

    // 非 owner は grant 一覧を取得できない（owner ゲート）。
    assert!(
        matches!(
            service
                .list_share_link_grants(&m1ctx, l.link_id, None)
                .await,
            Err(StorageError::Forbidden)
        ),
        "非 owner の grant 一覧は Forbidden"
    );
}

/// #375 の本丸: per-user 個別取消が **durable**（同じ URL＋パスワードで再解錠できない）。
/// PR #373 はここが担保できず（リンクが active なままだと再 redeem で復元できた）取消を撤去した。
#[tokio::test]
async fn share_link_grant_revoke_is_durable() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;

    let file = upload(&service, &http, &octx, None, "durable.txt", b"d")
        .await
        .expect("upload");
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            Some("pw-durable"),
            None,
            None,
        )
        .await
        .expect("create");
    service
        .redeem_share_link(&bctx, &l.token, Some("pw-durable"), None)
        .await
        .expect("redeem");
    assert!(
        service.get_metadata(&bctx, file.id, None).await.is_ok(),
        "解錠すると読める"
    );

    // 非 owner は他人の付与を取り消せない（owner ゲート）。
    assert!(
        matches!(
            service
                .revoke_share_link_grant(&bctx, l.link_id, &bob, None)
                .await,
            Err(StorageError::Forbidden)
        ),
        "非 owner の個別取消は Forbidden"
    );

    service
        .revoke_share_link_grant(&octx, l.link_id, &bob, None)
        .await
        .expect("revoke grant");
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "取消でアクセスを失う（存在秘匿）"
    );

    // ★ #375 の核心: リンクは active なまま。同じ token＋同じパスワードでも再解錠できない。
    assert!(
        matches!(
            service
                .redeem_share_link(&bctx, &l.token, Some("pw-durable"), None)
                .await,
            Err(StorageError::Forbidden)
        ),
        "取消済み user は同じ URL＋パスワードで再 redeem できない（deny 台帳）"
    );
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "拒否された再 redeem は via_link タプルを張っていない"
    );
    // 台帳行は残り、ソフト失効している（deny 台帳の実体）。
    assert!(
        matches!(
            grant_revoked_at(&pool, l.link_id, &bob).await,
            Some(Some(_))
        ),
        "grant 行は消さずソフト失効させる"
    );

    // owner から見た可視化にも出ない（取消の成否が読み取れる）。
    assert!(
        service
            .list_share_link_grants(&octx, l.link_id, None)
            .await
            .expect("list grants")
            .is_empty(),
        "取消済みは解錠済み一覧に出ない"
    );
    let links = service
        .list_share_links(&octx, file.id, None)
        .await
        .expect("list links");
    assert_eq!(
        links
            .iter()
            .find(|x| x.link_id == l.link_id)
            .expect("link is still active")
            .redeem_count,
        0,
        "redeem_count が減る（owner が取消の成否を知る唯一のフィードバック）"
    );

    // 2 度目の取消は冪等成功（監査は増やさない）。
    service
        .revoke_share_link_grant(&octx, l.link_id, &bob, None)
        .await
        .expect("idempotent revoke");
    assert_eq!(
        audit_count(&pool, &org, "node.share_link.grant.revoke", "allow").await,
        1,
        "冪等な no-op は監査を増やさない"
    );
    assert!(
        audit_count(&pool, &org, "node.share_link.redeem", "deny").await >= 1,
        "拒否した再 redeem は deny 監査に残る"
    );
}

/// 個別取消は対象ユーザーだけを落とし、同じリンクの他の解錠者には影響しない。
#[tokio::test]
async fn share_link_grant_revoke_keeps_other_users() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let m1 = format!("ituser{}", Uuid::new_v4().simple());
    let m1ctx = make_ctx(&org, &m1);
    seed_org_member(&authz, &org, &m1).await;
    let m2 = format!("ituser{}", Uuid::new_v4().simple());
    let m2ctx = make_ctx(&org, &m2);
    seed_org_member(&authz, &org, &m2).await;

    let file = upload(&service, &http, &octx, None, "keep.txt", b"k")
        .await
        .expect("upload");
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            Some("pw-keep"),
            None,
            None,
        )
        .await
        .expect("create");
    for c in [&m1ctx, &m2ctx] {
        service
            .redeem_share_link(c, &l.token, Some("pw-keep"), None)
            .await
            .expect("redeem");
    }

    service
        .revoke_share_link_grant(&octx, l.link_id, &m1, None)
        .await
        .expect("revoke m1");
    assert!(
        matches!(
            service.get_metadata(&m1ctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "取消した m1 は読めない"
    );
    assert!(
        service.get_metadata(&m2ctx, file.id, None).await.is_ok(),
        "m2 のアクセスは無傷（per-user 剥奪が他人を巻き込まない）"
    );
    let grants = service
        .list_share_link_grants(&octx, l.link_id, None)
        .await
        .expect("list grants");
    assert_eq!(grants.len(), 1, "一覧は m2 のみ");
    assert_eq!(grants[0].user_id, m2);
    let links = service
        .list_share_links(&octx, file.id, None)
        .await
        .expect("list links");
    assert_eq!(
        links
            .iter()
            .find(|x| x.link_id == l.link_id)
            .expect("link")
            .redeem_count,
        1,
        "redeem_count は 1"
    );
}

/// deny のスコープは **per-(link,user)**。あるリンクで取り消しても別リンクの付与は生き、
/// 逆に別リンク経由でアクセスがあっても取消したリンクからは再解錠できない。
///
/// これは参照カウントに `g.revoked_at IS NULL` を課すことの直接の回帰テスト。述語を落とすと、
/// リンク A の revoked 行を数えて `remaining = 1` になり、B 失効後も via_link タプルが残る（fail-open）。
#[tokio::test]
async fn share_link_grant_revoke_scope_is_per_link() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;

    let file = upload(&service, &http, &octx, None, "perlink.txt", b"p")
        .await
        .expect("upload");
    // パスワード付きリンクは per-user capability なので同一 (audience, role) の複数発行を許す（A-1）。
    let mut links = Vec::new();
    for pw in ["pw-a", "pw-b"] {
        links.push(
            service
                .create_share_link(
                    &octx,
                    file.id,
                    GeneralAccessLevel::Organization,
                    ShareRole::Viewer,
                    None,
                    Some(pw),
                    None,
                    None,
                )
                .await
                .expect("create"),
        );
    }
    let (a, b) = (&links[0], &links[1]);
    service
        .redeem_share_link(&bctx, &a.token, Some("pw-a"), None)
        .await
        .expect("redeem a");
    service
        .redeem_share_link(&bctx, &b.token, Some("pw-b"), None)
        .await
        .expect("redeem b");

    // A の付与を取消 → B が同じ (node,user,role) を保持しているのでタプルは残る（参照カウント）。
    service
        .revoke_share_link_grant(&octx, a.link_id, &bob, None)
        .await
        .expect("revoke grant on a");
    assert!(
        service.get_metadata(&bctx, file.id, None).await.is_ok(),
        "B の付与が生きているので読める（参照カウント）"
    );
    // ただし A からは再解錠できない（deny は per-link・別リンクのアクセス有無に依らない）。
    assert!(
        matches!(
            service
                .redeem_share_link(&bctx, &a.token, Some("pw-a"), None)
                .await,
            Err(StorageError::Forbidden)
        ),
        "A の deny は B 経由のアクセスがあっても効く（per-link）"
    );

    // B も取消 → 最後の live grant が消えるのでタプルが剥奪される。
    service
        .revoke_share_link_grant(&octx, b.link_id, &bob, None)
        .await
        .expect("revoke grant on b");
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "全 live grant が消えて via_link タプルが剥奪される（revoked 行を数えていない証拠）"
    );
}

/// リンクごとの失効では台帳行を**削除**し（再 redeem が構造的に不可能なので deny 行が無意味・
/// 墓石を溜めない）、**active な別リンクの deny 行は残す**（そちらは再 redeem を拒否するために必要）。
/// 墓石掃除と durability の両立が壊れていないことを 1 本で押さえる（Codex P2）。
#[tokio::test]
async fn share_link_revoke_clears_grants_but_keeps_deny_on_active_link() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;

    let file = upload(&service, &http, &octx, None, "tomb.txt", b"t")
        .await
        .expect("upload");
    let mut links = Vec::new();
    for pw in ["pw-t1", "pw-t2"] {
        links.push(
            service
                .create_share_link(
                    &octx,
                    file.id,
                    GeneralAccessLevel::Organization,
                    ShareRole::Viewer,
                    None,
                    Some(pw),
                    None,
                    None,
                )
                .await
                .expect("create"),
        );
    }
    let (a, b) = (links[0].clone(), links[1].clone());
    service
        .redeem_share_link(&bctx, &a.token, Some("pw-t1"), None)
        .await
        .expect("redeem a");
    service
        .redeem_share_link(&bctx, &b.token, Some("pw-t2"), None)
        .await
        .expect("redeem b");

    // A は **active のまま** bob を個別取消 → deny 行が残る（durability に必要）。
    service
        .revoke_share_link_grant(&octx, a.link_id, &bob, None)
        .await
        .expect("revoke grant on a");
    assert!(
        matches!(
            grant_revoked_at(&pool, a.link_id, &bob).await,
            Some(Some(_))
        ),
        "active リンクの個別取消は deny 行を残す"
    );

    // B はリンクごと失効 → 台帳行は削除される（墓石を残さない）。
    service
        .revoke_share_link(&octx, b.link_id, None)
        .await
        .expect("revoke link b");
    assert!(
        grant_revoked_at(&pool, b.link_id, &bob).await.is_none(),
        "失効したリンクの台帳行は削除される（再 redeem が構造的に不可能なので deny 行は不要）"
    );
    // A の deny 行は無傷で、A からの再解錠は依然として拒否される。
    assert!(
        matches!(
            grant_revoked_at(&pool, a.link_id, &bob).await,
            Some(Some(_))
        ),
        "B の失効掃除が A の deny 行を巻き込まない"
    );
    assert!(
        matches!(
            service
                .redeem_share_link(&bctx, &a.token, Some("pw-t1"), None)
                .await,
            Err(StorageError::Forbidden)
        ),
        "A の個別取消は durable なまま"
    );
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "両経路でアクセスを失う"
    );
}

/// リンク失効時に台帳行が削除され、外形的な振る舞い（一覧・アクセス）が保たれる。
#[tokio::test]
async fn share_link_revoke_soft_marks_grants() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;

    let file = upload(&service, &http, &octx, None, "soft.txt", b"s")
        .await
        .expect("upload");
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            Some("pw-soft"),
            None,
            None,
        )
        .await
        .expect("create");
    service
        .redeem_share_link(&bctx, &l.token, Some("pw-soft"), None)
        .await
        .expect("redeem");
    assert!(matches!(
        grant_revoked_at(&pool, l.link_id, &bob).await,
        Some(None)
    ));

    service
        .revoke_share_link(&octx, l.link_id, None)
        .await
        .expect("revoke link");
    assert!(
        grant_revoked_at(&pool, l.link_id, &bob).await.is_none(),
        "リンク失効で台帳行は削除される（墓石を溜めない）"
    );
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "リンク失効でアクセスを失う"
    );
    // 失効リンクは owner の一覧から消えるので、可視化 API 側の見え方は grant 一覧で確認する。
    assert!(
        service
            .list_share_link_grants(&octx, l.link_id, None)
            .await
            .expect("list grants")
            .is_empty(),
        "失効後の解錠済み一覧は空"
    );
    // 失効済みリンクは token 引きの active 述語で弾かれる（再 redeem は構造的に不可能）。
    assert!(
        matches!(
            service
                .redeem_share_link(&bctx, &l.token, Some("pw-soft"), None)
                .await,
            Err(StorageError::Forbidden)
        ),
        "失効リンクは deny 行が無くても再 redeem できない"
    );
}

/// #376 の決定的レーステスト: redeem が「失効の後」に入った場合、必ず拒否される。
///
/// 別コネクションで node の advisory lock を掴んでから redeem を走らせ、`pg_locks` で
/// **ロック待ちに入ったこと**（＝検証を通過して付与直前まで来たこと）を確認してから失効を確定する。
/// 直列化が無いコードでは redeem がロックを取らないので `await_advisory_wait` が panic して落ちる。
#[tokio::test]
async fn share_link_redeem_denied_after_concurrent_revoke() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;

    let file = upload(&service, &http, &octx, None, "race.txt", b"r")
        .await
        .expect("upload");
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            Some("pw-race"),
            None,
            None,
        )
        .await
        .expect("create");

    let key = share_link_lock_key(&pool, file.id).await;
    let mut hold = pool.begin().await.expect("hold tx");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *hold)
        .await
        .expect("take advisory lock");

    let service = Arc::new(service);
    let task = tokio::spawn({
        let s = Arc::clone(&service);
        let c = bctx.clone();
        let token = l.token.clone();
        async move { s.redeem_share_link(&c, &token, Some("pw-race"), None).await }
    });

    // redeem が検証を終えてロック待ちに入るまで待つ（決定的な同期点）。
    await_advisory_wait(&pool, key).await;
    // ロック保持下でリンクを失効させ、コミットしてロックを解放する。
    sqlx::query("UPDATE node_share_link SET revoked_at = now() WHERE link_id = $1")
        .bind(l.link_id)
        .execute(&mut *hold)
        .await
        .expect("revoke while holding lock");
    hold.commit().await.expect("commit hold");

    assert!(
        matches!(
            task.await.expect("join redeem"),
            Err(StorageError::Forbidden)
        ),
        "失効後にロックを取れた redeem は拒否される（TOCTOU が閉じている）"
    );
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "失効済みリンクからアクセスが残らない"
    );
    // 孤児タプル（台帳から参照されない via_link）が無いことを FGA に直接問う。
    assert!(
        !authz
            .check(
                &bctx.subject(),
                Relation::Viewer,
                &bctx.ns().file(&file.id.to_string()),
                Consistency::HigherConsistency,
            )
            .await
            .expect("fga check"),
        "孤児 via_link タプルが残っていない"
    );
    assert!(
        grant_revoked_at(&pool, l.link_id, &bob).await.is_none(),
        "拒否された redeem は台帳行を作らない"
    );
}

/// 同一ユーザーが 2 本のリンクを**同時に** redeem しても参照カウントが壊れない（#376）。
///
/// 順序をこじ開けず、advisory lock による直列化のおかげで**どちらの順序でも成立する不変条件**を
/// 検証する（だから flaky にならない）。直列化前は `prior` の読みが tx 外だったため、両者が
/// 「行なし」を観測して片方の台帳行が落ち、片方のリンク失効でアクセスを失い得た。
#[tokio::test]
async fn share_link_concurrent_redeem_two_links_refcount() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;

    let file = upload(&service, &http, &octx, None, "conc.txt", b"c")
        .await
        .expect("upload");
    let mut links = Vec::new();
    for pw in ["pw-c1", "pw-c2"] {
        links.push(
            service
                .create_share_link(
                    &octx,
                    file.id,
                    GeneralAccessLevel::Organization,
                    ShareRole::Viewer,
                    None,
                    Some(pw),
                    None,
                    None,
                )
                .await
                .expect("create"),
        );
    }
    let (a, b) = (links[0].clone(), links[1].clone());

    let service = Arc::new(service);
    let (ra, rb) = tokio::join!(
        {
            let s = Arc::clone(&service);
            let c = bctx.clone();
            let token = a.token.clone();
            async move { s.redeem_share_link(&c, &token, Some("pw-c1"), None).await }
        },
        {
            let s = Arc::clone(&service);
            let c = bctx.clone();
            let token = b.token.clone();
            async move { s.redeem_share_link(&c, &token, Some("pw-c2"), None).await }
        }
    );
    ra.expect("redeem a");
    rb.expect("redeem b");

    // リンクごとに live な台帳行が 1 本ずつある（どちらの順序でも同じ）。
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM node_share_link_grant \
         WHERE node_id = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(file.id)
    .bind(&bob)
    .fetch_one(&pool)
    .await
    .expect("count live grants");
    assert_eq!(live, 2, "同時 redeem でもリンクごとに台帳行が残る");

    assert!(service.get_metadata(&bctx, file.id, None).await.is_ok());
    service
        .revoke_share_link(&octx, a.link_id, None)
        .await
        .expect("revoke a");
    assert!(
        service.get_metadata(&bctx, file.id, None).await.is_ok(),
        "A 失効でも B の付与が保持（参照カウント）"
    );
    service
        .revoke_share_link(&octx, b.link_id, None)
        .await
        .expect("revoke b");
    assert!(
        matches!(
            service.get_metadata(&bctx, file.id, None).await,
            Err(StorageError::NotFound)
        ),
        "両方失効でタプル消滅"
    );
}

/// 期限失効 sweep とリンク延長が競合したとき、延長されたリンクの解錠者を deny 台帳へ載せない。
///
/// grant をソフト失効させる（#375）ようになったため、sweep が「延長で active に戻ったリンク」の
/// grant に触ると、その全員が**二度と再 redeem できない**永久追放になる。sweep は期限の再確認
/// UPDATE を per-user 剥奪より先に行い、0 行だったリンクには触れてはならない。
#[tokio::test]
async fn share_link_expire_race_with_extend_keeps_grants() {
    use chrono::Utc;
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;

    let file = upload(&service, &http, &octx, None, "exprace.txt", b"e")
        .await
        .expect("upload");
    let expires = Utc::now() + chrono::Duration::hours(1);
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            Some(expires),
            Some("pw-exprace"),
            None,
            None,
        )
        .await
        .expect("create");
    service
        .redeem_share_link(&bctx, &l.token, Some("pw-exprace"), None)
        .await
        .expect("redeem");

    // sweep から見て期限切れに見える論理時刻。
    let sweep_now = expires + chrono::Duration::hours(1);
    let key = share_link_lock_key(&pool, file.id).await;
    let mut hold = pool.begin().await.expect("hold tx");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *hold)
        .await
        .expect("take advisory lock");

    let service = Arc::new(service);
    let task = tokio::spawn({
        let s = Arc::clone(&service);
        async move { s.revoke_expired_share_links(sweep_now).await }
    });

    // sweep が期限切れリンクを拾ってロック待ちに入るまで待つ。
    await_advisory_wait(&pool, key).await;
    // ロック保持下で延長（sweep が見た期限はもう古い）。
    sqlx::query("UPDATE node_share_link SET expires_at = $2 WHERE link_id = $1")
        .bind(l.link_id)
        .bind(sweep_now + chrono::Duration::hours(1))
        .execute(&mut *hold)
        .await
        .expect("extend while holding lock");
    hold.commit().await.expect("commit hold");
    task.await.expect("join sweep").expect("sweep ok");

    // 延長されたリンクは active のまま・解錠者のアクセスと台帳は無傷。
    let links = service
        .list_share_links(&octx, file.id, None)
        .await
        .expect("list links");
    assert!(
        links.iter().any(|x| x.link_id == l.link_id),
        "延長されたリンクは失効していない"
    );
    assert!(
        service.get_metadata(&bctx, file.id, None).await.is_ok(),
        "延長されたリンクの解錠者はアクセスを保つ"
    );
    assert!(
        matches!(grant_revoked_at(&pool, l.link_id, &bob).await, Some(None)),
        "延長されたリンクの grant は deny 台帳へ載らない（永久追放を作らない）"
    );
    assert!(
        service
            .redeem_share_link(&bctx, &l.token, Some("pw-exprace"), None)
            .await
            .is_ok(),
        "再 redeem も通る（deny 台帳に載っていない）"
    );
}

/// 破損した role の grant を個別取消すると `Integrity` で落ちる（黙ってタプルを残さない）。
#[tokio::test]
async fn share_link_grant_revoke_rejects_corrupt_role() {
    let Some(ctx) = setup().await else { return };
    let Ctx {
        service,
        pool,
        authz,
        http,
        ..
    } = ctx;

    let org = format!("itorg{}", Uuid::new_v4().simple());
    let owner = format!("ituser{}", Uuid::new_v4().simple());
    let octx = make_ctx(&org, &owner);
    seed_org_member(&authz, &org, &owner).await;
    let bob = format!("ituser{}", Uuid::new_v4().simple());
    let bctx = make_ctx(&org, &bob);
    seed_org_member(&authz, &org, &bob).await;

    let file = upload(&service, &http, &octx, None, "corrupt.txt", b"x")
        .await
        .expect("upload");
    let l = service
        .create_share_link(
            &octx,
            file.id,
            GeneralAccessLevel::Organization,
            ShareRole::Viewer,
            None,
            Some("pw-corrupt"),
            None,
            None,
        )
        .await
        .expect("create");
    service
        .redeem_share_link(&bctx, &l.token, Some("pw-corrupt"), None)
        .await
        .expect("redeem");
    sqlx::query("UPDATE node_share_link_grant SET role = 'bogus' WHERE link_id = $1")
        .bind(l.link_id)
        .execute(&pool)
        .await
        .expect("corrupt role");

    assert!(
        matches!(
            service
                .revoke_share_link_grant(&octx, l.link_id, &bob, None)
                .await,
            Err(StorageError::Integrity(_))
        ),
        "破損 role は Integrity で落ちる"
    );
}

/// B-1: 毒行（未知 kind＝FgaObject を再構成できない破損/将来種別）は sweep 対象から外れる。残すと
/// next_expiry が過去時刻を返し続け、タイマが全速ループに入るため（kind IN ('file','folder') 除外）。
#[tokio::test]
async fn share_link_sweep_ignores_poison_kind() {
    use chrono::Utc;
    let Some(ctx) = setup().await else { return };
    let Ctx { service, pool, .. } = ctx;

    // 未知 kind の期限切れリンクを直接投入（アプリ経路では作れないが破損/将来種別を模す）。
    let link_id = Uuid::new_v4();
    let node_id = Uuid::new_v4();
    let tenant = format!("ittenant{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO node_share_link \
           (link_id, node_id, tenant_id, org, kind, audience, role, token, expires_at, created_by, updated_by) \
         VALUES ($1, $2, $3, 'o', 'weird', 'organization', 'viewer', $4, $5, 'sys', 'sys')",
    )
    .bind(link_id)
    .bind(node_id)
    .bind(&tenant)
    .bind(format!("tok{}", Uuid::new_v4().simple()))
    .bind(Utc::now() - chrono::Duration::hours(1))
    .execute(&pool)
    .await
    .expect("insert poison");

    // sweep はエラーにならず（head-of-line blocking なし）、毒行は kind フィルタで触れられない。
    service
        .revoke_expired_share_links(Utc::now())
        .await
        .expect("sweep はエラーにならない");
    let alive: bool =
        sqlx::query_scalar("SELECT revoked_at IS NULL FROM node_share_link WHERE link_id = $1")
            .bind(link_id)
            .fetch_one(&pool)
            .await
            .expect("check");
    assert!(
        alive,
        "毒行は sweep 対象外＝失効されず、かつホットループの燃料にもならない（B-1）"
    );
}
