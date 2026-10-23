//! レコード変更 outbox ＋ SSE 差分配信の結合テスト（Phase 15 Task 15.6・data-platform §9・PIT-70・PIT-73）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で実行する。
//! 配信は検証用に手動駆動（`StreamConfig::manual` ＋ `stream_tick`）にし、故障を注入する窓を
//! 決定的に作る。遅延の計測だけは本番と同じ常駐タスク（250 ms 窓）で回す。
//!
//! クライアントは `(record_id, rev)` で冪等に適用するモデル（[`Client`]）で表す
//! （data-platform §9.2 の契約。既知の `rev` 以下の upsert・手元に無い行の removed は捨てる）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{AuthContext, AuthzClient, Principal, Relation};
use data::{
    BulkImportOptions, BulkImportRow, CmpOp, DataError, DataStore, FieldDef, FieldPolicy,
    FieldType, LinkCardinality, LinkDef, LookupDef, NewDataTable, PolicyExpr, PolicyOperand,
    RecordShareRole, RecordStream, RefResolver, RowPolicy, StreamConfig, StreamEvent,
    StreamMessage, StreamResetReason, TableSchema,
};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

struct AllowRefs;

#[async_trait]
impl RefResolver for AllowRefs {
    async fn user_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
    }
    async fn role_exists(&self, _: &AuthContext, _: &str) -> Result<bool, String> {
        Ok(true)
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
        store_name: format!("shiki-data-stream-test-{}", Uuid::new_v4()),
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

fn manual() -> StreamConfig {
    StreamConfig {
        manual: true,
        ..StreamConfig::default()
    }
}

/// `applicant = $user ∨ has_role(keiri)` の行述語と、keiri だけが読める `salary`。
fn expense_schema() -> TableSchema {
    let mut applicant = FieldDef::new("applicant", FieldType::UserRef);
    applicant.indexed = true;
    let mut status = FieldDef::new("status", FieldType::Select);
    status.options = vec!["draft".into(), "done".into()];
    status.indexed = true;
    TableSchema {
        fields: vec![
            applicant,
            FieldDef::new("title", FieldType::Text),
            FieldDef::new("salary", FieldType::Number),
            status,
        ],
        status_field: None,
        row_policy: Some(RowPolicy {
            read: PolicyExpr::Any(vec![
                PolicyExpr::FieldCmp {
                    field: "applicant".into(),
                    op: CmpOp::Eq,
                    value: PolicyOperand::UserId,
                },
                PolicyExpr::HasRole {
                    role: "keiri".into(),
                    subtree: true,
                },
            ]),
            write: None,
        }),
        field_policy: vec![FieldPolicy {
            field: "salary".into(),
            readable_by: PolicyExpr::HasRole {
                role: "keiri".into(),
                subtree: true,
            },
        }],
        aggregate_min_rows: None,
        fsm_ref: None,
    }
}

/// alice（owner・keiri）・bob（editor・一般）・carol（viewer・keiri）。
struct Fx {
    store: DataStore,
    authz: Arc<dyn AuthzClient>,
    pool: PgPool,
    alice: AuthContext,
    bob: AuthContext,
    carol: AuthContext,
    table: Uuid,
}

async fn fixture(pool: PgPool, authz: Arc<dyn AuthzClient>, config: StreamConfig) -> Fx {
    let tenant = format!("t-{}", Uuid::new_v4());
    let alice = ctx(&tenant, "alice");
    let bob = ctx(&tenant, "bob");
    let carol = ctx(&tenant, "carol");
    let store = DataStore::new(pool.clone(), Arc::clone(&authz), Arc::new(AllowRefs))
        // テストごとに独立した台帳 consumer（＝ストリームの名前空間）を使う。同じバイナリの他テストの
        // リレー（手動・常駐の両方）がこのテストの outbox を先に採番して、故障注入の窓や位置の検証を
        // 崩さないようにする（本番は全レプリカで同じ名前・同じ consumer のリレーは採番行のロックで直列化）。
        .with_stream_config(StreamConfig {
            consumer: format!("data-stream-it-{}", Uuid::new_v4().simple()),
            ..config
        });
    // 新しい台帳 consumer の登録は既存の outbox を配送済みにする（他テストの滞留を背負わない）。
    store.register_stream_consumer().await.expect("consumer");
    let table = store
        .create_table(
            &alice,
            NewDataTable {
                name: "expense".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .expect("table");
    let obj = alice.ns().data_table(&table.id.to_string());
    authz
        .write_tuple(&bob.subject(), Relation::Editor, &obj)
        .await
        .unwrap();
    authz
        .write_tuple(&carol.subject(), Relation::Viewer, &obj)
        .await
        .unwrap();
    for u in [&alice, &carol] {
        authz
            .write_tuple(&u.subject(), Relation::Member, &alice.ns().role("keiri"))
            .await
            .unwrap();
    }
    Fx {
        store,
        authz,
        pool,
        alice,
        bob,
        carol,
        table: table.id,
    }
}

/// `(record_id, rev)` で冪等に適用するクライアント（data-platform §9.2）。
#[derive(Default)]
struct Client {
    rows: HashMap<Uuid, (i64, Value)>,
    /// 最後に受けた位置（サーバは昇順で送るので連続処理済みの位置＝`Last-Event-ID`）。
    last_id: Option<i64>,
    applied: usize,
    dropped: usize,
    raw: Vec<String>,
}

impl Client {
    fn apply(&mut self, m: &StreamMessage) {
        self.raw
            .push(format!("{} {}", m.event.name(), m.event.data_json()));
        match &m.event {
            StreamEvent::Upserted(u) => {
                let known = self.rows.get(&u.record.id).map(|r| r.0);
                let take = match known {
                    None => true,
                    Some(k) => u.record.rev > k || (u.derived && u.record.rev >= k),
                };
                if take {
                    self.rows
                        .insert(u.record.id, (u.record.rev, u.record.data.clone()));
                    self.applied += 1;
                } else {
                    self.dropped += 1;
                }
            }
            StreamEvent::Removed(r) => {
                if self.rows.remove(&r.record_id).is_some() {
                    self.applied += 1;
                } else {
                    self.dropped += 1;
                }
            }
            _ => {}
        }
        if let Some(id) = m.id {
            if !matches!(m.event, StreamEvent::Reset(_)) {
                self.last_id = Some(id);
            }
        }
    }
}

/// 受信済みのイベントを取り切る（短い待ちで来なければ終わり）。閉じたら `true`。
async fn drain(s: &mut RecordStream, c: &mut Client) -> (Vec<StreamMessage>, bool) {
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(80), s.next()).await {
            Ok(Some(m)) => {
                c.apply(&m);
                out.push(m);
            }
            Ok(None) => return (out, true),
            Err(_) => return (out, false),
        }
    }
}

/// このテストのテナントの outbox を、このテストの台帳 consumer で全て採番し終えるまでリレーを回す。
///
/// 「1 回のリレーが 0 件を返した」は「残りが無い」を意味しない: `claim_undelivered` は
/// `FOR UPDATE SKIP LOCKED` なので、同じバイナリの**他テストのリレー（別の台帳 consumer）**が
/// 掴んでいる行はその Tx のコミットまで飛ばされる（その Tx が積む派生値の再配信も、コミットまで
/// 見えない）。テナントの未採番が 0 になるまで回すことで、他のリレーの Tx の完了も待つ。
async fn relay_all(fx: &Fx) {
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
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("outbox の採番が収束しない");
}

/// このテナントの outbox の保持期間切れ（時間の経過）を模擬し、**本番の GC 入口**で消す。消えた
/// このテナントの行数を返す（模擬の時点の行が全て消えるまで回す）。
///
/// 共有 DB では他テストの台帳 consumer（テストごとに登録・全テナントを採番する）や他の機能の
/// consumer が登録されている。明示の consumer 集合で GC すると、それらが未配送の行や並行の登録と
/// 競合する（autocommit の接続で呼ぶと GC の相互排除のロックが外れ、他テストの登録の fast-forward
/// を FK 違反で落とした）。ここでは RAG の ack と**登録済みの全台帳 consumer の配送**を済ませ、保持期間
/// だけが GC を止める状態にしてから、登録台帳を読む本番の入口で GC する。
async fn expire_retention_and_gc(fx: &Fx) -> usize {
    let tenant = &fx.alice.tenant_id;
    // 先に、登録済みの全台帳 consumer が配送済み（追いついた）状態にする。並行 GC と FK で競合しないよう
    // 行を `FOR KEY SHARE` で押さえる（claim 中の他テストのリレーはそのコミットを待つ）。
    sqlx::query(
        "INSERT INTO outbox_delivery (consumer, event_id, tenant_id) \
         SELECT c.name, o.id, o.tenant_id FROM outbox_consumer c CROSS JOIN storage_event_outbox o \
         WHERE o.tenant_id = $1 FOR KEY SHARE OF o ON CONFLICT (consumer, event_id) DO NOTHING",
    )
    .bind(tenant)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE storage_event_outbox SET processed_at = now() WHERE tenant_id = $1")
        .bind(tenant)
        .execute(&fx.pool)
        .await
        .unwrap();
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM storage_event_outbox WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    // この配送の記録は台帳 consumer ごとのリレーの直列化（採番行のロック）の外から書くので、上の INSERT
    // のコミット前に claim の文を始めていた他テストのリレーは、同じ行を claim してそれぞれの保持期間で
    // 採番し得る（READ COMMITTED の anti-join はロック待ちの後も文のスナップショットで判定する）。
    // 本番では同じ consumer の配送はそのリレーしか書かないので起きない。そうした遅れて届く台帳行も
    // 時間の経過で保持期間を過ぎたものとして、毎回期限切れにしてから GC する。GC は claim 中
    // （他テストのリレーが行ロック中）の行をその回スキップする（SKIP LOCKED）ので、消えるまで回す。
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        sqlx::query(
            "UPDATE outbox_delivery SET retain_until = now() - interval '1 second' \
             WHERE tenant_id = $1 AND retain_until > now() - interval '1 second'",
        )
        .bind(tenant)
        .execute(&fx.pool)
        .await
        .unwrap();
        let mut tx = fx.pool.begin().await.unwrap();
        storage::event::gc_delivered_registered(&mut tx, 10_000)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let left: i64 =
            sqlx::query_scalar("SELECT count(*) FROM storage_event_outbox WHERE id = ANY($1)")
                .bind(&ids)
                .fetch_one(&fx.pool)
                .await
                .unwrap();
        if left == 0 {
            return ids.len();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("保持期間を過ぎた行が GC されない");
}

async fn tick(fx: &Fx) {
    relay_all(fx).await;
    fx.store.stream_tick().await.expect("tick");
}

fn upserts(ms: &[StreamMessage]) -> Vec<(Uuid, Value)> {
    ms.iter()
        .filter_map(|m| match &m.event {
            StreamEvent::Upserted(u) => Some((u.record.id, u.record.data.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn other_users_edit_reaches_the_grid_within_one_second() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    // 本番と同じ常駐タスク（250 ms 窓・既定設定）で計る。
    let fx = fixture(pool, authz, StreamConfig::default()).await;
    let mut s = fx
        .store
        .subscribe_records(&fx.bob, fx.table, None, None)
        .await
        .unwrap();
    let mut c = Client::default();
    let (ready, _) = drain(&mut s, &mut c).await;
    assert!(matches!(ready[0].event, StreamEvent::Ready(_)));
    let mut worst = Duration::ZERO;
    for i in 0..5 {
        let t0 = Instant::now();
        let rec = fx
            .store
            .create_record(
                &fx.alice,
                fx.table,
                json!({"applicant": "bob", "title": format!("t{i}"), "salary": 10}),
                None,
            )
            .await
            .unwrap();
        loop {
            let m = tokio::time::timeout(Duration::from_secs(2), s.next())
                .await
                .expect("1 秒以内に届く")
                .expect("open");
            c.apply(&m);
            if let StreamEvent::Upserted(u) = &m.event {
                if u.record.id == rec.id {
                    break;
                }
            }
        }
        worst = worst.max(t0.elapsed());
    }
    eprintln!("他ユーザーの編集が届くまでの最悪値: {worst:?}");
    assert!(worst < Duration::from_secs(1), "{worst:?}");
}

#[tokio::test]
async fn invisible_rows_and_masked_columns_never_reach_the_stream() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz, manual()).await;
    let mut sb = fx
        .store
        .subscribe_records(&fx.bob, fx.table, None, None)
        .await
        .unwrap();
    let mut sc = fx
        .store
        .subscribe_records(&fx.carol, fx.table, None, None)
        .await
        .unwrap();
    let (mut cb, mut cc) = (Client::default(), Client::default());
    drain(&mut sb, &mut cb).await;
    drain(&mut sc, &mut cc).await;
    let hidden = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "alice", "title": "秘密の件名", "salary": 987_654}),
            None,
        )
        .await
        .unwrap();
    let mine = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "bob", "title": "bob の件", "salary": 123_456}),
            None,
        )
        .await
        .unwrap();
    // 不可視の行の更新・削除も流れない。
    fx.store
        .update_record(
            &fx.alice,
            fx.table,
            hidden.id,
            json!({"title": "まだ秘密"}),
            1,
            None,
        )
        .await
        .unwrap();
    tick(&fx).await;
    let (mb, _) = drain(&mut sb, &mut cb).await;
    let (mc, _) = drain(&mut sc, &mut cc).await;
    // bob: 自分の行だけ・salary は無い。
    let ub = upserts(&mb);
    assert_eq!(ub.len(), 1, "{:?}", cb.raw);
    assert_eq!(ub[0].0, mine.id);
    assert!(ub[0].1.get("salary").is_none(), "マスク列は流れない");
    let all = cb.raw.join("\n");
    for leak in [
        hidden.id.to_string(),
        "秘密".into(),
        "987654".into(),
        "123456".into(),
    ] {
        assert!(
            !all.contains(&leak),
            "{leak} が bob のストリームに出た: {all}"
        );
    }
    // carol（keiri）: 両方・salary も見える。
    let uc = upserts(&mc);
    assert!(uc
        .iter()
        .any(|(id, d)| *id == hidden.id && d["salary"] == json!(987_654)));
    assert!(uc.iter().any(|(id, _)| *id == mine.id));
    assert_eq!(fx.store.stream_verify_drops(), 0);
}

