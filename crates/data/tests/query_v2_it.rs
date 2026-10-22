//! クエリコンパイラ v2 の正しさ（Task 15.3・query-optimizer.md §9.2・PIT-76）。
//!
//! - **参照実装との乱数比較**: 本体を直接読んで（擬似列全走査に相当）、テスト側の独立した評価器
//!   （§3.1 の 2 値論理の表）で条件を評価し、全順序（ソートキー → `record_int`・値なしは末尾・
//!   切り詰め値はプレフィクス順）で並べた結果と、keyset ページングで全ページを辿った結果を比べる。
//!   データは偏り（状態の 80% が done）・相関（金額と作成順）・値なし・多値・切り詰め値を含む。
//! - **全戦略の強制**と**予算を極端に絞った切替**と**本体経路**で結果の集合と順序が一致する。
//! - **TLP**: `Q = (Q ∧ p) ∪ (Q ∧ ¬p)`（交わらない）。
//! - **行述語の意味論**: 値の無い行の `ne` は不成立（Guard と事後検査の両方・v1 の本体経路と同一）。
//! - 事後検査で落ちた行が 0 件・`with_count` 未指定で件数クエリが出ない・走査効率がテーブル単位で集計される。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）で実行する（authz は全許可モック）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::type_complexity
)]

