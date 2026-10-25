//! テーブル（`data_table`）の共有の結合テスト（#864）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で実行する。
//!
//! - 付与: viewer は読める・書けない／editor は書ける／ロール（部署）への共有はそのメンバーに効く
//! - 取消: REST は即時に読めない（404）・開いている購読は TTL を待たず `stream.reset{authz_changed}` で閉じる
//! - 権限: owner 以外の共有・解除・一覧は 403、見えないテーブル・削除済みは 404
//! - 共有先: 存在しない・他テナントのユーザー／ロール、形の不正な id は 400（タプルを作らない）
//! - 監査: 付与・解除の allow 行、拒否の deny 行
//! - 材料化: 共有の直後に（読取を待たず）材料化の前提が検査し直され、包含が崩れた列は降格する（PIT-71）

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines
)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, Relation};
use data::{
    DataError, DataStore, FieldDef, FieldType, LinkCardinality, LinkDef, Materialization,
    NewDataTable, RecordStream, RefResolver, RollupDef, RollupOp, StreamConfig, StreamEvent,
    StreamResetReason, TableSchema, TableShareRole,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use storage::ShareTarget;
use uuid::Uuid;

/// テナントごとのディレクトリ（`(tenant, id)` の集合）。実装の `DirectoryStore` と同じく、呼び出し元の
/// テナントに居る id だけを「存在する」とする。
struct Directory {
    users: HashSet<(String, String)>,
    roles: HashSet<(String, String)>,
}

#[async_trait]
impl RefResolver for Directory {
    async fn user_exists(&self, ctx: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(self
            .users
            .contains(&(ctx.tenant_id.clone(), id.to_string())))
    }
    async fn role_exists(&self, ctx: &AuthContext, id: &str) -> Result<bool, String> {
        Ok(self
            .roles
            .contains(&(ctx.tenant_id.clone(), id.to_string())))
    }
    async fn file_readable(&self, _: &AuthContext, _: Uuid) -> Result<bool, String> {
        Ok(true)
    }
}

async fn setup() -> Option<(PgPool, Arc<dyn AuthzClient>)> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(base_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("マイグレーション適用");
    let model: Value =
        serde_json::from_str(include_str!("../../authz/model/authorization-model.json"))
            .expect("model json");
    let config = OpenFgaConfig {
        base_url,
        store_name: format!("shiki-data-table-share-{}", Uuid::new_v4()),
    };
    let fga = OpenFgaClient::connect(reqwest::Client::new(), &config, &model)
        .await
        .expect("OpenFGA 接続");
    Some((pool, Arc::new(fga)))
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

/// alice（owner）・bob・carol・dave（sales のメンバー）・eve（何もない）。`mallory` と `ops` は**別テナント**
/// にだけ居る。
struct Fx {
    store: DataStore,
    authz: Arc<dyn AuthzClient>,
    pool: PgPool,
    alice: AuthContext,
    bob: AuthContext,
    carol: AuthContext,
    dave: AuthContext,
    eve: AuthContext,
    table: Uuid,
}

const USERS: [&str; 5] = ["alice", "bob", "carol", "dave", "eve"];

fn directory(tenant: &str) -> Directory {
    let other = format!("{tenant}-other");
    let mut users: HashSet<(String, String)> = USERS
        .iter()
        .map(|u| (tenant.to_string(), (*u).to_string()))
        .collect();
    users.insert((other.clone(), "mallory".into()));
    let roles = [
        (tenant.to_string(), "sales".to_string()),
        (other, "ops".to_string()),
    ]
    .into_iter()
    .collect();
    Directory { users, roles }
}

fn schema(fields: Vec<FieldDef>) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy: None,
        field_policy: vec![],
        aggregate_min_rows: Some(1),
        fsm_ref: None,
        rag: None,
    }
}