#[tokio::test]
async fn visible_to_invisible_sends_removed_without_values() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz, manual()).await;
    let r1 = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "bob", "title": "見える", "salary": 1}),
            None,
        )
        .await
        .unwrap();
    let r2 = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "bob", "title": "消える"}),
            None,
        )
        .await
        .unwrap();
    let mut s = fx
        .store
        .subscribe_records(&fx.bob, fx.table, None, None)
        .await
        .unwrap();
    let mut c = Client::default();
    drain(&mut s, &mut c).await;
    // REST で初期表示（グリッドの行）。
    for r in [&r1, &r2] {
        let got = fx
            .store
            .get_record(&fx.bob, fx.table, r.id, None)
            .await
            .unwrap();
        c.rows.insert(got.id, (got.rev, got.data));
    }
    // 可視 → 不可視（申請者を付け替える）と削除。
    fx.store
        .update_record(
            &fx.alice,
            fx.table,
            r1.id,
            json!({"applicant": "alice"}),
            1,
            None,
        )
        .await
        .unwrap();
    fx.store
        .delete_record(&fx.alice, fx.table, r2.id, 1, None)
        .await
        .unwrap();
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    let removed: Vec<&StreamMessage> = ms
        .iter()
        .filter(|m| matches!(m.event, StreamEvent::Removed(_)))
        .collect();
    assert_eq!(removed.len(), 2, "{:?}", c.raw);
    for m in &removed {
        // record_id だけ（値を含まない・PIT-73）。
        let d = m.event.data_json();
        assert_eq!(d.as_object().unwrap().len(), 1, "{d}");
        assert!(d.get("record_id").is_some());
    }
    assert!(c.rows.is_empty(), "以後その行は表示されない");
    assert!(!c.raw.join("\n").contains("見える"));
    // 不可視 → 可視は新規出現として upsert。
    fx.store
        .update_record(
            &fx.alice,
            fx.table,
            r1.id,
            json!({"applicant": "bob"}),
            2,
            None,
        )
        .await
        .unwrap();
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    assert_eq!(upserts(&ms).len(), 1);
    assert_eq!(c.rows.get(&r1.id).map(|r| r.0), Some(3));
}

