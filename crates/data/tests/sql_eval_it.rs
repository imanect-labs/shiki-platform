//! LLM の生成成功率の評価（Task 15.10・data-platform §16.1「なぜ必要か」）。
//!
//! 同じ 50 問（5 カテゴリ × 10 問・`tests/sql_eval/questions.json`）を、同じスキーマ説明・同じ例の数で
//! **IR 直投入（ベースライン）→ SQL 前段面**の順に LLM へ書かせ、実データで実行して正解の SQL の結果と比べる。
//! 失敗は 構文エラー／部分集合外／列・テーブル名誤り／意味的に不正 に分類する。
//!
//! - `gold_answers_are_valid`: 正解の SQL がすべて実行でき、結果が空でなく、正解の SQL を IR に変換したものを
//!   IR 直投入で流しても評価器の判定が一致する（評価器自体の検証・常に走る）。
//! - `llm_generation_success_rate`: `SHIKI_SQL_EVAL_URL`（OpenAI 互換の chat/completions）と
//!   `SHIKI_SQL_EVAL_MODELS`（カンマ区切り）があるときだけ走る。任意で `SHIKI_SQL_EVAL_API_KEY`・
//!   `SHIKI_SQL_EVAL_HEADER`（`名前: 値` の追加ヘッダ）・`SHIKI_SQL_EVAL_CA_BUNDLE`（PEM）・
//!   `SHIKI_SQL_EVAL_OUT`（結果 JSON の出力先ディレクトリ）。結果は `docs/data/sql-eval.md` に記録する。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）で実行する（無ければスキップ）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::struct_field_names
)]

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use authz::AuthContext;
use common::{create_table, ctx, field, schema, setup, store_with, unique_tenant};
use data::{
    DataError, DataQuery, DataStore, FieldType, LinkCardinality, LinkDef, QueryResult, SqlRequest,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
struct Question {
    id: String,
    category: String,
    question: String,
    gold: String,
    #[serde(default)]
    ordered: bool,
    #[serde(default)]
    set: bool,
}

fn questions() -> Vec<Question> {
    serde_json::from_str(include_str!("sql_eval/questions.json")).unwrap()
}

const SCHEMA: &str = include_str!("sql_eval/schema.md");
const PROMPT_SQL: &str = include_str!("sql_eval/prompt_sql.md");
const PROMPT_IR: &str = include_str!("sql_eval/prompt_ir.md");

// ---------------------------------------------------------------- データ

fn indexed(key: &str, ty: FieldType) -> data::FieldDef {
    let mut f = field(key, ty);
    f.indexed = true;
    f
}

fn text(key: &str) -> data::FieldDef {
    let mut f = indexed(key, FieldType::Text);
    f.searchable = true;
    f
}

fn select(key: &str, opts: &[&str]) -> data::FieldDef {
    let mut f = indexed(key, FieldType::Select);
    f.options = opts.iter().map(|s| (*s).to_string()).collect();
    f
}

fn link(key: &str, to: Uuid, sym: &str) -> data::FieldDef {
    let mut f = indexed(key, FieldType::RecordRef);
    f.ref_table = Some(to);
    f.link = Some(LinkDef {
        cardinality: LinkCardinality::ManyToOne,
        symmetric_field: None,
        symmetric_key: Some(sym.into()),
        reverse: false,
    });
    f
}

fn table_schema(fields: Vec<data::FieldDef>) -> data::TableSchema {
    let mut s = schema(fields);
    s.aggregate_min_rows = Some(1);
    s
}

/// 顧客名と商品（名前・カテゴリ・価格）。
#[derive(Deserialize)]
struct Seed {
    customers: Vec<String>,
    products: Vec<(String, String, i64)>,
}

const CITIES: [&str; 5] = ["東京", "大阪", "福岡", "名古屋", "札幌"];
const TIERS: [&str; 3] = ["gold", "silver", "bronze"];

fn date_plus(base: chrono::NaiveDate, days: i64) -> String {
    (base + chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string()
}

struct Eval {
    store: DataStore,
    who: AuthContext,
    tables: HashMap<String, Uuid>,
}

/// 決定的なデータ（金額・価格・日付は一意＝並べ替えの同値が出ない）。
async fn seed() -> Option<Eval> {
    let pool = setup().await?;
    let store = store_with(pool);
    let who = ctx(&unique_tenant(), "alice");
    let customers = create_table(
        &store,
        &who,
        "customers",
        table_schema(vec![
            text("name"),
            select("city", &CITIES),
            select("tier", &TIERS),
            indexed("signup_date", FieldType::Date),
        ]),
    )
    .await;
    let products = create_table(
        &store,
        &who,
        "products",
        table_schema(vec![
            text("name"),
            select("category", &["文具", "家電", "家具", "食品"]),
            indexed("price", FieldType::Number),
        ]),
    )
    .await;
    let orders = create_table(
        &store,
        &who,
        "orders",
        table_schema(vec![
            text("order_no"),
            link("customer", customers.id, "orders"),
            select("status", &["受付", "出荷済", "キャンセル"]),
            indexed("amount", FieldType::Number),
            indexed("ordered_on", FieldType::Date),
            select("channel", &["web", "店舗", "電話"]),
        ]),
    )
    .await;
    let lines = create_table(
        &store,
        &who,
        "order_lines",
        table_schema(vec![
            link("order_ref", orders.id, "lines"),
            link("product", products.id, "lines"),
            indexed("qty", FieldType::Number),
            indexed("unit_price", FieldType::Number),
        ]),
    )
    .await;
    let signup0 = chrono::NaiveDate::from_ymd_opt(2023, 1, 10).unwrap();
    let mut cids = Vec::new();
    let seed: Seed = serde_json::from_str(include_str!("sql_eval/seed.json")).unwrap();
    for (i, name) in seed.customers.iter().enumerate() {
        let r = store
            .create_record(
                &who,
                customers.id,
                json!({
                    "name": name,
                    "city": CITIES[(i * 3 + i / 5) % 5],
                    "tier": TIERS[(i * 2 + i / 3) % 3],
                    "signup_date": date_plus(signup0, i as i64 * 37),
                }),
                None,
            )
            .await
            .unwrap();
        cids.push(r.id);
    }
    let mut pids = Vec::new();
    for (name, cat, price) in &seed.products {
        let r = store
            .create_record(
                &who,
                products.id,
                json!({"name": name, "category": cat, "price": price}),
                None,
            )
            .await
            .unwrap();
        pids.push((r.id, price));
    }
    let day0 = chrono::NaiveDate::from_ymd_opt(2025, 8, 15).unwrap();
    for i in 0..200usize {
        // 出荷済 60%・受付 25%・キャンセル 15%／web 60%・店舗 20%・電話 20%。
        let status = match i % 20 {
            0..=11 => "出荷済",
            12..=16 => "受付",
            _ => "キャンセル",
        };
        let channel = match i % 5 {
            0..=2 => "web",
            3 => "店舗",
            _ => "電話",
        };
        let amount = 1200 + ((i * 73) % 200) as i64 * 497;
        let o = store
            .create_record(
                &who,
                orders.id,
                json!({
                    "order_no": format!("O-{:04}", i + 1),
                    "customer": [cids[(i * 7) % cids.len()].to_string()],
                    "status": status,
                    "amount": amount,
                    "ordered_on": date_plus(day0, i as i64),
                    "channel": channel,
                }),
                None,
            )
            .await
            .unwrap();
        for k in 0..=(i % 3) {
            let (pid, price) = pids[(i * 5 + k * 11) % pids.len()];
            store
                .create_record(
                    &who,
                    lines.id,
                    json!({
                        "order_ref": [o.id.to_string()],
                        "product": [pid.to_string()],
                        "qty": 1 + (i + k) % 5,
                        "unit_price": price,
                    }),
                    None,
                )
                .await
                .unwrap();
        }
    }
    let tables = [
        ("customers", customers.id),
        ("products", products.id),
        ("orders", orders.id),
        ("order_lines", lines.id),
    ]
    .into_iter()
    .map(|(n, id)| (n.to_string(), id))
    .collect();
    Some(Eval { store, who, tables })
}

// ---------------------------------------------------------------- 結果の比較

/// 表形式に揃えた結果（値は正規化した文字列）。
#[derive(Debug, Clone)]
struct Table {
    cols: Vec<String>,
    rows: Vec<Vec<String>>,
}

fn norm(v: &Value) -> String {
    let num = |x: f64| format!("{x:.4}");
    match v {
        Value::Null => "∅".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), num),
        Value::String(s) => match s.trim().parse::<f64>() {
            Ok(x) if x.is_finite() => num(x),
            _ => s.clone(),
        },
        Value::Array(a) if a.len() == 1 => norm(&a[0]),
        other => other.to_string(),
    }
}

