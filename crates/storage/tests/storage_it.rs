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
    content_address::sha256_hex, object_store::S3Config, Node, NodeKind, ObjectStore,
    S3ObjectStore, ShareRole, ShareTarget, StorageError, StorageService,
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
        5 * 1024 * 1024 * 1024,
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

/// org メンバーとして seed する（ルート作成の認可に必要）。
async fn seed_org_member(authz: &Arc<dyn AuthzClient>, org: &str, uid: &str) {
    authz
        .write_tuple(
            &Subject::user(uid),
            Relation::Member,
            &FgaObject::organization(org),
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
        .list_children(&cctx, None, None, 50, None)
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
        .list_children(&cctx, None, None, 50, None)
        .await
        .expect("C list root after share");
    let ids: Vec<Uuid> = page_c2.items.iter().map(|n| n.id).collect();
    assert_eq!(ids, vec![folder_a.id], "共有された folderA のみ見える");

    // --- ページング（uid のルート子＝folderA/folderB を limit 1 で 2 ページ） ---
    let p1 = service
        .list_children(&actx, None, None, 1, None)
        .await
        .expect("page1");
    assert_eq!(p1.items.len(), 1, "1 ページ目は 1 件");
    assert!(p1.next_cursor.is_some(), "続きがある");
    let p2 = service
        .list_children(&actx, None, p1.next_cursor.as_deref(), 1, None)
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
        .list_shared_with_me(&bctx, None)
        .await
        .expect("shared with me");
    assert!(
        inbox.iter().any(|n| n.id == file.id),
        "共有された一覧に現れる"
    );
    // owner の「共有された一覧」には自作 file は出ない（作成者除外）。
    let owner_inbox = service
        .list_shared_with_me(&octx, None)
        .await
        .expect("owner inbox");
    assert!(
        !owner_inbox.iter().any(|n| n.id == file.id),
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