async fn fixture(pool: PgPool, authz: Arc<dyn AuthzClient>) -> Fx {
    let tenant = format!("t-{}", Uuid::new_v4());
    let store = DataStore::new(
        pool.clone(),
        Arc::clone(&authz),
        Arc::new(directory(&tenant)),
    )
    .with_stream_config(StreamConfig {
        manual: true,
        // TTL は十分長くする（解除が TTL ではなく世代で即時に効くことを確かめる）。
        authz_ttl: Duration::from_hours(1),
        consumer: format!("data-table-share-it-{}", Uuid::new_v4().simple()),
        ..StreamConfig::default()
    });
    store.register_stream_consumer().await.expect("consumer");
    let alice = ctx(&tenant, "alice");
    let dave = ctx(&tenant, "dave");
    authz
        .write_tuple(&dave.subject(), Relation::Member, &dave.ns().role("sales"))
        .await
        .unwrap();
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "customers".into(),
                schema: schema(vec![FieldDef::new("name", FieldType::Text)]),
            },
            None,
        )
        .await
        .expect("table");
    Fx {
        store,
        authz,
        pool,
        bob: ctx(&tenant, "bob"),
        carol: ctx(&tenant, "carol"),
        dave,
        eve: ctx(&tenant, "eve"),
        alice,
        table: table.id,
    }
}

fn user(id: &str) -> ShareTarget {
    ShareTarget::User { id: id.into() }
}

fn role(id: &str) -> ShareTarget {
    ShareTarget::Role { id: id.into() }
}

impl Fx {
    async fn share(&self, target: &ShareTarget, r: TableShareRole) {
        self.store
            .share_table(&self.alice, self.table, target, r, None)
            .await
            .expect("share");
    }

    async fn unshare(&self, target: &ShareTarget, r: TableShareRole) {
        self.store
            .unshare_table(&self.alice, self.table, target, r, None)
            .await
            .expect("unshare");
    }

    async fn record(&self, name: &str) -> Uuid {
        self.store
            .create_record(&self.alice, self.table, json!({ "name": name }), None)
            .await
            .unwrap()
            .id
    }

    async fn sees_table(&self, who: &AuthContext) -> bool {
        self.store
            .list_tables_for_viewer(who, 200)
            .await
            .unwrap()
            .iter()
            .any(|t| t.id == self.table)
    }

    async fn shares(&self) -> Vec<(ShareTarget, TableShareRole)> {
        let mut v = self
            .store
            .list_table_shares(&self.alice, self.table, None)
            .await
            .unwrap();
        v.sort_by_key(|(t, r)| format!("{t:?}{r:?}"));
        v
    }

    /// この表の監査行 `(action, decision, actor)`（記録順）。
    async fn audit(&self, prefix: &str) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT action, decision, actor FROM audit_log \
             WHERE tenant_id = $1 AND object_type = 'data_table' AND object_id = $2 \
               AND action LIKE $3 ORDER BY id",
        )
        .bind(&self.alice.tenant_id)
        .bind(self.table.to_string())
        .bind(format!("{prefix}%"))
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