fn from_sql(r: &data::SqlQueryResult) -> Table {
    Table {
        cols: r.columns.clone(),
        rows: r
            .rows
            .iter()
            .map(|row| row.iter().map(norm).collect())
            .collect(),
    }
}

/// IR の結果を表に揃える（集計はグループの値＋指標の値・行は全列＋`_id`）。
fn from_ir(q: &DataQuery, r: &QueryResult, into: &mut Table) {
    if let Some(groups) = &r.groups {
        if into.cols.is_empty() {
            into.cols = q
                .group_by
                .iter()
                .cloned()
                .chain((0..q.aggregate.len()).map(|i| format!("m{i}")))
                .collect();
        }
        for g in groups {
            let mut row: Vec<String> = q
                .group_by
                .iter()
                .map(|k| norm(g.key.get(k).unwrap_or(&Value::Null)))
                .collect();
            row.extend(g.values.iter().map(norm));
            into.rows.push(row);
        }
        return;
    }
    let items = r.items.as_deref().unwrap_or_default();
    if into.cols.is_empty() {
        let mut keys = BTreeSet::new();
        for it in items {
            if let Some(o) = it.data.as_object() {
                keys.extend(o.keys().cloned());
            }
        }
        into.cols = std::iter::once("_id".to_string()).chain(keys).collect();
    }
    for it in items {
        let row = into
            .cols
            .iter()
            .map(|c| {
                if c == "_id" {
                    it.id.to_string()
                } else {
                    norm(it.data.get(c).unwrap_or(&Value::Null))
                }
            })
            .collect();
        into.rows.push(row);
    }
}

