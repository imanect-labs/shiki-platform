//! 構造化データの permission-aware 検索の PoC（Task 15.8・docs/data/rag-integration.md）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で、
//! 「増分索引（outbox 相乗り）→ pre-filter（`data_table` タグ）→ post-filter（可視集合を絞るクエリ 1 本）」を
//! 通しで動かす。**ベクタストア（Qdrant）は使わない**: 取得段は in-memory の索引（[`FakeRecordIndex`]）で
//! 部分一致の候補を返す偽物に置き換える。権限の判定（pre-filter のタグ・post-filter のクエリ・投影の検証・
//! 世代）はすべて本物を通る。
//!
//! 検証すること:
//! - 行レベル権限が検索結果に効く（A は自分の行だけ・B は別の集合・経理ロールは全行・剥奪で 0 件）
//! - pre-filter が汚染されても（tenant-only 縮退）post-filter が見えない行を落とす
//! - 索引と検索の間に OpenFGA へタプルを 1 本も書かない
//! - 投影にマスク対象列を含めるスキーマは 422（`DataError::Invalid`）
//! - `field_policy` を後から付けると、索引の再構築前は古いチャンクが世代不一致で落ち、再構築後に戻る

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use authz::client::{OpenFgaClient, OpenFgaConfig};
use authz::{
    AuthContext, AuthzClient, AuthzError, CheckTuple, Consistency, FgaObject, ObjectType,
    Principal, ReadTupleKey, Relation, Subject,
};
use data::{
    CmpOp, DataError, DataStore, FieldDef, FieldPolicy, FieldType, NewDataTable, PolicyExpr,
    PolicyOperand, RagIndexWorker, RagProjection, RecordChunk, RecordChunkSink, RecordShareRole,
    RefResolver, RowPolicy, TableSchema,
};
use rag::{
    post_filter_sources, readable_tables, record_results, RagError, RecordProjection,
    RecordSearchResult, RecordVisibility, SourceCandidate, SourceRef, VisibleRecords,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// 偽物と配線
// ---------------------------------------------------------------------------

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

/// 実 OpenFGA への委譲＋書込回数の計数（索引・検索がタプルを書かないことの証拠）。
struct CountingAuthz {
    inner: Arc<dyn AuthzClient>,
    writes: AtomicUsize,
}

#[async_trait]
impl AuthzClient for CountingAuthz {
    async fn check(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
        c: Consistency,
    ) -> Result<bool, AuthzError> {
        self.inner.check(s, r, o, c).await
    }
    async fn batch_check(
        &self,
        items: &[CheckTuple],
        c: Consistency,
    ) -> Result<Vec<bool>, AuthzError> {
        self.inner.batch_check(items, c).await
    }
    async fn write_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.write_tuple(s, r, o).await
    }
    async fn delete_tuple(
        &self,
        s: &Subject,
        r: Relation,
        o: &FgaObject,
    ) -> Result<bool, AuthzError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete_tuple(s, r, o).await
    }
    async fn read_tuples(
        &self,
        o: &FgaObject,
        r: Option<Relation>,
    ) -> Result<Vec<ReadTupleKey>, AuthzError> {
        self.inner.read_tuples(o, r).await
    }
    async fn list_objects(
        &self,
        s: &Subject,
        r: Relation,
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.inner.list_objects(s, r, t).await
    }
    async fn delete_object_tuples(&self, o: &FgaObject) -> Result<u32, AuthzError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete_object_tuples(o).await
    }
    async fn read_subject_objects(
        &self,
        s: &Subject,
        t: ObjectType,
    ) -> Result<Vec<String>, AuthzError> {
        self.inner.read_subject_objects(s, t).await
    }
}

/// in-memory のチャンク索引（ベクタストアの代役・sink と取得段を兼ねる）。
#[derive(Default)]
struct FakeRecordIndex {
    tables: Mutex<HashMap<(String, Uuid), TableIdx>>,
    resets: AtomicUsize,
}