/// viewer 共有で読める・書けない／editor 共有で書ける。一覧は直接の viewer / editor だけ（owner は出さない）。
/// 付与は冪等で、監査に allow 行が残る。
#[tokio::test]
async fn viewer_reads_and_editor_writes_after_share() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz).await;
    let r = fx.record("acme").await;
    // 前提: 共有前は見えない（存在も分からない 404）。
    assert!(matches!(
        fx.store.get_record(&fx.bob, fx.table, r, None).await,
        Err(DataError::NotFound)
    ));
    assert!(!fx.sees_table(&fx.bob).await);

    fx.share(&user("bob"), TableShareRole::Viewer).await;
    fx.share(&user("bob"), TableShareRole::Viewer).await; // 冪等
    fx.share(&user("carol"), TableShareRole::Editor).await;

    // bob（viewer）: 一覧に出る・読める・書けない（見えるので 403）。
    assert!(fx.sees_table(&fx.bob).await);
    let got = fx
        .store
        .get_record(&fx.bob, fx.table, r, None)
        .await
        .unwrap();
    assert_eq!(got.data["name"], json!("acme"));
    assert!(matches!(
        fx.store
            .create_record(&fx.bob, fx.table, json!({"name": "x"}), None)
            .await,
        Err(DataError::Forbidden)
    ));
    // carol（editor）: 作成・更新できる。
    let c = fx
        .store
        .create_record(&fx.carol, fx.table, json!({"name": "by carol"}), None)
        .await
        .unwrap();
    fx.store
        .update_record(&fx.carol, fx.table, r, json!({"name": "renamed"}), 1, None)
        .await
        .unwrap();
    assert_eq!(
        fx.store
            .get_record(&fx.bob, fx.table, c.id, None)
            .await
            .unwrap()
            .data["name"],
        json!("by carol")
    );
    // 共有した人でも共有の操作はできない（owner だけ）。
    assert!(matches!(
        fx.store
            .share_table(
                &fx.carol,
                fx.table,
                &user("eve"),
                TableShareRole::Viewer,
                None
            )
            .await,
        Err(DataError::Forbidden)
    ));

    assert_eq!(
        fx.shares().await,
        vec![
            (user("bob"), TableShareRole::Viewer),
            (user("carol"), TableShareRole::Editor),
        ],
        "owner タプルは一覧に出ない"
    );
    let audit = fx.audit("data.table.share").await;
    let allow: Vec<_> = audit
        .iter()
        .filter(|(a, d, who)| a == "data.table.share" && d == "allow" && who == "alice")
        .collect();
    assert_eq!(allow.len(), 3, "付与ごとに allow の監査行: {audit:?}");
    assert!(
        audit
            .iter()
            .any(|(a, d, who)| a == "data.table.share" && d == "deny" && who == "carol"),
        "owner 以外の共有は deny で残る: {audit:?}"
    );
}

/// 部署（ロール）への共有はそのメンバーに効き、解除で即時に読めなくなる。
#[tokio::test]
async fn role_share_reaches_members_and_unshare_is_immediate() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz).await;
    let r = fx.record("acme").await;
    fx.share(&role("sales"), TableShareRole::Viewer).await;
    assert!(fx.sees_table(&fx.dave).await, "sales のメンバーに見える");
    fx.store
        .get_record(&fx.dave, fx.table, r, None)
        .await
        .unwrap();
    assert!(
        !fx.sees_table(&fx.eve).await,
        "メンバーでない人には見えない"
    );
    assert_eq!(
        fx.shares().await,
        vec![(role("sales"), TableShareRole::Viewer)]
    );

    fx.unshare(&role("sales"), TableShareRole::Viewer).await;
    fx.unshare(&role("sales"), TableShareRole::Viewer).await; // 冪等
    assert!(matches!(
        fx.store.get_record(&fx.dave, fx.table, r, None).await,
        Err(DataError::NotFound)
    ));
    assert!(!fx.sees_table(&fx.dave).await);
    assert!(fx.shares().await.is_empty());
    let audit = fx.audit("data.table.unshare").await;
    assert_eq!(
        audit
            .iter()
            .filter(|(_, d, who)| d == "allow" && who == "alice")
            .count(),
        2,
        "{audit:?}"
    );
}

/// 受信済みのイベントを取り切る（短い待ちで来なければ終わり）。閉じたら `true`。
async fn drain(s: &mut RecordStream) -> (Vec<StreamEvent>, bool) {
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(80), s.next()).await {
            Ok(Some(m)) => out.push(m.event),
            Ok(None) => return (out, true),
            Err(_) => return (out, false),
        }
    }
}