#[tokio::test]
async fn resume_from_last_event_id_without_gaps_under_concurrent_commits() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = Arc::new(fixture(pool, authz, manual()).await);
    // carol（keiri）は全行が見える＝全ての位置にイベントが出る（欠落の検出が厳密になる）。
    let mut s = fx
        .store
        .subscribe_records(&fx.carol, fx.table, None, None)
        .await
        .unwrap();
    let mut c = Client::default();
    let mut seen: Vec<i64> = Vec::new();
    let (ready, _) = drain(&mut s, &mut c).await;
    let start = ready[0].id.unwrap();
    // 8 本の並行書込（作成と更新の混在）。
    let mut tasks = Vec::new();
    for w in 0..8 {
        let fx = Arc::clone(&fx);
        tasks.push(tokio::spawn(async move {
            for i in 0..6 {
                let r = fx
                    .store
                    .create_record(
                        &fx.alice,
                        fx.table,
                        json!({"applicant": "alice", "title": format!("w{w}-{i}")}),
                        None,
                    )
                    .await
                    .unwrap();
                fx.store
                    .update_record(&fx.alice, fx.table, r.id, json!({"title": "upd"}), 1, None)
                    .await
                    .unwrap();
            }
        }));
    }
    // 書込の途中で受けて、途中で切断する。
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        tick(&fx).await;
        let (ms, _) = drain(&mut s, &mut c).await;
        seen.extend(ms.iter().filter_map(|m| m.id).filter(|i| *i > start));
    }
    let resume_at = c.last_id.unwrap();
    s.close();
    for t in tasks {
        t.await.unwrap();
    }
    // 再接続（Last-Event-ID）→ 最後まで。
    let mut s = fx
        .store
        .subscribe_records(&fx.carol, fx.table, Some(resume_at), None)
        .await
        .unwrap();
    for _ in 0..10 {
        tick(&fx).await;
        let (ms, _) = drain(&mut s, &mut c).await;
        seen.extend(
            ms.iter()
                .filter(|m| !matches!(m.event, StreamEvent::Ready(_)))
                .filter_map(|m| m.id),
        );
    }
    // 位置は 1 つずつ・穴なし（重複は再接続の境界で許す＝冪等に捨てられる）。
    let mut uniq = seen.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(uniq.len(), 96, "8 本 × 6 件 × (作成 + 更新)");
    assert!(
        uniq.windows(2).all(|w| w[1] == w[0] + 1),
        "欠落なし: {uniq:?}"
    );
    assert_eq!(uniq[0], start + 1);
    // 手元の状態は REST と一致（全行 rev = 2）。
    assert_eq!(c.rows.len(), 48);
    assert!(c
        .rows
        .values()
        .all(|(rev, d)| *rev == 2 && d["title"] == "upd"));
}