mod common;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use authz::AuthContext;
use common::{create_table, ctx, field, schema, setup, store_with, unique_tenant};
use data::{
    CmpOp, CondOp, Condition, DataError, DataQuery, DataStore, FieldType, ForcedStrategy,
    PolicyExpr, PolicyOperand, QueryTuning, ReadPath, RowPolicy, SortKey, SortPrecision,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// データ
// ---------------------------------------------------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const STATUS: [&str; 4] = ["done", "open", "wip", "hold"];
const TAGS: [&str; 4] = ["a", "b", "c", "d"];

fn long_title(i: u64) -> String {
    // 同一プレフィクス（2,000B を超える共通部分）＋ 末尾だけ違う＝切り詰め値の同値群。
    format!("{}{i:04}", "長".repeat(800))
}

fn v2_schema() -> data::TableSchema {
    let mut title = field("title", FieldType::Text);
    title.indexed = true;
    title.searchable = true;
    let mut status = field("status", FieldType::Select);
    status.indexed = true;
    status.options = STATUS.iter().map(|s| (*s).to_string()).collect();
    let mut amount = field("amount", FieldType::Number);
    amount.indexed = true;
    let mut tags = field("tags", FieldType::MultiSelect);
    tags.indexed = true;
    tags.options = TAGS.iter().map(|s| (*s).to_string()).collect();
    let mut due = field("due", FieldType::Date);
    due.indexed = true;
    schema(vec![title, status, amount, tags, due])
}

fn record(r: &mut Rng, i: u64) -> Value {
    let mut m = serde_json::Map::new();
    match r.below(10) {
        0 => {} // 値なし
        1 | 2 => {
            m.insert("title".into(), json!(long_title(r.below(6))));
        }
        _ => {
            m.insert(
                "title".into(),
                json!(format!(
                    "t{:02}{}",
                    r.below(30),
                    ["", "x", "ab"][r.below(3) as usize]
                )),
            );
        }
    }
    // 偏り: 80% が done。相関: 新しい行ほど open が多い。
    let st = if i > 300 && r.below(3) == 0 {
        "open"
    } else if r.below(10) < 8 {
        "done"
    } else {
        STATUS[r.below(4) as usize]
    };
    m.insert("status".into(), json!(st));
    if r.below(5) != 0 {
        // 金額は作成順と相関（＋ノイズ）・小数と負数を含む。
        let base = (i as i64) / 4 - 20 + (r.below(7) as i64);
        let v = if r.below(6) == 0 {
            json!(base as f64 + 0.5)
        } else {
            json!(base)
        };
        m.insert("amount".into(), v);
    }
    let n = r.below(4);
    let mut tags: Vec<&str> = (0..n).map(|_| TAGS[r.below(4) as usize]).collect();
    tags.sort_unstable();
    tags.dedup();
    if !tags.is_empty() {
        m.insert("tags".into(), json!(tags));
    }
    if r.below(3) != 0 {
        m.insert(
            "due".into(),
            json!(format!("2026-0{}-1{}", 1 + r.below(9), r.below(10))),
        );
    }
    Value::Object(m)
}

/// 本体を直接読む（参照実装の入力・テスト専用に record_int も読む）。
struct Row {
    id: Uuid,
    rint: i64,
    owner: String,
    created: String,
    updated: String,
    data: HashMap<String, Value>,
}

async fn load_rows(pool: &PgPool, tenant: &str, table: &data::DataTable) -> Vec<Row> {
    let rows: Vec<(Uuid, i64, String, String, String, Value)> = sqlx::query_as(
        "SELECT id, record_int, owner, \
           to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), \
           to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), data \
         FROM data_record WHERE tenant_id = $1 AND table_id = $2",
    )
    .bind(tenant)
    .bind(table.id)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(id, rint, owner, created, updated, data)| {
            let mut m = HashMap::new();
            for f in &table.schema.fields {
                if let Some(v) = data.get(format!("f{}", f.id)) {
                    if !v.is_null() {
                        m.insert(f.key.clone(), v.clone());
                    }
                }
            }
            Row {
                id,
                rint,
                owner,
                created,
                updated,
                data: m,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 参照実装（§3.1 の表をテスト側で独立に実装）
// ---------------------------------------------------------------------------

fn values_of(row: &Row, key: &str) -> Vec<Value> {
    match key {
        "_owner" => vec![json!(row.owner)],
        "_created_at" => vec![json!(row.created)],
        "_updated_at" => vec![json!(row.updated)],
        _ => match row.data.get(key) {
            None => vec![],
            Some(Value::Array(a)) => a.clone(),
            Some(v) => vec![v.clone()],
        },
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn cmp_vals(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::String(x), Value::String(y)) => Some(x.as_bytes().cmp(y.as_bytes())),
        _ => num(a)?.partial_cmp(&num(b)?),
    }
}

fn eval(c: &Condition, row: &Row) -> bool {
    match c {
        Condition::All(v) => v.iter().all(|c| eval(c, row)),
        Condition::Any(v) => v.iter().any(|c| eval(c, row)),
        Condition::Not(x) => !eval(x, row),
        Condition::Cmp(cmp) => {
            let vals = values_of(row, &cmp.field);
            let arg = cmp.value.clone().unwrap_or(Value::Null);
            let eq = |v: &Value, a: &Value| cmp_vals(v, a) == Some(Ordering::Equal);
            match cmp.op {
                CondOp::Exists => !vals.is_empty(),
                CondOp::IsNull => vals.is_empty(),
                CondOp::Eq => vals.iter().any(|v| eq(v, &arg)),
                CondOp::Neq => !vals.iter().any(|v| eq(v, &arg)),
                CondOp::In => vals
                    .iter()
                    .any(|v| arg.as_array().unwrap().iter().any(|a| eq(v, a))),
                CondOp::Lt => vals
                    .iter()
                    .any(|v| cmp_vals(v, &arg) == Some(Ordering::Less)),
                CondOp::Lte => vals
                    .iter()
                    .any(|v| matches!(cmp_vals(v, &arg), Some(Ordering::Less | Ordering::Equal))),
                CondOp::Gt => vals
                    .iter()
                    .any(|v| cmp_vals(v, &arg) == Some(Ordering::Greater)),
                CondOp::Gte => vals.iter().any(|v| {
                    matches!(cmp_vals(v, &arg), Some(Ordering::Greater | Ordering::Equal))
                }),
                CondOp::StartsWith => vals
                    .iter()
                    .any(|v| v.as_str().unwrap().starts_with(arg.as_str().unwrap())),
                CondOp::Contains => vals
                    .iter()
                    .any(|v| v.as_str().unwrap().contains(arg.as_str().unwrap())),
                CondOp::EndsWith => vals
                    .iter()
                    .any(|v| v.as_str().unwrap().ends_with(arg.as_str().unwrap())),
                CondOp::Matches => unreachable!(),
            }
        }
    }
}

/// ソート値（text は索引のプレフィクス＝先頭 2,000B を文字境界で）。
fn sort_value(row: &Row, key: &str) -> Option<Value> {
    let v = values_of(row, key).into_iter().next()?;
    Some(match v {
        Value::String(s) => {
            let mut end = s.len().min(2_000);
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            Value::String(s[..end].to_string())
        }
        other => other,
    })
}

fn reference(rows: &[Row], q: &DataQuery) -> Vec<Uuid> {
    let mut hit: Vec<&Row> = rows
        .iter()
        .filter(|r| q.filter.as_ref().is_none_or(|c| eval(c, r)))
        .collect();
    let sort: Vec<SortKey> = if q.sort.is_empty() {
        vec![SortKey {
            field: "_updated_at".into(),
            descending: true,
        }]
    } else {
        q.sort.clone()
    };
    hit.sort_by(|a, b| {
        for k in &sort {
            let (x, y) = (sort_value(a, &k.field), sort_value(b, &k.field));
            let o = match (x, y) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => return Ordering::Greater,
                (Some(_), None) => return Ordering::Less,
                (Some(x), Some(y)) => cmp_vals(&x, &y).unwrap(),
            };
            let o = if k.descending { o.reverse() } else { o };
            if o != Ordering::Equal {
                return o;
            }
        }
        let o = a.rint.cmp(&b.rint);
        if sort[0].descending {
            o.reverse()
        } else {
            o
        }
    });
    hit.into_iter().map(|r| r.id).collect()
}

// ---------------------------------------------------------------------------
// 乱数クエリ
// ---------------------------------------------------------------------------

fn leaf(r: &mut Rng) -> Condition {
    let c = |f: &str, op, v: Option<Value>| Condition::cmp(f.to_string(), op, v);
    match r.below(14) {
        0 => c(
            "status",
            CondOp::Eq,
            Some(json!(STATUS[r.below(4) as usize])),
        ),
        1 => c(
            "status",
            CondOp::Neq,
            Some(json!(STATUS[r.below(4) as usize])),
        ),
        2 => c(
            "status",
            CondOp::In,
            Some(json!([
                STATUS[r.below(4) as usize],
                STATUS[r.below(4) as usize]
            ])),
        ),
        3 => c("amount", CondOp::Lt, Some(json!(r.below(80) as i64 - 10))),
        4 => c("amount", CondOp::Gte, Some(json!(r.below(80) as i64 - 10))),
        5 => c("amount", CondOp::Eq, Some(json!(r.below(40) as i64))),
        6 => c("amount", CondOp::IsNull, None),
        7 => c("tags", CondOp::Eq, Some(json!(TAGS[r.below(4) as usize]))),
        8 => c("tags", CondOp::Neq, Some(json!(TAGS[r.below(4) as usize]))),
        9 => c("tags", CondOp::Exists, None),
        10 => c(
            "title",
            CondOp::StartsWith,
            Some(json!(["t0", "t1", "長長", "t2"][r.below(4) as usize])),
        ),
        11 => c(
            "title",
            CondOp::Contains,
            Some(json!(["x", "ab", "長0", "長00"][r.below(4) as usize])),
        ),
        12 => c(
            "title",
            CondOp::Eq,
            Some(json!(if r.below(2) == 0 {
                long_title(r.below(6))
            } else {
                format!("t{:02}", r.below(30))
            })),
        ),
        _ => c(
            "due",
            CondOp::Gt,
            Some(json!(format!("2026-0{}-15", 1 + r.below(9)))),
        ),
    }
}

fn cond(r: &mut Rng, depth: u32) -> Condition {
    if depth == 0 {
        return leaf(r);
    }
    match r.below(5) {
        0 => Condition::Not(Box::new(cond(r, depth - 1))),
        1 => Condition::All((0..2 + r.below(2)).map(|_| cond(r, depth - 1)).collect()),
        2 => Condition::Any((0..2 + r.below(2)).map(|_| cond(r, depth - 1)).collect()),
        _ => leaf(r),
    }
}

fn sort(r: &mut Rng) -> Vec<SortKey> {
    let k = |f: &str, d: bool| SortKey {
        field: f.into(),
        descending: d,
    };
    match r.below(7) {
        0 => vec![],
        1 => vec![k("amount", r.below(2) == 0)],
        2 => vec![k("status", false), k("amount", true)],
        3 => vec![k("title", r.below(2) == 0)],
        4 => vec![k("_created_at", true)],
        5 => vec![k("due", false), k("title", false)],
        _ => vec![k("amount", false), k("_created_at", false)],
    }
}

// ---------------------------------------------------------------------------
// 実行
// ---------------------------------------------------------------------------

/// keyset で全ページを辿る（ページ間の重複が無いことも確かめる）。
async fn all_pages(
    store: &DataStore,
    c: &AuthContext,
    table: Uuid,
    q: &DataQuery,
    tuning: &QueryTuning,
) -> (Vec<Uuid>, data::QueryExecStats) {
    let mut out = Vec::new();
    let mut q = q.clone();
    for _ in 0..1_000 {
        let (r, dbg) = store.run_query_debug(c, table, &q, tuning).await.unwrap();
        assert_eq!(dbg.stats.verify_dropped, 0, "事後検査で行が落ちた: {q:?}");
        out.extend(r.items.unwrap().into_iter().map(|x| x.id));
        match r.next_cursor {
            Some(cur) => q.cursor = Some(cur),
            None => return (out, dbg.stats),
        }
    }
    panic!("ページングが終わらない: {q:?}");
}

fn tunings() -> Vec<(&'static str, QueryTuning)> {
    let f = |s| QueryTuning {
        force: Some(s),
        ..Default::default()
    };
    vec![
        ("auto", QueryTuning::default()),
        ("sort", f(ForcedStrategy::Sort)),
        ("filter", f(ForcedStrategy::Filter)),
        ("union", f(ForcedStrategy::Union)),
        ("policy", f(ForcedStrategy::Policy)),
        ("safe", f(ForcedStrategy::Safe)),
        (
            "budget1",
            QueryTuning {
                sort_budget: Some(1),
                ..Default::default()
            },
        ),
        (
            "body",
            QueryTuning {
                read_path: ReadPath::Body,
                ..Default::default()
            },
        ),
    ]
}

async fn seeded(pool: &PgPool, n: u64) -> (DataStore, AuthContext, String, data::DataTable) {
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = create_table(&store, &c, "v2", v2_schema()).await;
    let mut r = Rng(0x5eed_1234_abcd);
    for i in 0..n {
        store
            .create_record(&c, table.id, record(&mut r, i), None)
            .await
            .unwrap();
    }
    (store, c, tenant, table)
}

#[tokio::test]
async fn random_queries_match_reference_under_every_strategy() {
    let Some(pool) = setup().await else { return };
    let (store, c, tenant, table) = seeded(&pool, 360).await;
    let rows = load_rows(&pool, &tenant, &table).await;
    let mut r = Rng(0xabc0_0123_4567);
    let mut nonempty = 0;
    for qi in 0..40 {
        let q = DataQuery {
            filter: (r.below(8) != 0).then(|| cond(&mut r, 2)),
            sort: sort(&mut r),
            limit: Some(1 + r.below(40) as u32),
            ..Default::default()
        };
        let want = reference(&rows, &q);
        nonempty += usize::from(!want.is_empty());
        let text_sort = q.sort.iter().any(|k| k.field == "title");
        for (name, t) in tunings() {
            let (got, _) = all_pages(&store, &c, table.id, &q, &t).await;
            if name == "body" && text_sort {
                // 本体経路は完全値で並べる（索引経路はプレフィクス順の契約）。集合だけ比べる。
                let a: HashSet<_> = got.iter().collect();
                let b: HashSet<_> = want.iter().collect();
                assert_eq!(a, b, "q{qi} {name}: {q:?}");
                assert_eq!(got.len(), want.len(), "q{qi} {name}: 重複");
            } else {
                assert_eq!(got, want, "q{qi} {name}: {q:?}");
            }
        }
    }
    assert!(nonempty > 20, "空でない結果が十分ある: {nonempty}");
}

#[tokio::test]
async fn tlp_partitions_hold_through_sql() {
    let Some(pool) = setup().await else { return };
    let (store, c, _, table) = seeded(&pool, 200).await;
    let mut r = Rng(77);
    for _ in 0..15 {
        let q = cond(&mut r, 2);
        let p = cond(&mut r, 1);
        let run = |f: Condition| {
            let store = store.clone();
            let c = c.clone();
            async move {
                let q = DataQuery {
                    filter: Some(f),
                    limit: Some(200),
                    ..Default::default()
                };
                all_pages(&store, &c, table.id, &q, &QueryTuning::default())
                    .await
                    .0
            }
        };
        let all: HashSet<Uuid> = run(q.clone()).await.into_iter().collect();
        let yes: HashSet<Uuid> = run(Condition::All(vec![q.clone(), p.clone()]))
            .await
            .into_iter()
            .collect();
        let no: HashSet<Uuid> = run(Condition::All(vec![q.clone(), Condition::Not(Box::new(p))]))
            .await
            .into_iter()
            .collect();
        assert!(yes.is_disjoint(&no));
        assert_eq!(all, yes.union(&no).copied().collect());
    }
}

#[tokio::test]
async fn nullable_sort_keeps_rows_without_value_at_the_end() {
    let Some(pool) = setup().await else { return };
    let (store, c, tenant, table) = seeded(&pool, 120).await;
    let rows = load_rows(&pool, &tenant, &table).await;
    let missing: HashSet<Uuid> = rows
        .iter()
        .filter(|r| !r.data.contains_key("amount"))
        .map(|r| r.id)
        .collect();
    assert!(!missing.is_empty());
    for desc in [false, true] {
        let q = DataQuery {
            sort: vec![SortKey {
                field: "amount".into(),
                descending: desc,
            }],
            limit: Some(7),
            ..Default::default()
        };
        let (got, _) = all_pages(&store, &c, table.id, &q, &QueryTuning::default()).await;
        assert_eq!(got.len(), rows.len(), "可視行が欠落しない（desc={desc}）");
        let tail: HashSet<Uuid> = got[got.len() - missing.len()..].iter().copied().collect();
        assert_eq!(tail, missing, "値なしは末尾（desc={desc}）");
    }
}

#[tokio::test]
async fn truncated_sort_contract_and_precision_flag() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let c = ctx(&tenant, "alice");
    let table = create_table(&store, &c, "prefix", v2_schema()).await;
    for i in 0..60 {
        store
            .create_record(
                &c,
                table.id,
                json!({"title": long_title(i % 7), "status": "done"}),
                None,
            )
            .await
            .unwrap();
    }
    let q = DataQuery {
        sort: vec![SortKey {
            field: "title".into(),
            descending: false,
        }],
        limit: Some(9),
        ..Default::default()
    };
    let (r, _) = store
        .run_query_debug(&c, table.id, &q, &QueryTuning::default())
        .await
        .unwrap();
    assert_eq!(r.sort_precision, SortPrecision::Prefix);
    let (got, _) = all_pages(&store, &c, table.id, &q, &QueryTuning::default()).await;
    let uniq: HashSet<_> = got.iter().collect();
    assert_eq!(
        (got.len(), uniq.len()),
        (60, 60),
        "同一プレフィクス多数でも重複・欠落なし"
    );
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["sort_precision"], json!("prefix"));
}

#[tokio::test]
async fn count_is_opt_in_and_scan_metrics_are_recorded() {
    let Some(pool) = setup().await else { return };
    let (store, c, _, table) = seeded(&pool, 80).await;
    let q = DataQuery {
        filter: Some(Condition::cmp("status", CondOp::Eq, Some(json!("done")))),
        limit: Some(5),
        ..Default::default()
    };
    let (r, dbg) = store
        .run_query_debug(&c, table.id, &q, &QueryTuning::default())
        .await
        .unwrap();
    assert!(r.count.is_none());
    assert!(
        !dbg.stats.count_issued,
        "with_count 未指定で件数クエリを出さない"
    );
    assert!(!dbg.statements.iter().any(|s| s.label == "count"));
    let counted = DataQuery {
        with_count: true,
        ..q.clone()
    };
    let (r, dbg) = store
        .run_query_debug(&c, table.id, &counted, &QueryTuning::default())
        .await
        .unwrap();
    assert!(dbg.stats.count_issued);
    let total = store
        .run_query(
            &c,
            table.id,
            &DataQuery {
                limit: Some(200),
                ..q.clone()
            },
            None,
        )
        .await
        .unwrap()
        .items
        .unwrap()
        .len() as u64;
    assert_eq!(r.count, Some(data::CountResult::Exact(total)));
    // 上限（テナント設定で変更可）に達すると AtLeast。
    let capped = store_with(pool.clone()).with_count_cap(3);
    let r = capped
        .run_query(&c, table.id, &counted, None)
        .await
        .unwrap();
    assert_eq!(r.count, Some(data::CountResult::AtLeast(3)));
    // 走査効率がテーブル単位で集計される（store は自分の実行だけを数える）。
    let m = store.query_metrics(table.id).expect("指標がある");
    assert!(m.requests >= 2 && m.scanned > 0 && m.returned > 0, "{m:?}");
    assert!(m.scan_ratio_p95 >= 1.0, "{m:?}");
    assert_eq!(m.verify_dropped, 0);
}

#[tokio::test]
async fn budget_switch_and_union_drive_are_observable() {
    let Some(pool) = setup().await else { return };
    let (store, c, tenant, table) = seeded(&pool, 360).await;
    let rows = load_rows(&pool, &tenant, &table).await;
    // 「hold」（まれ）を金額順: ソート駆動は予算切れ → フィルタ駆動へ切り替わる。結果は参照と一致。
    let q = DataQuery {
        filter: Some(Condition::cmp("status", CondOp::Eq, Some(json!("hold")))),
        sort: vec![SortKey {
            field: "amount".into(),
            descending: true,
        }],
        limit: Some(5),
        ..Default::default()
    };
    let (r, dbg) = store
        .run_query_debug(
            &c,
            table.id,
            &q,
            &QueryTuning {
                sort_budget: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        dbg.stats.switched || dbg.stats.doublings > 0,
        "{:?}",
        dbg.stats
    );
    let got: Vec<Uuid> = r.items.unwrap().into_iter().map(|x| x.id).collect();
    assert_eq!(got, reference(&rows, &q)[..got.len()]);
    // 別の列どうしの OR（各項が絞れている）は OR 駆動。
    let q = DataQuery {
        filter: Some(Condition::Any(vec![
            Condition::cmp("status", CondOp::Eq, Some(json!("hold"))),
            Condition::cmp("title", CondOp::Eq, Some(json!("t05"))),
        ])),
        sort: vec![SortKey {
            field: "_created_at".into(),
            descending: false,
        }],
        limit: Some(3),
        ..Default::default()
    };
    let (_, dbg) = store
        .run_query_debug(&c, table.id, &q, &QueryTuning::default())
        .await
        .unwrap();
    assert_eq!(dbg.stats.strategy, "union", "{:?}", dbg.stats);
}

// ---------------------------------------------------------------------------
// 行述語（v1 の意味論）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn policy_ne_on_missing_value_is_false_on_every_path() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool.clone());
    let tenant = unique_tenant();
    let alice = ctx(&tenant, "alice");
    let mut level = field("level", FieldType::Select);
    level.indexed = true;
    level.options = vec!["public".into(), "secret".into()];
    let mut status = field("status", FieldType::Select);
    status.indexed = true;
    status.options = vec!["a".into(), "b".into()];
    let mut s = schema(vec![level, status]);
    // 「secret でない行」だけ見える。level が無い行は v1 では不成立（fail-closed）。
    s.row_policy = Some(RowPolicy {
        read: PolicyExpr::FieldCmp {
            field: "level".into(),
            op: CmpOp::Ne,
            value: PolicyOperand::Lit(json!("secret")),
        },
        write: None,
    });
    let table = create_table(&store, &alice, "ne", s).await;
    let mut visible = HashSet::new();
    for (i, data) in [
        json!({"level": "public", "status": "a"}),
        json!({"level": "secret", "status": "a"}),
        json!({"status": "a"}),
        json!({"level": "public", "status": "b"}),
        json!({}),
    ]
    .into_iter()
    .enumerate()
    {
        let rec = store.create_record(&alice, table.id, data, None).await;
        // 作成は書込述語（= read）を通らない行でも可能（作成時は述語をかけない v1 の挙動）。
        let rec = rec.unwrap();
        if i == 0 || i == 3 {
            visible.insert(rec.id);
        }
    }
    for (name, t) in tunings() {
        for filter in [
            None,
            Some(Condition::cmp("status", CondOp::Eq, Some(json!("a")))),
        ] {
            let q = DataQuery {
                filter: filter.clone(),
                limit: Some(2),
                ..Default::default()
            };
            let (got, stats) = all_pages(&store, &alice, table.id, &q, &t).await;
            let got: HashSet<Uuid> = got.into_iter().collect();
            let want: HashSet<Uuid> = if filter.is_some() {
                visible.iter().take(1).copied().collect::<HashSet<_>>()
            } else {
                visible.clone()
            };
            if filter.is_none() {
                assert_eq!(got, want, "{name}: {stats:?}");
            } else {
                assert!(got.len() == 1 && got.is_subset(&visible), "{name}: {got:?}");
            }
        }
    }
    // 件数・集計も同じ（事後検査が効かない経路は Guard だけで正しい）。
    for (name, t) in tunings() {
        let (r, _) = store
            .run_query_debug(
                &alice,
                table.id,
                &DataQuery {
                    with_count: true,
                    limit: Some(0),
                    ..Default::default()
                },
                &t,
            )
            .await
            .unwrap();
        assert_eq!(r.count, Some(data::CountResult::Exact(2)), "{name}");
    }
    assert_eq!(
        DataStore::post_filter_drops_total(),
        0,
        "事後検査で落ちた行が 0"
    );
}

#[tokio::test]
async fn row_policy_on_unindexed_field_is_422() {
    let Some(pool) = setup().await else { return };
    let store = store_with(pool);
    let c = ctx(&unique_tenant(), "alice");
    let mut s = schema(vec![field("applicant", FieldType::UserRef)]);
    s.row_policy = Some(RowPolicy {
        read: PolicyExpr::FieldCmp {
            field: "applicant".into(),
            op: CmpOp::Eq,
            value: PolicyOperand::UserId,
        },
        write: None,
    });
    let r = store
        .create_table(
            &c,
            data::NewDataTable {
                name: "x".into(),
                schema: s,
            },
            None,
        )
        .await;
    assert!(
        matches!(r, Err(DataError::Invalid(ref m)) if m.contains("indexed")),
        "{r:?}"
    );
}

#[tokio::test]
async fn aggregates_match_between_paths_with_small_cell_suppression() {
    let Some(pool) = setup().await else { return };
    let (store, c, _, table) = seeded(&pool, 150).await;
    let q = DataQuery {
        filter: Some(Condition::All(vec![
            Condition::cmp("tags", CondOp::Exists, None),
            Condition::cmp("amount", CondOp::Lt, Some(json!(10))),
        ])),
        group_by: vec!["status".into()],
        aggregate: vec![
            data::MetricSpec {
                metric: data::Metric::Count,
                field: None,
            },
            data::MetricSpec {
                metric: data::Metric::Sum,
                field: Some("amount".into()),
            },
        ],
        ..Default::default()
    };
    let mut results = Vec::new();
    for (name, t) in tunings() {
        let (r, _) = store.run_query_debug(&c, table.id, &q, &t).await.unwrap();
        let mut g = r.groups.unwrap();
        g.sort_by(|a, b| {
            a.key["status"]
                .to_string()
                .cmp(&b.key["status"].to_string())
        });
        results.push((name, g, r.suppressed));
    }
    for (name, g, sup) in &results[1..] {
        assert_eq!(g, &results[0].1, "{name}");
        assert_eq!(sup, &results[0].2, "{name}");
    }
    // 少人数のグループ（hold など）は値が伏せられる。
    assert!(results[0].2, "スモールセル抑制が起きる: {:?}", results[0].1);
    assert!(results[0]
        .1
        .iter()
        .any(|g| g.value.is_null() && g.values.iter().all(Value::is_null)));
}