/// このテナントの outbox を、この fixture の台帳 consumer で全て採番し終えるまでリレーを回して配信する。
async fn tick(fx: &Fx) {
    let ledger = fx.store.stream_config().consumer.clone();
    for _ in 0..2_000 {
        fx.store.relay_record_changes_once().await.expect("relay");
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM storage_event_outbox o WHERE o.tenant_id = $1 \
               AND NOT EXISTS (SELECT 1 FROM outbox_delivery d \
                               WHERE d.event_id = o.id AND d.consumer = $2)",
        )
        .bind(&fx.alice.tenant_id)
        .bind(&ledger)
        .fetch_one(&fx.pool)
        .await
        .expect("pending");
        if pending == 0 {
            fx.store.stream_tick().await.expect("tick");
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("outbox の採番が収束しない");
}

/// 解除した相手の開いている購読は、TTL（1 時間）を待たず次のバッチで `stream.reset{authz_changed}` で閉じる。
/// 共有が残っている相手の購読は閉じない。
#[tokio::test]
async fn unshare_resets_the_open_stream_of_the_revoked_user() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz).await;
    fx.record("acme").await;
    fx.share(&user("bob"), TableShareRole::Viewer).await;
    fx.share(&user("carol"), TableShareRole::Viewer).await;
    let mut bob = fx
        .store
        .subscribe_records(&fx.bob, fx.table, None, None)
        .await
        .unwrap();
    let mut carol = fx
        .store
        .subscribe_records(&fx.carol, fx.table, None, None)
        .await
        .unwrap();
    tick(&fx).await; // 世代の基準を取る
    drain(&mut bob).await;
    drain(&mut carol).await;

    fx.unshare(&user("bob"), TableShareRole::Viewer).await;
    tick(&fx).await;
    let (events, closed) = drain(&mut bob).await;
    assert!(closed, "解除で購読ごと落ちる: {events:?}");
    assert!(
        matches!(
            events.last(),
            Some(StreamEvent::Reset(data::StreamResetEvent {
                reason: StreamResetReason::AuthzChanged
            }))
        ),
        "{events:?}"
    );
    // carol（共有が残る）の購読は続き、その後の変更も届く。
    let (_, closed) = drain(&mut carol).await;
    assert!(!closed, "共有が残る相手の購読は閉じない");
    fx.record("after").await;
    tick(&fx).await;
    let (events, closed) = drain(&mut carol).await;
    assert!(!closed);
    assert!(
        events.iter().any(|e| matches!(e, StreamEvent::Upserted(_))),
        "{events:?}"
    );
}

/// 見える範囲が変わらない viewer ⇄ editor の変更でも、開いている購読に `authz_changed` が届く
/// （画面が編集・追加・取込の操作を出し直す）。
#[tokio::test]
async fn role_change_resets_the_open_stream_even_if_rows_are_unchanged() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz).await;
    fx.record("acme").await;
    fx.share(&user("bob"), TableShareRole::Viewer).await;
    for (grant, revoke) in [
        (Some(TableShareRole::Editor), None),
        (None, Some(TableShareRole::Editor)),
    ] {
        let mut bob = fx
            .store
            .subscribe_records(&fx.bob, fx.table, None, None)
            .await
            .unwrap();
        tick(&fx).await; // 世代の基準を取る
        let (_, closed) = drain(&mut bob).await;
        assert!(!closed);
        if let Some(r) = grant {
            fx.share(&user("bob"), r).await;
        }
        if let Some(r) = revoke {
            fx.unshare(&user("bob"), r).await;
        }
        tick(&fx).await;
        let (events, closed) = drain(&mut bob).await;
        assert!(closed, "役割の変更で購読ごと落ちる: {events:?}");
        assert!(
            matches!(
                events.last(),
                Some(StreamEvent::Reset(data::StreamResetEvent {
                    reason: StreamResetReason::AuthzChanged
                }))
            ),
            "{events:?}"
        );
    }
}