#[tokio::test]
async fn beyond_retention_gets_stream_reset() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(
        pool,
        authz,
        StreamConfig {
            retention: Duration::from_millis(200),
            ..manual()
        },
    )
    .await;
    for i in 0..3 {
        fx.store
            .create_record(
                &fx.alice,
                fx.table,
                json!({"applicant": "alice", "title": format!("{i}")}),
                None,
            )
            .await
            .unwrap();
    }
    relay_all(&fx).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let deleted = expire_retention_and_gc(&fx).await;
    assert!(deleted >= 3, "保持期間を過ぎた行は GC される: {deleted}");
    // 保持期間外の位置からの再開は stream.reset（黙って欠落させない）。
    let mut s = fx
        .store
        .subscribe_records(&fx.carol, fx.table, Some(1), None)
        .await
        .unwrap();
    let mut c = Client::default();
    let (ms, closed) = drain(&mut s, &mut c).await;
    assert!(
        matches!(
            ms[0].event,
            StreamEvent::Reset(data::StreamResetEvent {
                reason: StreamResetReason::Retention
            })
        ),
        "{:?}",
        c.raw
    );
    assert!(closed);
    // 保持期間内（最新）からは普通に繋がる。
    let head = ms[0].id.unwrap();
    let mut s = fx
        .store
        .subscribe_records(&fx.carol, fx.table, Some(head), None)
        .await
        .unwrap();
    let (ms, _) = drain(&mut s, &mut c).await;
    assert!(matches!(ms[0].event, StreamEvent::Ready(_)));
}

/// 配信のクラッシュ窓（data-platform §9.2 の表）それぞれで欠落が無く、重複は冪等に捨てられる。
#[tokio::test]
async fn crash_windows_lose_nothing_and_duplicates_are_idempotent() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz, manual()).await;
    let mut c = Client::default();
    let mut s = fx
        .store
        .subscribe_records(&fx.carol, fx.table, None, None)
        .await
        .unwrap();
    drain(&mut s, &mut c).await;

    // ① 永続化（採番）の前にリレーが落ちる: claim した Tx がロールバックする。
    let r1 = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "alice", "title": "one"}),
            None,
        )
        .await
        .unwrap();
    // claim は `SKIP LOCKED` なので、他テストの台帳 consumer のリレーが同じ outbox 行を claim 中だと
    // その回は掴めない。r1 を掴んだ Tx が落ちるまで繰り返す（掴めずに巻き戻した回は何も変えない）。
    let mut crashed_with_r1 = false;
    for _ in 0..200 {
        let mut tx = fx.pool.begin().await.unwrap();
        storage::event::lock_consumer_seq(&mut tx, &fx.store.stream_config().consumer, None)
            .await
            .unwrap();
        let claimed = storage::event::claim_undelivered(
            &mut tx,
            &fx.store.stream_config().consumer,
            1_000_000,
        )
        .await
        .unwrap();
        let got = claimed.iter().any(|e| e.node_id == r1.id);
        tx.rollback().await.unwrap(); // ← クラッシュ
        if got {
            crashed_with_r1 = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(crashed_with_r1, "r1 を claim した Tx が落ちる");
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    assert_eq!(upserts(&ms).len(), 1, "再 claim されて届く: {:?}", c.raw);
    assert!(c.rows.contains_key(&r1.id));

    // ② 永続化の後・SSE 送信の前に落ちる: 採番済みで、送る相手が居ない。
    let r2 = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "alice", "title": "two"}),
            None,
        )
        .await
        .unwrap();
    relay_all(&fx).await; // 永続化
    let resume = c.last_id.unwrap();
    s.close(); // ← 送信前に接続が落ちる
    tick(&fx).await; // 送信は失敗し、購読は片付く
    assert_eq!(fx.store.stream_subscriber_count(), 0);
    assert!(!c.rows.contains_key(&r2.id));
    let mut s = fx
        .store
        .subscribe_records(&fx.carol, fx.table, Some(resume), None)
        .await
        .unwrap();
    tick(&fx).await;
    drain(&mut s, &mut c).await;
    assert!(
        c.rows.contains_key(&r2.id),
        "Last-Event-ID 以降として再送される"
    );

    // ③ SSE 送信の後・クライアントの確認（位置の保存）の前に落ちる: 再接続で重複が来る。
    let before_save = c.last_id.unwrap();
    let r3 = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "alice", "title": "three"}),
            None,
        )
        .await
        .unwrap();
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    assert_eq!(upserts(&ms).len(), 1);
    let applied = c.applied;
    s.close(); // ← 受けたが Last-Event-ID を保存できずに落ちた（位置は before_save のまま）
    let mut s = fx
        .store
        .subscribe_records(&fx.carol, fx.table, Some(before_save), None)
        .await
        .unwrap();
    tick(&fx).await;
    let dropped = c.dropped;
    let (ms, _) = drain(&mut s, &mut c).await;
    assert_eq!(upserts(&ms).len(), 1, "再送される（at-least-once）");
    assert_eq!(c.applied, applied, "重複は (record_id, rev) で捨てられる");
    assert_eq!(c.dropped, dropped + 1);
    assert_eq!(c.rows.get(&r3.id).map(|r| r.0), Some(1));
}

