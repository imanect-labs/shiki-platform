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
    AuthContext, AuthzClient, Consistency, Principal, Relation,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use storage::{
    content_address::sha256_hex, object_store::S3Config, DirectoryStore, Node, NodeKind,
    ObjectStore, S3ObjectStore, ShareRole, ShareTarget, StorageError, StorageService,
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
        "INSERT INTO node (org, tenant_id, kind, name, created_by) \
         VALUES ($1, 'default', 'folder', 'myfolder', $2) RETURNING id",
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

/// tenant_id ＋ org でスコープした AuthContext を作る（テナント分離検証用）。
fn make_ctx_tenant(org: &str, tenant: &str, uid: &str) -> AuthContext {
    AuthContext::new(
        Principal {
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