/// owner 以外は 403（見えるが owner でない）、見えないテーブル・削除済みは 404。ミニアプリが owner の
/// テーブル（利用者は editor）も利用者には共有できない。
#[tokio::test]
async fn only_owner_can_share_and_invisible_is_not_found() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz).await;
    fx.share(&user("bob"), TableShareRole::Editor).await;
    let t = user("eve");
    let v = TableShareRole::Viewer;
    // bob: 見える（editor）が owner でない → 403（共有・解除・一覧とも）。
    assert!(matches!(
        fx.store.share_table(&fx.bob, fx.table, &t, v, None).await,
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        fx.store.unshare_table(&fx.bob, fx.table, &t, v, None).await,
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        fx.store.list_table_shares(&fx.bob, fx.table, None).await,
        Err(DataError::Forbidden)
    ));
    // eve: 見えない → 404（存在しないテーブルと同じ）。
    for id in [fx.table, Uuid::new_v4()] {
        assert!(matches!(
            fx.store.share_table(&fx.eve, id, &t, v, None).await,
            Err(DataError::NotFound)
        ));
        assert!(matches!(
            fx.store.unshare_table(&fx.eve, id, &t, v, None).await,
            Err(DataError::NotFound)
        ));
        assert!(matches!(
            fx.store.list_table_shares(&fx.eve, id, None).await,
            Err(DataError::NotFound)
        ));
    }
    assert!(
        !fx.sees_table(&fx.eve).await,
        "拒否された共有は付いていない"
    );
    let denies = fx.audit("data.table.").await;
    for (action, who) in [
        ("data.table.share", "bob"),
        ("data.table.unshare", "bob"),
        ("data.table.shares.list", "bob"),
        ("data.table.share", "eve"),
    ] {
        assert!(
            denies
                .iter()
                .any(|(a, d, w)| a == action && d == "deny" && w == who),
            "{action} by {who} の deny 行: {denies:?}"
        );
    }

    // ミニアプリが owner のテーブル: 利用者（editor）は共有できない。
    let app = Uuid::new_v4();
    let app_table = fx
        .store
        .create_table_for_app(
            &fx.alice,
            NewDataTable {
                name: "app-orders".into(),
                schema: schema(vec![FieldDef::new("name", FieldType::Text)]),
            },
            app,
            None,
        )
        .await
        .unwrap();
    let obj = fx.alice.ns().data_table(&app_table.id.to_string());
    // インストールした人（owner を持つ）でも、ミニアプリ所有のテーブルは /data から共有・解除・一覧できない
    // （インストールが付けたロールのタプルを外させない）。拒否は監査に deny で残る。
    assert!(matches!(
        fx.store
            .share_table(&fx.alice, app_table.id, &t, v, None)
            .await,
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        fx.store
            .unshare_table(&fx.alice, app_table.id, &t, v, None)
            .await,
        Err(DataError::Forbidden)
    ));
    assert!(matches!(
        fx.store
            .list_table_shares(&fx.alice, app_table.id, None)
            .await,
        Err(DataError::Forbidden)
    ));
    let app_denies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND object_id = $2 AND decision = 'deny' \
         AND metadata ->> 'reason' = 'app_owned'",
    )
    .bind(&fx.alice.tenant_id)
    .bind(app_table.id.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(app_denies, 3, "アプリ所有の拒否は監査に deny で残る");
    // インストールと同じ形にする: owner はミニアプリ、利用者は editor。
    fx.authz
        .write_tuple(
            &fx.alice.ns().miniapp_principal(&app.to_string()),
            Relation::Owner,
            &obj,
        )
        .await
        .unwrap();
    fx.authz
        .delete_tuple(&fx.alice.subject(), Relation::Owner, &obj)
        .await
        .unwrap();
    fx.authz
        .write_tuple(&fx.alice.subject(), Relation::Editor, &obj)
        .await
        .unwrap();
    assert!(matches!(
        fx.store
            .share_table(&fx.alice, app_table.id, &t, v, None)
            .await,
        Err(DataError::Forbidden)
    ));

    // 削除済みのテーブルは owner にも、タプルが残っている共有相手（bob）にも 404（存在しないのと区別しない）。
    fx.store
        .delete_table(&fx.alice, fx.table, None)
        .await
        .unwrap();
    assert!(matches!(
        fx.store.share_table(&fx.alice, fx.table, &t, v, None).await,
        Err(DataError::NotFound)
    ));
    assert!(matches!(
        fx.store.list_table_shares(&fx.alice, fx.table, None).await,
        Err(DataError::NotFound)
    ));
    assert!(matches!(
        fx.store.list_table_shares(&fx.bob, fx.table, None).await,
        Err(DataError::NotFound)
    ));
}