/// 番号の飛び（コミット順と採番順がずれた順序非依存のリレー）を受けても low-water mark は進めず、
/// 間の位置が埋まってから昇順に配信する。飛びの直後に切断しても再開で間が届く。
#[tokio::test]
async fn low_water_mark_never_skips_an_uncommitted_gap() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz, manual()).await;
    let mut c = Client::default();
    let mut s = fx
        .store
        .subscribe_records(&fx.carol, fx.table, None, None)
        .await
        .unwrap();
    let (ready, _) = drain(&mut s, &mut c).await;
    let head = ready[0].id.unwrap();
    let ra = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "alice", "title": "a"}),
            None,
        )
        .await
        .unwrap();
    let rb = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "alice", "title": "b"}),
            None,
        )
        .await
        .unwrap();
    let ev = |rid: Uuid| {
        let pool = fx.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT id FROM storage_event_outbox WHERE node_id = $1 AND private_payload IS NOT NULL",
            )
            .bind(rid)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let (ea, eb) = (ev(ra.id).await, ev(rb.id).await);
    let ledger = fx.store.stream_config().consumer.clone();
    let stream = format!("{ledger}:{}", fx.table);
    // 順序非依存のリレーを模す: head+1 を採番した Tx A がコミットする前に、head+2 の Tx B がコミットする。
    let mut a = fx.pool.begin().await.unwrap();
    let insert =
        "INSERT INTO outbox_delivery (consumer, event_id, tenant_id, delivery_seq, retain_until) \
                  VALUES ($1, $2, $3, $4, now() + interval '1 hour')";
    sqlx::query(insert)
        .bind(&stream)
        .bind(ea)
        .bind(&fx.alice.tenant_id)
        .bind(head + 1)
        .execute(&mut *a)
        .await
        .unwrap();
    let mut b = fx.pool.begin().await.unwrap();
    sqlx::query(insert)
        .bind(&stream)
        .bind(eb)
        .bind(&fx.alice.tenant_id)
        .bind(head + 2)
        .execute(&mut *b)
        .await
        .unwrap();
    for (e, tx) in [(ea, &mut a), (eb, &mut b)] {
        storage::event::mark_delivered(tx, &ledger, &[e])
            .await
            .unwrap();
    }
    sqlx::query("UPDATE outbox_delivery_seq SET next_seq = $2 WHERE consumer = $1")
        .bind(&stream)
        .bind(head + 3)
        .execute(&mut *b)
        .await
        .unwrap();
    b.commit().await.unwrap();
    // head+2 だけが見える状態: 飛び番では進めない（何も送らない）。A が未コミットの間は
    // テナントの未採番が 0 にならないので（A の台帳行が見えない）、採番し切る `tick` ではなく
    // 配信の 1 バッチだけを回す（内部のリレーは A が参照ロックを持つ行を飛ばす）。
    fx.store.stream_tick().await.unwrap();
    let (ms, _) = drain(&mut s, &mut c).await;
    assert!(upserts(&ms).is_empty(), "飛び番の先を送らない: {:?}", c.raw);
    // 飛びを見た直後に切断し、受けた位置（head）から再開する。
    s.close();
    let mut s = fx
        .store
        .subscribe_records(&fx.carol, fx.table, Some(head), None)
        .await
        .unwrap();
    a.commit().await.unwrap();
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    let ids: Vec<i64> = ms
        .iter()
        .filter(|m| matches!(m.event, StreamEvent::Upserted(_)))
        .filter_map(|m| m.id)
        .collect();
    assert_eq!(ids, vec![head + 1, head + 2], "間の位置が昇順で届く");
    assert!(c.rows.contains_key(&ra.id) && c.rows.contains_key(&rb.id));
}