fn column(t: &Table, i: usize) -> Vec<String> {
    t.rows.iter().map(|r| r[i].clone()).collect()
}

/// 正解の各列に候補の列を割り当て（余分な列・列の順序・列名は問わない）、行が一致するか。
/// `ordered` は行の順序まで、`set` は重複を除いた集合で比べる。
fn same_answer(gold: &Table, cand: &Table, ordered: bool, set: bool) -> bool {
    let key = |mut v: Vec<String>| {
        if set {
            v.sort();
            v.dedup();
        } else if !ordered {
            v.sort();
        }
        v
    };
    if !set && gold.rows.len() != cand.rows.len() {
        return false;
    }
    let gcols: Vec<Vec<String>> = (0..gold.cols.len()).map(|j| key(column(gold, j))).collect();
    let ccols: Vec<Vec<String>> = (0..cand.cols.len()).map(|j| key(column(cand, j))).collect();
    let options: Vec<Vec<usize>> = gcols
        .iter()
        .map(|g| (0..ccols.len()).filter(|&c| &ccols[c] == g).collect())
        .collect();
    let project = |t: &Table, idx: &[usize]| -> Vec<Vec<String>> {
        let mut rows: Vec<Vec<String>> = t
            .rows
            .iter()
            .map(|r| idx.iter().map(|&i| r[i].clone()).collect())
            .collect();
        if set {
            rows.sort();
            rows.dedup();
        } else if !ordered {
            rows.sort();
        }
        rows
    };
    let want = project(gold, &(0..gold.cols.len()).collect::<Vec<_>>());
    fn search(
        j: usize,
        options: &[Vec<usize>],
        pick: &mut Vec<usize>,
        ok: &mut dyn FnMut(&[usize]) -> bool,
    ) -> bool {
        if j == options.len() {
            return ok(pick);
        }
        for &c in &options[j] {
            if pick.contains(&c) {
                continue;
            }
            pick.push(c);
            if search(j + 1, options, pick, ok) {
                return true;
            }
            pick.pop();
        }
        false
    }
    let mut ok = |idx: &[usize]| project(cand, idx) == want;
    search(0, &options, &mut Vec::new(), &mut ok)
}