#[derive(Default)]
struct TableIdx {
    generation: Option<String>,
    chunks: BTreeMap<Uuid, RecordChunk>,
}

#[async_trait]
impl RecordChunkSink for FakeRecordIndex {
    async fn generation(
        &self,
        tenant_id: &str,
        table_id: Uuid,
    ) -> Result<Option<String>, DataError> {
        Ok(self
            .tables
            .lock()
            .unwrap()
            .get(&(tenant_id.to_string(), table_id))
            .and_then(|t| t.generation.clone()))
    }
    async fn reset_table(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        generation: Option<&str>,
    ) -> Result<(), DataError> {
        self.resets.fetch_add(1, Ordering::SeqCst);
        let mut g = self.tables.lock().unwrap();
        let t = g.entry((tenant_id.to_string(), table_id)).or_default();
        t.chunks.clear();
        t.generation = generation.map(str::to_string);
        Ok(())
    }
    async fn upsert(&self, chunks: &[RecordChunk]) -> Result<(), DataError> {
        let mut g = self.tables.lock().unwrap();
        for c in chunks {
            let t = g.entry((c.tenant_id.clone(), c.table_id)).or_default();
            // 同じレコードの古い rev で上書きしない（再配信・順序の入れ替わり）。
            if t.chunks
                .get(&c.record_id)
                .is_some_and(|old| old.rev > c.rev)
            {
                continue;
            }
            t.chunks.insert(c.record_id, c.clone());
        }
        Ok(())
    }
    async fn remove(
        &self,
        tenant_id: &str,
        table_id: Uuid,
        record_ids: &[Uuid],
    ) -> Result<(), DataError> {
        if let Some(t) = self
            .tables
            .lock()
            .unwrap()
            .get_mut(&(tenant_id.to_string(), table_id))
        {
            for id in record_ids {
                t.chunks.remove(id);
            }
        }
        Ok(())
    }
}

/// pre-filter の形（`rag::PreFilter` と同じ 2 形）。
enum Pre {
    Tags(Vec<String>),
    TenantOnly,
}

impl FakeRecordIndex {
    /// 取得段の代役: テナントは無条件 AND、pre-filter はタグの積、本文は部分一致。
    fn retrieve(&self, ctx: &AuthContext, query: &str, pre: &Pre) -> Vec<SourceCandidate> {
        let g = self.tables.lock().unwrap();
        let mut out = Vec::new();
        for ((tenant, _), t) in g.iter() {
            if *tenant != ctx.tenant_id {
                continue;
            }
            for c in t.chunks.values() {
                if let Pre::Tags(tags) = pre {
                    if !tags.contains(&c.authz_tag) {
                        continue;
                    }
                }
                if c.title.contains(query) || c.body.contains(query) {
                    out.push(SourceCandidate {
                        chunk_id: Uuid::new_v4(),
                        source: SourceRef::Record {
                            table_id: c.table_id,
                            record_id: c.record_id,
                        },
                        score: 1.0,
                        generation: Some(c.generation.clone()),
                    });
                }
            }
        }
        out
    }

    fn chunk_bodies(&self, tenant: &str, table: Uuid) -> Vec<String> {
        self.tables
            .lock()
            .unwrap()
            .get(&(tenant.to_string(), table))
            .map(|t| t.chunks.values().map(|c| c.body.clone()).collect())
            .unwrap_or_default()
    }
}

/// `RecordVisibility` → `DataStore::rag_visible_records`（本番は両 crate に依存する配線層に置く薄いアダプタ）。
struct DataVisibility(DataStore);

#[async_trait]
impl RecordVisibility for DataVisibility {
    async fn visible_records(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        record_ids: &[Uuid],
    ) -> Result<VisibleRecords, RagError> {
        let v = self
            .0
            .rag_visible_records(ctx, table_id, record_ids, None)
            .await
            .map_err(|e| RagError::Source(e.to_string()))?;
        Ok(VisibleRecords {
            generation: v.generation,
            records: v
                .records
                .into_iter()
                .map(|h| {
                    (
                        h.record_id,
                        RecordProjection {
                            rev: h.rev,
                            title: h.title,
                            body: h.body,
                        },
                    )
                })
                .collect(),
        })
    }
}