/// 権限の剥奪: SSE は TTL 以内に止まり、TTL を超えて続かない。REST は即時。共有解除は TTL を待たない。
#[tokio::test]
async fn revocation_stops_delivery_within_ttl_and_rest_is_immediate() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let ttl = Duration::from_secs(1);
    let fx = fixture(
        pool,
        authz,
        StreamConfig {
            authz_ttl: ttl,
            ..manual()
        },
    )
    .await;
    let r = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "bob", "title": "x"}),
            None,
        )
        .await
        .unwrap();
    let mut s = fx
        .store
        .subscribe_records(&fx.bob, fx.table, None, None)
        .await
        .unwrap();
    let mut c = Client::default();
    drain(&mut s, &mut c).await;
    // テーブル権限の剥奪（OpenFGA を直接・世代は進めない＝TTL だけが頼り）。
    fx.authz
        .delete_tuple(
            &fx.bob.subject(),
            Relation::Editor,
            &fx.alice.ns().data_table(&fx.table.to_string()),
        )
        .await
        .unwrap();
    let revoked_at = Instant::now();
    // REST は TTL の影響を受けない（即時に拒否）。
    assert!(matches!(
        fx.store.get_record(&fx.bob, fx.table, r.id, None).await,
        Err(DataError::Forbidden)
    ));
    // TTL が切れるまで配信を回し続け、止まった時刻を計る。購読の材料は剥奪より前（購読の開始時）に
    // 解決したので、TTL は剥奪の時刻より前に切れる。判定はバッチの開始時刻で行う（配信の判定は
    // バッチの先頭で材料を確かめてから行う）。
    let mut stopped = None;
    let mut seq = 1;
    while revoked_at.elapsed() < ttl * 3 {
        fx.store
            .update_record(
                &fx.alice,
                fx.table,
                r.id,
                json!({"title": format!("v{seq}")}),
                seq,
                None,
            )
            .await
            .unwrap();
        seq += 1;
        relay_all(&fx).await;
        let tick_started = revoked_at.elapsed();
        fx.store.stream_tick().await.unwrap();
        let (ms, closed) = drain(&mut s, &mut c).await;
        if tick_started >= ttl {
            assert!(
                upserts(&ms).is_empty(),
                "TTL を超えて配信が続かない（{tick_started:?}）"
            );
        }
        if closed {
            stopped = Some(tick_started);
            assert!(ms.iter().any(|m| matches!(
                m.event,
                StreamEvent::Reset(data::StreamResetEvent {
                    reason: StreamResetReason::AuthzChanged
                })
            )));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let stopped = stopped.expect("TTL で止まる");
    eprintln!("剥奪から配信停止（を判定したバッチの開始）まで {stopped:?}（TTL {ttl:?}）");
    // TTL 以内に止まる（バッチの窓 1 つ分の誤差を許す）。
    assert!(stopped <= ttl + Duration::from_millis(350), "{stopped:?}");

    // 共有解除・ロール変更は世代で即時（TTL を長くしても待たない）。
    let fx2 = fixture(
        fx.pool.clone(),
        Arc::clone(&fx.authz),
        StreamConfig {
            authz_ttl: Duration::from_hours(1),
            ..manual()
        },
    )
    .await;
    let shared = fx2
        .store
        .create_record(
            &fx2.alice,
            fx2.table,
            json!({"applicant": "alice", "title": "共有"}),
            None,
        )
        .await
        .unwrap();
    let target = storage::ShareTarget::User { id: "bob".into() };
    fx2.store
        .share_record(
            &fx2.alice,
            fx2.table,
            shared.id,
            &target,
            RecordShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let mut s = fx2
        .store
        .subscribe_records(&fx2.bob, fx2.table, None, None)
        .await
        .unwrap();
    let mut c = Client::default();
    drain(&mut s, &mut c).await;
    let t0 = Instant::now();
    fx2.store
        .unshare_record(
            &fx2.alice,
            fx2.table,
            shared.id,
            &target,
            RecordShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    tick(&fx2).await;
    let (ms, closed) = drain(&mut s, &mut c).await;
    assert!(closed, "共有解除で購読ごと落ちる: {:?}", c.raw);
    assert!(matches!(ms.last().unwrap().event, StreamEvent::Reset(_)));
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "TTL（1 時間）を待たない"
    );
    // ロール変更（テナント全体の世代）も同じ。
    let mut s = fx2
        .store
        .subscribe_records(&fx2.carol, fx2.table, None, None)
        .await
        .unwrap();
    let mut c = Client::default();
    drain(&mut s, &mut c).await;
    tick(&fx2).await; // 世代の基準を取る
    fx2.authz
        .delete_tuple(
            &fx2.carol.subject(),
            Relation::Member,
            &fx2.alice.ns().role("keiri"),
        )
        .await
        .unwrap();
    fx2.store
        .notify_authz_changed(&fx2.carol, None)
        .await
        .unwrap();
    tick(&fx2).await;
    let (_, closed) = drain(&mut s, &mut c).await;
    assert!(closed, "ロールの剥奪で購読ごと落ちる: {:?}", c.raw);
}

#[tokio::test]
async fn bulk_import_transition_derived_schema_and_awareness_reach_subscribers() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz, manual()).await;
    let mut s = fx
        .store
        .subscribe_records(&fx.bob, fx.table, None, None)
        .await
        .unwrap();
    let mut c = Client::default();
    drain(&mut s, &mut c).await;
    // 一括取込も同じ入口で outbox を出す（見える行だけが届く）。
    let rows = (0..20)
        .map(|i| BulkImportRow {
            key: Some(format!("k{i}")),
            data: json!({"applicant": if i % 2 == 0 { "bob" } else { "alice" }, "title": format!("bulk{i}")}),
        })
        .collect();
    let import = fx
        .store
        .start_bulk_import(
            &fx.alice,
            fx.table,
            rows,
            BulkImportOptions::default(),
            None,
        )
        .await
        .unwrap();
    fx.store
        .run_bulk_import(&fx.alice.tenant_id, fx.table, import)
        .await
        .unwrap();
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    assert_eq!(upserts(&ms).len(), 10, "bob の行だけ: {:?}", c.raw.len());

    // awareness: 見えない行を指すカーソルは届かず、見える行のものはデータなしで届く。
    let hidden = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "alice", "title": "秘"}),
            None,
        )
        .await
        .unwrap();
    let visible_id = *c.rows.keys().next().unwrap();
    tick(&fx).await;
    drain(&mut s, &mut c).await;
    fx.store
        .post_awareness(
            &fx.alice,
            fx.table,
            Some(hidden.id),
            Some("title".into()),
            None,
        )
        .await
        .unwrap();
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    assert!(
        !ms.iter()
            .any(|m| matches!(m.event, StreamEvent::Awareness(_))),
        "見えない行のカーソルは届かない"
    );
    fx.store
        .post_awareness(
            &fx.alice,
            fx.table,
            Some(visible_id),
            Some("title".into()),
            None,
        )
        .await
        .unwrap();
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    let aw: Vec<Value> = ms
        .iter()
        .filter(|m| matches!(m.event, StreamEvent::Awareness(_)))
        .map(|m| m.event.data_json())
        .collect();
    assert_eq!(aw.len(), 1);
    assert_eq!(aw[0]["record_id"], json!(visible_id));
    assert!(aw[0].get("data").is_none() && !aw[0].to_string().contains("bulk"));
    // マスク列のカーソルは送る側で拒否（bob は salary を読めない）。
    assert!(matches!(
        fx.store
            .post_awareness(
                &fx.bob,
                fx.table,
                Some(visible_id),
                Some("salary".into()),
                None
            )
            .await,
        Err(DataError::Forbidden)
    ));

    // スキーマの改訂は schema.changed を送って閉じる。
    let table = fx.store.get_table(&fx.alice, fx.table, None).await.unwrap();
    let mut next = table.schema.clone();
    next.fields.push(FieldDef::new("memo", FieldType::Text));
    fx.store
        .update_table_schema(&fx.alice, fx.table, next, None, None)
        .await
        .unwrap();
    tick(&fx).await;
    let (ms, closed) = drain(&mut s, &mut c).await;
    assert!(matches!(
        ms.last().unwrap().event,
        StreamEvent::SchemaChanged(_)
    ));
    assert!(closed);
}