// ---------------------------------------------------------------- 実行

const MAX_PAGES: usize = 40;

/// SQL を実行する（`LIMIT` があれば 1 ページ、無ければ全ページ）。
async fn exec_sql(ev: &Eval, sql: &str) -> Result<Table, DataError> {
    let mut out: Option<Table> = None;
    let mut cursor = None;
    for _ in 0..MAX_PAGES {
        let r = ev
            .store
            .run_sql(
                &ev.who,
                &SqlRequest {
                    sql: sql.into(),
                    cursor: cursor.clone(),
                    with_count: false,
                },
                None,
            )
            .await?;
        let t = from_sql(&r);
        match &mut out {
            Some(o) => o.rows.extend(t.rows),
            None => out = Some(t),
        }
        let single = r.query.limit.is_some() || r.query.group_limit.is_some();
        match r.next_cursor {
            Some(c) if !single => cursor = Some(c),
            _ => break,
        }
    }
    Ok(out.unwrap())
}

/// IR を実行する（`limit` があれば 1 ページ、無ければ全ページ）。
async fn exec_ir(ev: &Eval, table: Uuid, q: &DataQuery) -> Result<Table, DataError> {
    let mut out = Table {
        cols: Vec::new(),
        rows: Vec::new(),
    };
    let mut q = q.clone();
    for _ in 0..MAX_PAGES {
        let r = ev.store.run_query(&ev.who, table, &q, None).await?;
        from_ir(&q, &r, &mut out);
        match r.next_cursor {
            Some(c) if q.limit.is_none() => q.cursor = Some(c),
            _ => break,
        }
    }
    Ok(out)
}

#[tokio::test]
async fn gold_answers_are_valid() {
    let Some(ev) = seed().await else { return };
    let qs = questions();
    assert_eq!(qs.len(), 50);
    let mut per_cat: BTreeMap<String, usize> = BTreeMap::new();
    for q in &qs {
        *per_cat.entry(q.category.clone()).or_default() += 1;
        let gold = exec_sql(&ev, &q.gold)
            .await
            .unwrap_or_else(|e| panic!("{}: 正解の SQL が実行できない: {e:?}", q.id));
        assert!(!gold.rows.is_empty(), "{}: 正解の結果が空", q.id);
        assert!(
            gold.rows.iter().flatten().all(|v| v != "∅"),
            "{}: 正解の結果に値なし（抑制）がある: {:?}",
            q.id,
            gold.rows
        );
        assert!(same_answer(&gold, &gold, q.ordered, q.set), "{}", q.id);
        // 正解の SQL を IR へ変換して IR 直投入で流しても、評価器は正解と判定する（評価器の検証）。
        let c = ev.store.compile_sql(&ev.who, &q.gold).await.unwrap();
        let ir = exec_ir(&ev, c.table_id, &c.query).await.unwrap();
        assert!(
            same_answer(&gold, &ir, q.ordered, q.set),
            "{}: IR の結果が正解と一致しない\n{gold:?}\n{ir:?}",
            q.id
        );
        if std::env::var("SHIKI_SQL_EVAL_SHOW_GOLD").is_ok() {
            eprintln!(
                "{} rows={} {}",
                q.id,
                gold.rows.len(),
                serde_json::to_string(&c.query).unwrap()
            );
        }
    }
    assert!(per_cat.values().all(|&n| n == 10), "{per_cat:?}");
}