struct Env {
    store: DataStore,
    raw: Arc<dyn AuthzClient>,
    counting: Arc<CountingAuthz>,
    index: Arc<FakeRecordIndex>,
    worker: RagIndexWorker,
    tenant: String,
}

fn serial_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn setup() -> Option<Env> {
    let Ok(db_url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return None;
    };
    let Ok(fga_url) = std::env::var("OPENFGA_TEST_URL") else {
        eprintln!("OPENFGA_TEST_URL 未設定のためスキップ");
        return None;
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Postgres へ接続できること");
    sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
    let fga = OpenFgaClient::connect(
        reqwest::Client::new(),
        &OpenFgaConfig {
            base_url: fga_url,
            store_name: format!("shiki-rag-record-poc-{}", Uuid::new_v4()),
        },
        &authz::model::default_model(),
    )
    .await
    .expect("OpenFGA へ接続できること");
    let raw: Arc<dyn AuthzClient> = Arc::new(fga);
    let counting = Arc::new(CountingAuthz {
        inner: Arc::clone(&raw),
        writes: AtomicUsize::new(0),
    });
    let store = DataStore::new(
        pool,
        Arc::clone(&counting) as Arc<dyn AuthzClient>,
        Arc::new(AllowRefs),
    );
    let index = Arc::new(FakeRecordIndex::default());
    // テストごとの台帳 consumer（他のテストの sink とイベントを取り合わない）。
    let worker = RagIndexWorker::new(
        store.clone(),
        Arc::clone(&index) as Arc<dyn RecordChunkSink>,
    )
    .with_consumer(format!("test-rag-index-{}", Uuid::new_v4().simple()));
    worker.register().await.unwrap();
    Some(Env {
        store,
        raw,
        counting,
        index,
        worker,
        tenant: format!("t-{}", Uuid::new_v4().simple()),
    })
}

impl Env {
    fn ctx(&self, user: &str) -> AuthContext {
        AuthContext::new(
            Principal {
                kind: authz::PrincipalKind::User,
                id: user.into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(self.tenant.clone()),
            },
            "acme".into(),
            self.tenant.clone(),
        )
    }

    /// outbox を消費し切る（他テストのイベントも流れてくるので 0 件になるまで回す）。
    async fn drain(&self) {
        for _ in 0..1_000 {
            if self.worker.run_once().await.unwrap() == 0 {
                return;
            }
        }
        panic!("outbox が消費し切れない");
    }

    async fn grant(&self, who: &AuthContext, rel: Relation, table: Uuid) {
        self.raw
            .write_tuple(
                &who.subject(),
                rel,
                &who.ns().data_table(&table.to_string()),
            )
            .await
            .unwrap();
    }

    /// 検索の PoC 経路: pre-filter → 取得（偽物）→ post-filter（本物）→ 結果。
    async fn search(&self, ctx: &AuthContext, query: &str, poison: bool) -> Run {
        let readable = readable_tables(ctx, self.counting.as_ref(), 500)
            .await
            .unwrap();
        let pre = if poison || readable.overflowed {
            Pre::TenantOnly
        } else {
            Pre::Tags(readable.tags)
        };
        let candidates = self.index.retrieve(ctx, query, &pre);
        let n = candidates.len();
        let out = post_filter_sources(
            ctx,
            self.counting.as_ref(),
            &DataVisibility(self.store.clone()),
            candidates,
        )
        .await
        .unwrap();
        Run {
            candidates: n,
            stale: out.stale_chunks,
            results: record_results(&out.allowed),
        }
    }
}

struct Run {
    candidates: usize,
    stale: usize,
    results: Vec<RecordSearchResult>,
}

impl Run {
    fn records(&self) -> Vec<Uuid> {
        let mut v: Vec<Uuid> = self.results.iter().map(|r| r.record_id).collect();
        v.sort();
        v
    }
}

fn sorted(mut v: Vec<Uuid>) -> Vec<Uuid> {
    v.sort();
    v
}

fn field(key: &str, ty: FieldType) -> FieldDef {
    FieldDef::new(key, ty)
}

/// 申請者本人 or 経理ロール（subtree）だけが行を読める経費テーブル（検索にオプトイン）。
fn expense_schema() -> TableSchema {
    let mut applicant = field("applicant", FieldType::UserRef);
    applicant.indexed = true;
    TableSchema {
        fields: vec![
            applicant,
            field("title", FieldType::Text),
            field("memo", FieldType::Text),
            field("salary", FieldType::Number),
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
        field_policy: vec![],
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: Some(RagProjection {
            title_field: "title".into(),
            body_fields: vec!["memo".into()],
        }),
    }
}

struct Fixture {
    table: Uuid,
    alice: AuthContext,
    bob: AuthContext,
    carol: AuthContext,
    dave: AuthContext,
    r_alice: Uuid,
    r_bob: Uuid,
    r_alice2: Uuid,
}

/// alice がテーブルを作り、bob（viewer）・carol（viewer＋経理）に見せる。dave は何も持たない。
async fn fixture(env: &Env) -> Fixture {
    let alice = env.ctx("alice");
    let bob = env.ctx("bob");
    let carol = env.ctx("carol");
    let dave = env.ctx("dave");
    let table = env
        .store
        .create_table(
            &alice,
            NewDataTable {
                name: "expense".into(),
                schema: expense_schema(),
            },
            None,
        )
        .await
        .expect("table")
        .id;
    env.grant(&bob, Relation::Viewer, table).await;
    env.grant(&carol, Relation::Viewer, table).await;
    env.raw
        .write_tuple(
            &carol.subject(),
            Relation::Member,
            &alice.ns().role("keiri"),
        )
        .await
        .unwrap();
    let mk = |who: &AuthContext, applicant: &str, title: &str, memo: &str| {
        let (store, who, table) = (env.store.clone(), who.clone(), table);
        let data = json!({"applicant": applicant, "title": title, "memo": memo, "salary": 900});
        async move {
            store
                .create_record(&who, table, data, None)
                .await
                .expect("record")
                .id
        }
    };
    let r_alice = mk(&alice, "alice", "大阪出張", "新幹線 交通費").await;
    let r_bob = mk(&alice, "bob", "東京出張", "ホテル 宿泊費").await;
    let r_alice2 = mk(&alice, "alice", "大阪会議", "会議室 交通費").await;
    env.drain().await;
    Fixture {
        table,
        alice,
        bob,
        carol,
        dave,
        r_alice,
        r_bob,
        r_alice2,
    }
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

/// 行レベル権限が検索結果に効く（閲覧者ごとに別の集合・剥奪で 0 件）。OpenFGA のタプルを書かない。
#[tokio::test]
async fn row_level_permissions_apply_to_record_search() {
    let _serial = serial_lock().lock().await;
    let Some(env) = setup().await else { return };
    let f = fixture(&env).await;

    // 索引は全行を持つ（可視性を焼き込まない）。タグは data_table の構造タグ。
    assert_eq!(env.index.chunk_bodies(&env.tenant, f.table).len(), 3);

    let writes_before = env.counting.writes.load(Ordering::SeqCst);

    // A（alice）は自分の行だけ。B（bob）は別の集合。経理（carol）は全行。
    let a = env.search(&f.alice, "出張", false).await;
    assert_eq!(a.records(), vec![f.r_alice]);
    assert_eq!(
        a.candidates, 2,
        "pre-filter はテーブル単位（行は post-filter が絞る）"
    );
    let b = env.search(&f.bob, "出張", false).await;
    assert_eq!(b.records(), vec![f.r_bob]);
    let c = env.search(&f.carol, "出張", false).await;
    assert_eq!(c.records(), sorted(vec![f.r_alice, f.r_bob]));
    let a2 = env.search(&f.alice, "交通費", false).await;
    assert_eq!(a2.records(), sorted(vec![f.r_alice, f.r_alice2]));
    // 引用本文は投影（タイトル＋「表示名: 値」）。投影外の列（salary）は載らない。
    let hit = &a.results[0];
    assert_eq!(hit.title, "大阪出張");
    assert_eq!(hit.content, "memo: 新幹線 交通費");
    assert!(!hit.content.contains("900"));

    // テーブル viewer を持たない dave: pre-filter で候補 0。pre-filter を汚染しても post-filter が落とす。
    let d = env.search(&f.dave, "出張", false).await;
    assert_eq!((d.candidates, d.results.len()), (0, 0));
    let d = env.search(&f.dave, "出張", true).await;
    assert_eq!(d.candidates, 2);
    assert!(d.results.is_empty(), "汚染された pre-filter でも見えない");
    // bob も同じ（tenant-only 縮退でも自分の行だけ）。
    let b = env.search(&f.bob, "出張", true).await;
    assert_eq!(b.records(), vec![f.r_bob]);
    // 別テナントは何も引けない（取得段のテナント条件＋post-filter のテナント境界）。
    let other = AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("t-other".into()),
        },
        "acme".into(),
        "t-other".into(),
    );
    let current_gen = env
        .index
        .tables
        .lock()
        .unwrap()
        .values()
        .find_map(|t| t.generation.clone());
    let o = post_filter_sources(
        &other,
        env.counting.as_ref(),
        &DataVisibility(env.store.clone()),
        vec![SourceCandidate {
            chunk_id: Uuid::new_v4(),
            source: SourceRef::Record {
                table_id: f.table,
                record_id: f.r_alice,
            },
            score: 1.0,
            generation: current_gen,
        }],
    )
    .await
    .unwrap();
    assert!(
        o.allowed.is_empty(),
        "別テナントの post-filter は全件落とす"
    );

    // 索引と検索の間に OpenFGA へ 1 本も書いていない（行を FGA オブジェクトにしない）。
    assert_eq!(env.counting.writes.load(Ordering::SeqCst), writes_before);
    for r in [f.r_alice, f.r_bob, f.r_alice2] {
        let tuples = env
            .raw
            .read_tuples(&f.alice.ns().data_record(&r.to_string()), None)
            .await
            .unwrap();
        assert!(tuples.is_empty(), "data_record のタプルは無い: {tuples:?}");
    }

    // 個別共有（既存の data_record スパースタプル・v1 の第 4 層）は post-filter に即反映される。
    env.store
        .share_record(
            &f.alice,
            f.table,
            f.r_alice,
            &storage::ShareTarget::User { id: "bob".into() },
            RecordShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let b = env.search(&f.bob, "出張", false).await;
    assert_eq!(b.records(), sorted(vec![f.r_alice, f.r_bob]));
    env.store
        .unshare_record(
            &f.alice,
            f.table,
            f.r_alice,
            &storage::ShareTarget::User { id: "bob".into() },
            RecordShareRole::Viewer,
            None,
        )
        .await
        .unwrap();
    let b = env.search(&f.bob, "出張", false).await;
    assert_eq!(b.records(), vec![f.r_bob], "共有解除は次の検索から消える");

    // テーブル viewer の剥奪 → 次の検索から 0 件（汚染された pre-filter でも 0 件）。
    env.raw
        .delete_tuple(
            &f.bob.subject(),
            Relation::Viewer,
            &f.alice.ns().data_table(&f.table.to_string()),
        )
        .await
        .unwrap();
    for poison in [false, true] {
        let b = env.search(&f.bob, "出張", poison).await;
        assert!(b.results.is_empty(), "剥奪後は見えない（poison={poison}）");
    }

    // 更新・削除は outbox 経由で索引に追従し、引用本文は現在の値から作り直す。
    env.store
        .update_record(
            &f.alice,
            f.table,
            f.r_alice,
            json!({"title": "福岡出張"}),
            1,
            None,
        )
        .await
        .unwrap();
    env.store
        .delete_record(&f.alice, f.table, f.r_alice2, 1, None)
        .await
        .unwrap();
    env.drain().await;
    let a = env.search(&f.alice, "福岡", false).await;
    assert_eq!(a.records(), vec![f.r_alice]);
    assert_eq!(a.results[0].rev, 2);
    let a = env.search(&f.alice, "会議", false).await;
    assert_eq!(a.candidates, 0, "削除した行のチャンクは索引から消える");

    env.worker.unregister().await.unwrap();
}

/// 投影にマスク対象列を含めるスキーマは拒否（作成・改訂の両方・422 = `DataError::Invalid`）。
#[tokio::test]
async fn masked_columns_cannot_be_projected() {
    let _serial = serial_lock().lock().await;
    let Some(env) = setup().await else { return };
    let alice = env.ctx("alice");
    let salary_masked = || FieldPolicy {
        field: "salary".into(),
        readable_by: PolicyExpr::HasRole {
            role: "hr".into(),
            subtree: false,
        },
    };
    // 作成時: 給与列（マスク対象）を本文に入れる → 拒否。
    let mut s = expense_schema();
    s.field_policy = vec![salary_masked()];
    s.rag.as_mut().unwrap().body_fields.push("salary".into());
    let r = env
        .store
        .create_table(
            &alice,
            NewDataTable {
                name: "hr".into(),
                schema: s,
            },
            None,
        )
        .await;
    assert!(
        matches!(r, Err(DataError::Invalid(ref m)) if m.contains("field_policy")),
        "{r:?}"
    );
    // 存在しない列も拒否（派生列・参照列の拒否は data の単体テストで固定）。
    let mut s = expense_schema();
    s.rag.as_mut().unwrap().title_field = "nope".into();
    assert!(matches!(
        env.store
            .create_table(
                &alice,
                NewDataTable {
                    name: "hr2".into(),
                    schema: s
                },
                None
            )
            .await,
        Err(DataError::Invalid(_))
    ));

    // 改訂時: 投影している memo 列に後から field_policy を付ける → 拒否（投影から外さない限り通らない）。
    let mut s = expense_schema();
    s.rag.as_mut().unwrap().body_fields.push("salary".into());
    let t = env
        .store
        .create_table(
            &alice,
            NewDataTable {
                name: "hr3".into(),
                schema: s,
            },
            None,
        )
        .await
        .unwrap();
    let mut next = t.schema.clone();
    next.field_policy = vec![salary_masked()];
    let r = env
        .store
        .update_table_schema(&alice, t.id, next, Some(t.schema_version), None)
        .await;
    assert!(
        matches!(r, Err(DataError::Invalid(ref m)) if m.contains("field_policy")),
        "{r:?}"
    );
    env.worker.unregister().await.unwrap();
}

/// `field_policy` の後付けで索引の世代が変わり、古いチャンクは再構築まで出ず、再構築で戻る。
/// マスク列を投影から外す改訂では、その列の値が索引から消える（破棄して作り直す）。
#[tokio::test]
async fn adding_field_policy_invalidates_and_rebuilds_index() {
    let _serial = serial_lock().lock().await;
    let Some(env) = setup().await else { return };
    let alice = env.ctx("alice");
    let carol = env.ctx("carol");
    // salary を本文に投影したテーブル（まだマスクされていない）。
    let mut s = expense_schema();
    s.rag.as_mut().unwrap().body_fields.push("salary".into());
    s.row_policy = None;
    let t = env
        .store
        .create_table(
            &alice,
            NewDataTable {
                name: "staff".into(),
                schema: s,
            },
            None,
        )
        .await
        .unwrap();
    env.grant(&carol, Relation::Viewer, t.id).await;
    let rec = env
        .store
        .create_record(
            &alice,
            t.id,
            json!({"applicant": "alice", "title": "山田", "memo": "営業部", "salary": 1234}),
            None,
        )
        .await
        .unwrap()
        .id;
    env.drain().await;
    assert!(env
        .index
        .chunk_bodies(&env.tenant, t.id)
        .iter()
        .any(|b| b.contains("1234")));
    let r = env.search(&carol, "山田", false).await;
    assert_eq!(r.records(), vec![rec]);
    assert!(r.results[0].content.contains("salary: 1234"));
    let resets_before = env.index.resets.load(Ordering::SeqCst);

    // salary にマスクを付け、同じ改訂で投影から外す（そうしないと拒否される）。
    let mut next = t.schema.clone();
    next.rag.as_mut().unwrap().body_fields = vec!["memo".into()];
    next.field_policy = vec![FieldPolicy {
        field: "salary".into(),
        readable_by: PolicyExpr::HasRole {
            role: "hr".into(),
            subtree: false,
        },
    }];
    let t2 = env
        .store
        .update_table_schema(&alice, t.id, next, Some(t.schema_version), None)
        .await
        .unwrap();

    // 再構築前: 索引には給与入りの古いチャンクが残っているが、世代不一致で post-filter が落とす。
    assert!(env
        .index
        .chunk_bodies(&env.tenant, t.id)
        .iter()
        .any(|b| b.contains("1234")));
    let r = env.search(&carol, "1234", false).await;
    assert_eq!(r.candidates, 1, "古いチャンクは取得段では当たる");
    assert!(r.results.is_empty(), "世代不一致で落ちる（fail-closed）");
    assert_eq!(r.stale, 1);

    // 増分索引が schema イベントで世代の切替を検出 → 破棄して作り直す。
    env.drain().await;
    assert_eq!(env.index.resets.load(Ordering::SeqCst), resets_before + 1);
    let bodies = env.index.chunk_bodies(&env.tenant, t.id);
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies.iter().all(|b| !b.contains("1234")),
        "マスク列の値は索引から消える: {bodies:?}"
    );
    let r = env.search(&carol, "1234", false).await;
    assert_eq!((r.candidates, r.results.len()), (0, 0));
    let r = env.search(&carol, "山田", false).await;
    assert_eq!(r.records(), vec![rec]);
    assert_eq!(r.results[0].content, "memo: 営業部");

    // 投影列以外への field_policy の追加でも世代は変わる（保守的に倒す）→ 再構築される。
    let mut next = t2.schema.clone();
    next.field_policy.push(FieldPolicy {
        field: "applicant".into(),
        readable_by: PolicyExpr::HasRole {
            role: "hr".into(),
            subtree: false,
        },
    });
    env.store
        .update_table_schema(&alice, t.id, next, Some(t2.schema_version), None)
        .await
        .unwrap();
    let r = env.search(&carol, "山田", false).await;
    assert!(r.results.is_empty(), "再構築前は落ちる");
    env.drain().await;
    assert_eq!(env.index.resets.load(Ordering::SeqCst), resets_before + 2);
    let r = env.search(&carol, "山田", false).await;
    assert_eq!(r.records(), vec![rec]);

    // オプトアウト（rag を外す）→ 索引から外れ、post-filter も全件落とす。
    let t3 = env.store.get_table(&alice, t.id, None).await.unwrap();
    let mut next = t3.schema.clone();
    next.rag = None;
    env.store
        .update_table_schema(&alice, t.id, next, Some(t3.schema_version), None)
        .await
        .unwrap();
    let r = env.search(&carol, "山田", false).await;
    assert!(r.results.is_empty());
    env.drain().await;
    assert!(env.index.chunk_bodies(&env.tenant, t.id).is_empty());
    env.worker.unregister().await.unwrap();
}