/// 共有先の検査: 存在しない・他テナントのユーザー／ロール、形の不正な id は 400 でタプルを作らない。
/// 解除は形だけ検査する（存在しない相手の解除は冪等に成功する）。
#[tokio::test]
async fn invalid_share_targets_are_rejected() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz).await;
    let v = TableShareRole::Viewer;
    for target in [
        user("ghost"),
        user("mallory"), // 他テナントのユーザー
        role("nope"),
        role("ops"), // 他テナントのロール
        user(" bob"),
        user("bob|x"),
        user("a#member"),
        user(""),
    ] {
        let r = fx
            .store
            .share_table(&fx.alice, fx.table, &target, v, None)
            .await;
        assert!(matches!(&r, Err(e) if e.is_invalid()), "{target:?}: {r:?}");
    }
    assert!(fx.shares().await.is_empty(), "拒否した共有先のタプルは無い");
    assert!(fx.audit("data.table.share").await.is_empty());
    // 解除: 形の不正は 400、存在しない相手は冪等に成功。
    let r = fx
        .store
        .unshare_table(&fx.alice, fx.table, &user("bob|x"), v, None)
        .await;
    assert!(matches!(&r, Err(e) if e.is_invalid()), "{r:?}");
    fx.unshare(&user("ghost"), v).await;
}

fn link_to(to: Uuid) -> FieldDef {
    let mut f = FieldDef::new("items", FieldType::RecordRef);
    f.ref_table = Some(to);
    f.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToMany,
        symmetric_field: None,
        symmetric_key: None,
        symmetric_display_name: None,
        reverse: false,
    });
    f
}