#[tokio::test]
async fn referenced_record_change_refreshes_referrer_derived_values() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz, manual()).await;
    let customers = fx
        .store
        .create_table(
            &fx.alice,
            NewDataTable {
                name: "customers".into(),
                schema: TableSchema {
                    fields: vec![FieldDef::new("name", FieldType::Text)],
                    status_field: None,
                    row_policy: None,
                    field_policy: vec![],
                    aggregate_min_rows: None,
                    fsm_ref: None,
                },
            },
            None,
        )
        .await
        .unwrap();
    let mut cust = FieldDef::new("customer", FieldType::RecordRef);
    cust.ref_table = Some(customers.id);
    cust.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToOne,
        symmetric_field: None,
        symmetric_key: None,
        reverse: false,
    });
    let mut cname = FieldDef::new("customer_name", FieldType::Lookup);
    cname.lookup = Some(LookupDef {
        via_field: "customer".into(),
        target_field: "name".into(),
    });
    let orders = fx
        .store
        .create_table(
            &fx.alice,
            NewDataTable {
                name: "orders".into(),
                schema: TableSchema {
                    fields: vec![cust, cname, FieldDef::new("note", FieldType::Text)],
                    status_field: None,
                    row_policy: None,
                    field_policy: vec![],
                    aggregate_min_rows: None,
                    fsm_ref: None,
                },
            },
            None,
        )
        .await
        .unwrap();
    let acme = fx
        .store
        .create_record(&fx.alice, customers.id, json!({"name": "ACME"}), None)
        .await
        .unwrap();
    let order = fx
        .store
        .create_record(
            &fx.alice,
            orders.id,
            json!({"customer": [acme.id], "note": "n"}),
            None,
        )
        .await
        .unwrap();
    // 作成分の採番と、その派生イベントの採番を済ませてから繋ぐ（以降の変更だけを見る）。
    relay_all(&fx).await;
    relay_all(&fx).await;
    let mut s = fx
        .store
        .subscribe_records(&fx.alice, orders.id, None, None)
        .await
        .unwrap();
    let mut c = Client::default();
    drain(&mut s, &mut c).await;
    c.rows.insert(order.id, (order.rev, order.data.clone()));
    fx.store
        .update_record(
            &fx.alice,
            customers.id,
            acme.id,
            json!({"name": "ACME 株式会社"}),
            1,
            None,
        )
        .await
        .unwrap();
    // 1 回目でストリームへ採番し、参照元への派生イベントを積む。2 回目でそれが配られる。
    tick(&fx).await;
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    let got: Vec<&data::RecordUpsertedEvent> = ms
        .iter()
        .filter_map(|m| match &m.event {
            StreamEvent::Upserted(u) => Some(u),
            _ => None,
        })
        .collect();
    assert_eq!(got.len(), 1, "{:?}", c.raw);
    assert!(got[0].derived, "派生値だけの再配信");
    assert_eq!(got[0].record.id, order.id);
    assert_eq!(got[0].record.rev, order.rev, "rev は進まない");
    assert_eq!(got[0].record.data["customer_name"], json!("ACME 株式会社"));
    assert_eq!(c.rows[&order.id].1["customer_name"], json!("ACME 株式会社"));
}