// ---------------------------------------------------------------- LLM

/// 失敗の分類（AC の 4 分類＋LLM が答えを返さなかった場合）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
enum Failure {
    /// SQL として読めない／JSON として読めない・IR の形に合わない。
    Syntax,
    /// 部分集合外（SQL の未対応構文・IR の未対応の組み合わせ・資源上限）。
    Unsupported,
    /// 列・テーブル名の誤り。
    Name,
    /// 意味的に不正（実行できたが答えが違う・型や集計の組み合わせの誤り）。
    Semantic,
    /// LLM が答えを返さなかった（通信失敗・空応答）。
    NoAnswer,
}

impl Failure {
    fn label(self) -> &'static str {
        match self {
            Failure::Syntax => "構文エラー",
            Failure::Unsupported => "部分集合外",
            Failure::Name => "列・テーブル名誤り",
            Failure::Semantic => "意味的に不正",
            Failure::NoAnswer => "応答なし",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum Surface {
    Ir,
    Sql,
}

/// 実行できたが答えが違った（LLM には返さない）。
const WRONG_RESULT: &str = "結果が正解と異なる";

/// 1 問 1 面の試行の結果。
#[derive(Debug, Clone, serde::Serialize)]
struct Attempt {
    output: String,
    ok: bool,
    failure: Option<Failure>,
    /// 実行時のエラー（自己修正の入力にも使う。LLM に返す本文と同じもの）。
    error: Option<String>,
}

fn sql_failure(e: &DataError) -> Failure {
    match e {
        DataError::Rejected(r) => match r.code {
            data::RejectCode::SyntaxError => Failure::Syntax,
            data::RejectCode::UnknownName => Failure::Name,
            data::RejectCode::InvalidQuery => Failure::Semantic,
            data::RejectCode::Unsupported
            | data::RejectCode::InvalidJoin
            | data::RejectCode::LimitExceeded => Failure::Unsupported,
        },
        DataError::Forbidden | DataError::NotFound => Failure::Name,
        e if e.is_invalid() => ir_invalid(&e.invalid_message().unwrap_or_default()),
        _ => Failure::Unsupported,
    }
}

fn ir_invalid(m: &str) -> Failure {
    if m.contains("スキーマに存在しません") || m.contains("見つからない") {
        Failure::Name
    } else if [
        "数値で指定",
        "文字列で指定",
        "配列で指定",
        "value が必要",
        "UUID 文字列",
        "number フィールド",
        "数値の列",
    ]
    .iter()
    .any(|k| m.contains(k))
    {
        Failure::Semantic
    } else {
        Failure::Unsupported
    }
}

fn ir_failure(e: &DataError) -> Failure {
    match e {
        DataError::Forbidden | DataError::NotFound => Failure::Name,
        e if e.is_invalid() => ir_invalid(&e.invalid_message().unwrap_or_default()),
        DataError::Rejected(r) if r.code == data::RejectCode::UnknownName => Failure::Name,
        _ => Failure::Unsupported,
    }
}

/// LLM に返すエラー本文（API が返すものと同じ情報）。
fn error_text(e: &DataError) -> String {
    match e {
        DataError::Rejected(r) => serde_json::to_string(r).unwrap(),
        other => other.to_string(),
    }
}

/// 応答からコードフェンス・前置きを剥がす。
fn strip(out: &str, surface: Surface) -> String {
    let mut s = out.trim();
    if let Some(i) = s.find("```") {
        let rest = &s[i + 3..];
        let body = rest.split_once('\n').map_or(rest, |(_, b)| b);
        s = body.split("```").next().unwrap_or(body).trim();
    }
    match surface {
        Surface::Ir => match (s.find('{'), s.rfind('}')) {
            (Some(a), Some(b)) if a < b => s[a..=b].to_string(),
            _ => s.to_string(),
        },
        Surface::Sql => {
            let up = s.to_uppercase();
            let start = up.find("SELECT").unwrap_or(0);
            s[start..].trim().trim_end_matches(';').trim().to_string()
        }
    }
}

async fn judge(ev: &Eval, q: &Question, gold: &Table, surface: Surface, raw: &str) -> Attempt {
    let output = strip(raw, surface);
    let fail = |f: Failure, e: String| Attempt {
        output: output.clone(),
        ok: false,
        failure: Some(f),
        error: Some(e),
    };
    if output.is_empty() {
        return fail(Failure::NoAnswer, "応答が空です".into());
    }
    let res = match surface {
        Surface::Sql => exec_sql(ev, &output)
            .await
            .map_err(|e| (sql_failure(&e), error_text(&e))),
        Surface::Ir => {
            let v: Value = match serde_json::from_str(&output) {
                Ok(v) => v,
                Err(e) => return fail(Failure::Syntax, format!("JSON として読めません: {e}")),
            };
            let Some(name) = v.get("table").and_then(Value::as_str) else {
                return fail(Failure::Syntax, "\"table\" がありません".into());
            };
            let Some(&table) = ev.tables.get(&name.to_lowercase()) else {
                return fail(Failure::Name, format!("テーブル '{name}' は見つかりません"));
            };
            // `query` で包まずに DataQuery のフィールドを最上位へ並べた出力も受ける（包み方は評価器の
            // 都合でツールの引数なら別になるため・ベースラインに有利な側へ倒す）。
            let body = v.get("query").cloned().unwrap_or_else(|| {
                let mut o = v.clone();
                if let Some(m) = o.as_object_mut() {
                    m.remove("table");
                }
                o
            });
            let q: DataQuery = match serde_json::from_value(body) {
                Ok(q) => q,
                Err(e) => return fail(Failure::Syntax, format!("クエリ IR の形が不正です: {e}")),
            };
            exec_ir(ev, table, &q)
                .await
                .map_err(|e| (ir_failure(&e), error_text(&e)))
        }
    };
    match res {
        Ok(t) if same_answer(gold, &t, q.ordered, q.set) => Attempt {
            output,
            ok: true,
            failure: None,
            error: None,
        },
        Ok(t) => Attempt {
            output,
            ok: false,
            failure: Some(Failure::Semantic),
            error: Some(format!(
                "{WRONG_RESULT}（{} 行・列 {:?}）",
                t.rows.len(),
                t.cols
            )),
        },
        Err((f, e)) => fail(f, e),
    }
}

#[derive(Clone)]
struct Llm {
    client: reqwest::Client,
    url: String,
    key: Option<String>,
    header: Option<(String, String)>,
}

impl Llm {
    fn from_env() -> Option<Llm> {
        let url = std::env::var("SHIKI_SQL_EVAL_URL").ok()?;
        let mut b = reqwest::Client::builder().timeout(Duration::from_mins(5));
        if let Ok(p) = std::env::var("SHIKI_SQL_EVAL_CA_BUNDLE") {
            let pem = std::fs::read(p).unwrap();
            for c in reqwest::Certificate::from_pem_bundle(&pem).unwrap() {
                b = b.add_root_certificate(c);
            }
        }
        let header = std::env::var("SHIKI_SQL_EVAL_HEADER").ok().and_then(|h| {
            h.split_once(':')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        });
        Some(Llm {
            client: b.build().unwrap(),
            url,
            key: std::env::var("SHIKI_SQL_EVAL_API_KEY").ok(),
            header,
        })
    }

    /// chat/completions を呼ぶ（429・5xx・通信失敗は待って最大 5 回）。
    async fn chat(&self, model: &str, messages: &[Value]) -> Result<String, String> {
        let body = json!({
            "model": model,
            "messages": messages,
            "temperature": 0,
            "max_tokens": 8192,
        });
        let mut last = String::new();
        for i in 0..5u32 {
            if i > 0 {
                tokio::time::sleep(Duration::from_secs(5 * u64::from(i))).await;
            }
            let mut rq = self.client.post(&self.url).json(&body);
            if let Some(k) = &self.key {
                rq = rq.bearer_auth(k);
            }
            if let Some((k, v)) = &self.header {
                rq = rq.header(k, v);
            }
            let resp = match rq.send().await {
                Ok(r) => r,
                Err(e) => {
                    last = e.to_string();
                    continue;
                }
            };
            let status = resp.status();
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            if status.is_success() {
                if let Some(c) = v["choices"][0]["message"]["content"].as_str() {
                    if !c.trim().is_empty() {
                        return Ok(c.to_string());
                    }
                }
                last = format!("空の応答: {v}");
                continue;
            }
            last = format!("{status}: {v}");
            if !(status.as_u16() == 429 || status.is_server_error()) {
                break;
            }
        }
        Err(last)
    }
}

/// 例を除いた条件（`SHIKI_SQL_EVAL_NO_EXAMPLES=1`・両面から同じように「## 例」の節を外す）。
fn no_examples() -> bool {
    std::env::var("SHIKI_SQL_EVAL_NO_EXAMPLES").is_ok_and(|v| v == "1")
}

fn system_prompt(surface: Surface) -> String {
    let p = match surface {
        Surface::Ir => PROMPT_IR,
        Surface::Sql => PROMPT_SQL,
    };
    let p = if no_examples() {
        p.split("\n## 例").next().unwrap_or(p)
    } else {
        p
    };
    p.replace("{schema}", SCHEMA.trim())
}

/// 全問を並行して生成する（`prev` があれば、そのエラーを返して 1 回だけ書き直させる）。
async fn generate(
    llm: &Llm,
    model: &str,
    surface: Surface,
    qs: &[Question],
    prev: Option<&[Option<Attempt>]>,
) -> Vec<Option<Result<String, String>>> {
    let sem = Arc::new(tokio::sync::Semaphore::new(6));
    let mut tasks = Vec::new();
    for (i, q) in qs.iter().enumerate() {
        let mut messages = vec![
            json!({"role": "system", "content": system_prompt(surface)}),
            json!({"role": "user", "content": format!("質問: {}", q.question)}),
        ];
        if let Some(prev) = prev {
            // 書き直させるのはエラーで失敗したものだけ（「結果が違う」は本番では分からないので返さない）。
            let Some(a) = prev[i].as_ref().filter(|a| {
                !a.ok
                    && a.error
                        .as_deref()
                        .is_some_and(|e| !e.starts_with(WRONG_RESULT))
            }) else {
                tasks.push(None);
                continue;
            };
            messages.push(json!({"role": "assistant", "content": a.output}));
            messages.push(json!({"role": "user", "content": format!(
                "このクエリは次のエラーで失敗しました。修正したものだけを出力してください。\n{}",
                a.error.clone().unwrap_or_default()
            )}));
        }
        let (llm, model, sem) = (llm.clone(), model.to_string(), sem.clone());
        tasks.push(Some(tokio::spawn(async move {
            let _p = sem.acquire().await.unwrap();
            llm.chat(&model, &messages).await
        })));
    }
    let mut out = Vec::new();
    for t in tasks {
        out.push(match t {
            Some(h) => Some(h.await.unwrap()),
            None => None,
        });
    }
    out
}

/// 1 面ぶん（初回＋エラーを返した 1 回の書き直し）を評価する。
async fn eval_surface(
    ev: &Eval,
    llm: &Llm,
    model: &str,
    surface: Surface,
    qs: &[Question],
    golds: &[Table],
) -> (Vec<Attempt>, Vec<Option<Attempt>>) {
    let first = generate(llm, model, surface, qs, None).await;
    let mut attempts = Vec::new();
    for (i, r) in first.into_iter().enumerate() {
        attempts.push(match r.unwrap() {
            Ok(raw) => judge(ev, &qs[i], &golds[i], surface, &raw).await,
            Err(e) => Attempt {
                output: String::new(),
                ok: false,
                failure: Some(Failure::NoAnswer),
                error: Some(e),
            },
        });
    }
    let prev: Vec<Option<Attempt>> = attempts.iter().cloned().map(Some).collect();
    let second = generate(llm, model, surface, qs, Some(&prev)).await;
    let mut repaired = Vec::new();
    for (i, r) in second.into_iter().enumerate() {
        repaired.push(match r {
            None => None,
            Some(Ok(raw)) => Some(judge(ev, &qs[i], &golds[i], surface, &raw).await),
            Some(Err(e)) => Some(Attempt {
                output: String::new(),
                ok: false,
                failure: Some(Failure::NoAnswer),
                error: Some(e),
            }),
        });
    }
    (attempts, repaired)
}

/// 1 面ぶんの結果（面・初回・書き直し）。
type SurfaceResult = (Surface, Vec<Attempt>, Vec<Option<Attempt>>);

fn summarize(model: &str, qs: &[Question], res: &[SurfaceResult]) {
    eprintln!(
        "\n=== {model}{}",
        if no_examples() { "（例なし）" } else { "" }
    );
    for (surface, first, repaired) in res {
        let ok1 = first.iter().filter(|a| a.ok).count();
        let ok2 = first
            .iter()
            .zip(repaired)
            .filter(|(a, r)| a.ok || r.as_ref().is_some_and(|r| r.ok))
            .count();
        let mut cats: BTreeMap<&str, usize> = BTreeMap::new();
        for (q, a) in qs.iter().zip(first) {
            if a.ok {
                *cats.entry(q.category.as_str()).or_default() += 1;
            }
        }
        let mut fails: BTreeMap<&str, usize> = BTreeMap::new();
        for a in first.iter().filter(|a| !a.ok) {
            *fails.entry(a.failure.unwrap().label()).or_default() += 1;
        }
        eprintln!(
            "{surface:?}: 初回 {ok1}/50（{}%）・書き直し 1 回まで {ok2}/50 ・カテゴリ別 {cats:?} ・失敗 {fails:?}",
            ok1 * 2
        );
        for (q, a) in qs.iter().zip(first) {
            if !a.ok {
                eprintln!(
                    "  {} [{}] {} | {}",
                    q.id,
                    a.failure.unwrap().label(),
                    a.output.replace('\n', " "),
                    a.error.clone().unwrap_or_default()
                );
            }
        }
    }
}

#[tokio::test]
async fn llm_generation_success_rate() {
    let Some(llm) = Llm::from_env() else {
        eprintln!("SHIKI_SQL_EVAL_URL 未設定のため LLM 評価をスキップ");
        return;
    };
    let models: Vec<String> = std::env::var("SHIKI_SQL_EVAL_MODELS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        !models.is_empty(),
        "SHIKI_SQL_EVAL_MODELS を指定してください"
    );
    let Some(ev) = seed().await else { return };
    let qs = questions();
    let mut golds = Vec::new();
    for q in &qs {
        golds.push(exec_sql(&ev, &q.gold).await.unwrap());
    }
    let out_dir = std::env::var("SHIKI_SQL_EVAL_OUT").ok();
    for model in &models {
        // ベースライン（IR 直投入）を先に測る。
        let mut res = Vec::new();
        for surface in [Surface::Ir, Surface::Sql] {
            let (first, repaired) = eval_surface(&ev, &llm, model, surface, &qs, &golds).await;
            res.push((surface, first, repaired));
        }
        summarize(model, &qs, &res);
        if let Some(dir) = &out_dir {
            let rows: Vec<Value> = qs
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    let mut o = json!({"id": q.id, "category": q.category, "question": q.question});
                    for (surface, first, repaired) in &res {
                        let k = match surface {
                            Surface::Ir => "ir",
                            Surface::Sql => "sql",
                        };
                        o[k] = json!({"first": first[i], "repaired": repaired[i]});
                    }
                    o
                })
                .collect();
            let variant = if no_examples() { "-noex" } else { "" };
            let path = format!("{dir}/sql-eval-{model}{variant}.json");
            std::fs::write(&path, serde_json::to_string_pretty(&rows).unwrap()).unwrap();
            eprintln!("結果: {path}");
        }
    }
}