/// 材料化列はミニアプリ所有のテーブルの組にしか付かず、ミニアプリ所有のテーブルは /data から共有できない:
/// インストールした人（owner）が参照元 orders を bob に共有しようとしても 403 で、材料化の前提（viewer 集合の
/// 包含）は崩れず、材料化データ（索引行・依存登録）もそのまま残る（PIT-71）。
#[tokio::test]
async fn app_tables_with_materialized_columns_cannot_be_shared() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz).await;
    let app = Uuid::new_v4();
    let lines = fx
        .store
        .create_table_for_app(
            &fx.alice,
            NewDataTable {
                name: "lines".into(),
                schema: schema(vec![FieldDef::new("amount", FieldType::Number)]),
            },
            app,
            None,
        )
        .await
        .unwrap();
    let mut total = FieldDef::new("total", FieldType::Rollup);
    total.rollup = Some(RollupDef {
        link: "items".into(),
        target: Some("amount".into()),
        op: RollupOp::Sum,
    });
    total.indexed = true;
    let orders = fx
        .store
        .create_table_for_app(
            &fx.alice,
            NewDataTable {
                name: "orders".into(),
                schema: schema(vec![link_to(lines.id), total]),
            },
            app,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        orders.schema.field("total").unwrap().materialization,
        Some(Materialization::Invariant),
        "前提: 材料化している"
    );
    let line = fx
        .store
        .create_record(&fx.alice, lines.id, json!({"amount": 700}), None)
        .await
        .unwrap();
    let order = fx
        .store
        .create_record(
            &fx.alice,
            orders.id,
            json!({"items": [line.id.to_string()]}),
            None,
        )
        .await
        .unwrap();
    let (ti, tb): (i32, i64) = sqlx::query_as(
        "SELECT tenant_int, table_int FROM data_table WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&fx.alice.tenant_id)
    .bind(orders.id)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    let materialized = || async {
        let n: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM data_index_num WHERE tenant_int = $1 AND table_int = $2 \
                       AND field_id = $3) \
                  + (SELECT count(*) FROM data_derived_dep WHERE tenant_int = $1 AND table_int = $2 \
                       AND field_id = $3)",
        )
        .bind(ti)
        .bind(tb)
        .bind(i16::try_from(orders.schema.field("total").unwrap().id).unwrap())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        n
    };
    assert!(materialized().await > 0, "前提: 材料化した索引行がある");

    let before = materialized().await;
    assert!(matches!(
        fx.store
            .share_table(
                &fx.alice,
                orders.id,
                &user("bob"),
                TableShareRole::Viewer,
                None,
            )
            .await,
        Err(DataError::Forbidden)
    ));
    assert_eq!(
        materialized().await,
        before,
        "共有を拒んだので材料化はそのまま"
    );
    let t = fx
        .store
        .get_table(&fx.alice, orders.id, None)
        .await
        .unwrap();
    assert_eq!(
        t.schema.field("total").unwrap().materialization,
        Some(Materialization::Invariant)
    );
    assert!(matches!(
        fx.store.list_table_shares(&fx.alice, orders.id, None).await,
        Err(DataError::Forbidden)
    ));
    let _ = order;
}

/// 共有の操作は、認可より前に旧形式のテーブルを移行しない（見えない主体が共有 API を叩くだけで、識別子の
/// 移行・索引台帳の初期化を起こせない）。生存は副作用のない読み取りで確かめ、移行を伴う読込は認可の後。
#[tokio::test]
async fn share_api_does_not_migrate_tables_before_authorization() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz).await;
    let tenant = fx.alice.tenant_id.clone();
    let mut conn = fx.pool.acquire().await.unwrap();
    let placement = storage::tenant::ensure_placement(&mut conn, &tenant)
        .await
        .unwrap();
    let table_int = storage::tenant::allocate_table_int(&mut conn)
        .await
        .unwrap();
    // v1 形式（識別子未移行・索引台帳なし）のテーブル。bob には見えない。
    let legacy: Uuid = sqlx::query_scalar(
        "INSERT INTO data_table (tenant_id, org, name, schema, created_by, tenant_int, table_int, \
           key_format, next_field_id) \
         VALUES ($1, 'acme', 'legacy', $2, 'alice', $3, $4, 1, 16) RETURNING id",
    )
    .bind(&tenant)
    .bind(json!({"fields": [{"name": "title", "type": "text", "indexed": true}]}))
    .bind(placement.tenant_int)
    .bind(table_int)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    drop(conn);
    for r in [
        fx.store
            .share_table(
                &fx.bob,
                legacy,
                &user("carol"),
                TableShareRole::Viewer,
                None,
            )
            .await,
        fx.store
            .unshare_table(
                &fx.bob,
                legacy,
                &user("carol"),
                TableShareRole::Viewer,
                None,
            )
            .await,
        fx.store
            .list_table_shares(&fx.bob, legacy, None)
            .await
            .map(|_| ()),
    ] {
        assert!(matches!(r, Err(DataError::NotFound)), "{r:?}");
    }
    let (key_format, init): (i16, bool) = sqlx::query_as(
        "SELECT key_format, pivot_index_init FROM data_table WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&tenant)
    .bind(legacy)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        (key_format, init),
        (1, false),
        "見えない主体の呼び出しで移行が走った"
    );
}