/// スナップショット読取（data-platform §3.3）: 途中の更新・削除・作成があっても、基準位置の集合を
/// 作成順に重複・欠落なく、基準位置の値で返す。保持期間を過ぎたカーソルは 410。
#[tokio::test]
async fn snapshot_read_is_stable_across_concurrent_changes() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(
        pool,
        authz,
        StreamConfig {
            retention: Duration::from_mins(10),
            ..manual()
        },
    )
    .await;
    let mut ids = Vec::new();
    for i in 0..30 {
        // bob に見えるのは偶数番だけ（行述語は S 時点の値で評価される）。
        let applicant = if i % 2 == 0 { "bob" } else { "alice" };
        let r = fx
            .store
            .create_record(
                &fx.alice,
                fx.table,
                json!({"applicant": applicant, "title": format!("orig-{i:02}")}),
                None,
            )
            .await
            .unwrap();
        ids.push(r.id);
    }
    relay_all(&fx).await;
    let page = |cursor: Option<String>| data::DataQuery {
        limit: Some(4),
        cursor,
        snapshot: true,
        ..data::DataQuery::default()
    };
    let first = fx
        .store
        .run_query(&fx.bob, fx.table, &page(None), None)
        .await
        .unwrap();
    let s = first.snapshot_seq.expect("snapshot_seq");
    let mut seen: Vec<(Uuid, String, i64)> = first
        .items
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.data["title"].as_str().unwrap().to_string(), r.rev))
        .collect();
    // 返した行の更新（再び出てはならない）・先の行の更新・可視→不可視・削除・新規作成。
    fx.store
        .update_record(
            &fx.alice,
            fx.table,
            ids[0],
            json!({"title": "changed-0"}),
            1,
            None,
        )
        .await
        .unwrap();
    fx.store
        .update_record(
            &fx.alice,
            fx.table,
            ids[20],
            json!({"title": "changed-20"}),
            1,
            None,
        )
        .await
        .unwrap();
    fx.store
        .update_record(
            &fx.alice,
            fx.table,
            ids[22],
            json!({"applicant": "alice"}),
            1,
            None,
        )
        .await
        .unwrap();
    fx.store
        .delete_record(&fx.alice, fx.table, ids[24], 1, None)
        .await
        .unwrap();
    fx.store
        .update_record(
            &fx.alice,
            fx.table,
            ids[3],
            json!({"applicant": "bob"}),
            1,
            None,
        )
        .await
        .unwrap();
    relay_all(&fx).await; // 一部は採番済み
    fx.store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "bob", "title": "new"}),
            None,
        )
        .await
        .unwrap();
    fx.store
        .update_record(
            &fx.alice,
            fx.table,
            ids[26],
            json!({"title": "changed-26"}),
            1,
            None,
        )
        .await
        .unwrap(); // 未採番のまま
    let mut cursor = first.next_cursor;
    while let Some(c) = cursor {
        let r = fx
            .store
            .run_query(&fx.bob, fx.table, &page(Some(c)), None)
            .await
            .unwrap();
        assert_eq!(r.snapshot_seq, Some(s), "各ページに基準位置");
        seen.extend(
            r.items
                .unwrap()
                .into_iter()
                .map(|r| (r.id, r.data["title"].as_str().unwrap().to_string(), r.rev)),
        );
        cursor = r.next_cursor;
    }
    let want: Vec<(Uuid, String, i64)> = (0..30)
        .filter(|i| i % 2 == 0)
        .map(|i| (ids[i], format!("orig-{i:02}"), 1))
        .collect();
    // 集合と値: 基準位置の行がちょうど 1 回ずつ、基準位置の値で返る（重複・欠落なし）。
    let mut got_sorted = seen.clone();
    got_sorted.sort();
    let mut want_sorted = want.clone();
    want_sorted.sort();
    assert_eq!(
        got_sorted, want_sorted,
        "基準位置の集合・値（重複・欠落なし）"
    );
    assert_eq!(seen.len(), want.len());
    // 順序: 不変の内部位置（record_int）の昇順。record_int の採番はセッションごとにキャッシュされる
    // （cache 100）ので作成順とは限らないが、位置が動かないことが重複・欠落なしの根拠になる。
    // 削除済みの行は本体に位置が残らないので、残っている行だけで並びを確かめる。
    let ints: HashMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
        "SELECT id, record_int FROM data_record WHERE tenant_id = $1 AND table_id = $2",
    )
    .bind(&fx.alice.tenant_id)
    .bind(fx.table)
    .fetch_all(&fx.pool)
    .await
    .unwrap()
    .into_iter()
    .collect();
    let order: Vec<i64> = seen
        .iter()
        .filter_map(|(id, _, _)| ints.get(id).copied())
        .collect();
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "内部位置の昇順: {order:?}"
    );

    // ライブページングにも配信位置が付く。スナップショットのカーソルはライブへ持ち込めない。
    let live = fx
        .store
        .run_query(&fx.bob, fx.table, &data::DataQuery::default(), None)
        .await
        .unwrap();
    assert!(live.snapshot_seq.unwrap() >= s);
    let snap1 = fx
        .store
        .run_query(&fx.bob, fx.table, &page(None), None)
        .await
        .unwrap();
    let misuse = data::DataQuery {
        cursor: snap1.next_cursor.clone(),
        limit: Some(4),
        ..data::DataQuery::default()
    };
    assert!(matches!(
        fx.store.run_query(&fx.bob, fx.table, &misuse, None).await,
        Err(DataError::Invalid(_))
    ));
    // sort は受けない。
    let sorted = data::DataQuery {
        sort: vec![data::SortKey {
            field: "applicant".into(),
            descending: false,
        }],
        snapshot: true,
        ..data::DataQuery::default()
    };
    assert!(matches!(
        fx.store.run_query(&fx.bob, fx.table, &sorted, None).await,
        Err(DataError::Invalid(_))
    ));
}

#[tokio::test]
async fn snapshot_cursor_beyond_retention_is_gone() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(
        pool,
        authz,
        StreamConfig {
            retention: Duration::from_millis(200),
            ..manual()
        },
    )
    .await;
    for i in 0..5 {
        fx.store
            .create_record(
                &fx.alice,
                fx.table,
                json!({"applicant": "alice", "title": format!("{i}")}),
                None,
            )
            .await
            .unwrap();
    }
    relay_all(&fx).await;
    let q = data::DataQuery {
        limit: Some(2),
        snapshot: true,
        ..data::DataQuery::default()
    };
    let first = fx
        .store
        .run_query(&fx.carol, fx.table, &q, None)
        .await
        .unwrap();
    // 基準位置の後に変更が入り、その台帳行が保持期間を過ぎて GC される。
    fx.store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "alice", "title": "late"}),
            None,
        )
        .await
        .unwrap();
    relay_all(&fx).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    expire_retention_and_gc(&fx).await;
    let next = data::DataQuery {
        cursor: first.next_cursor,
        ..q
    };
    assert!(matches!(
        fx.store.run_query(&fx.carol, fx.table, &next, None).await,
        Err(DataError::Gone(_))
    ));
}

/// FSM 遷移も同じ outbox を通る（status の変化が購読者へ届く）。
#[tokio::test]
async fn fsm_transition_reaches_subscribers() {
    let Some((pool, authz)) = setup().await else {
        return;
    };
    let fx = fixture(pool, authz, manual()).await;
    let r = fx
        .store
        .create_record(
            &fx.alice,
            fx.table,
            json!({"applicant": "bob", "title": "fsm", "status": "draft"}),
            None,
        )
        .await
        .unwrap();
    relay_all(&fx).await;
    let mut s = fx
        .store
        .subscribe_records(&fx.bob, fx.table, None, None)
        .await
        .unwrap();
    let mut c = Client::default();
    drain(&mut s, &mut c).await;
    let fsm = data::FsmBody {
        states: vec!["draft".into(), "done".into()],
        transitions: vec![data::FsmTransition {
            from: "draft".into(),
            to: "done".into(),
            actor: PolicyExpr::Public,
        }],
    };
    fx.store
        .transition_record(&fx.alice, fx.table, r.id, "done", 1, &fsm, "status", None)
        .await
        .unwrap();
    tick(&fx).await;
    let (ms, _) = drain(&mut s, &mut c).await;
    let ups = upserts(&ms);
    assert_eq!(ups.len(), 1, "{:?}", c.raw);
    assert_eq!(ups[0].1["status"], json!("done"));
    assert_eq!(c.rows[&r.id].0, 2);
}
