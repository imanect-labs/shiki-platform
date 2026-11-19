//! データ分析課題: 1 年分の売上 CSV（約 2,400 行）を `query_csv` で集計して答える。
//!
//! - `csv-sales`: 1 枚の非正規化された表（地域・店名・分類・商品名を含む）。
//! - `csv-join`: 正規化された 3 枚（売上・店舗・商品）。結合（join）と、結合の外での突き合わせ
//!   （返品の無い店舗・2025 年以降の開店）が要る。
//!
//! 返品は数量・金額が負の行として入っている（合計には含める・「返品行」は数量が負の行）。
//! 生の CSV は `read_file` で読めるが約 10 万字あり、窓には載らない。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use agent_core::Tool;
use serde_json::json;

use super::csvq::{self, Table};
use super::{vfs, FnTool, Grade, Rng, Task};

const REGIONS: &[&str] = &["北海道", "関東", "関西", "九州"];
const STORE_NAMES: &[&str] = &[
    "札幌駅前",
    "旭川中央",
    "函館港",
    "新宿東口",
    "横浜西口",
    "大宮",
    "梅田",
    "三宮",
    "京都四条",
    "天神",
    "熊本下通",
    "小倉",
];
const CATEGORIES: &[&str] = &["家電", "文具", "食品", "衣料", "日用品"];
const PRODUCTS: &[&str] = &[
    "電気ケトル",
    "ドライヤー",
    "モバイル電池",
    "イヤホン",
    "万年筆",
    "ノート",
    "付箋",
    "ボールペン",
    "緑茶",
    "レトルトカレー",
    "チョコレート",
    "コーヒー豆",
    "Tシャツ",
    "靴下",
    "手袋",
    "パーカー",
    "洗剤",
    "歯ブラシ",
    "タオル",
    "ハンドソープ",
];

struct Row {
    order_id: String,
    date: String,
    month: String,
    store: usize,
    product: usize,
    qty: i64,
    amount: i64,
    ec: bool,
}

pub(crate) struct Sales {
    joined: bool,
    rows: Vec<Row>,
    /// 店舗の開店日。
    opened: Vec<String>,
    prices: Vec<i64>,
    tables: csvq::Tables,
    files: vfs::Files,
    answer: Arc<Mutex<Option<serde_json::Value>>>,
}

fn region_of(store: usize) -> &'static str {
    REGIONS[store / 3]
}

fn category_of(product: usize) -> &'static str {
    CATEGORIES[product / 4]
}

impl Sales {
    pub(crate) fn new(seed: u64, joined: bool) -> Self {
        let mut rng = Rng::new(seed ^ 0x5a1e);
        let prices: Vec<i64> = (0..PRODUCTS.len())
            .map(|_| rng.range(3, 120) as i64 * 50)
            .collect();
        let opened: Vec<String> = (0..STORE_NAMES.len())
            .map(|i| {
                // 3 店は 2025 年以降の開店。
                let y = if i % 4 == 1 {
                    2025
                } else {
                    rng.range(2012, 2024)
                };
                format!("{y}-{:02}-{:02}", rng.range(1, 12), rng.range(1, 28))
            })
            .collect();
        // 返品の無い店舗（2〜3 店）。
        let no_returns: Vec<usize> = vec![
            rng.range(0, 3) as usize,
            6 + rng.range(0, 2) as usize,
            9 + rng.range(0, 2) as usize,
        ];
        let mut rows = Vec::new();
        for i in 0..2_400 {
            let m = rng.range(0, 11) as usize;
            let (y, mm) = if m < 9 { (2025, m + 4) } else { (2026, m - 8) };
            let d = rng.range(1, 28);
            let store = rng.range(0, STORE_NAMES.len() as u64 - 1) as usize;
            let product = rng.range(0, PRODUCTS.len() as u64 - 1) as usize;
            let ret = !no_returns.contains(&store) && rng.range(0, 99) < 4;
            let qty = if ret {
                -(rng.range(1, 3) as i64)
            } else {
                rng.range(1, 12) as i64
            };
            rows.push(Row {
                order_id: format!("O{:05}", i + 1),
                date: format!("{y}-{mm:02}-{d:02}"),
                month: format!("{y}-{mm:02}"),
                store,
                product,
                qty,
                amount: qty * prices[product],
                ec: rng.range(0, 99) < 35,
            });
        }
        let mut tables = BTreeMap::new();
        if joined {
            let mut s = Table::new(&[
                "order_id",
                "date",
                "month",
                "store_id",
                "product_id",
                "qty",
                "amount",
                "channel",
            ]);
            for r in &rows {
                s.rows.push(vec![
                    r.order_id.clone(),
                    r.date.clone(),
                    r.month.clone(),
                    format!("S{:02}", r.store + 1),
                    format!("P{:02}", r.product + 1),
                    r.qty.to_string(),
                    r.amount.to_string(),
                    if r.ec { "EC" } else { "店舗" }.into(),
                ]);
            }
            let mut st = Table::new(&["store_id", "store_name", "region", "opened"]);
            for (i, n) in STORE_NAMES.iter().enumerate() {
                st.rows.push(vec![
                    format!("S{:02}", i + 1),
                    (*n).into(),
                    region_of(i).into(),
                    opened[i].clone(),
                ]);
            }
            let mut p = Table::new(&["product_id", "product_name", "category", "price"]);
            for (i, n) in PRODUCTS.iter().enumerate() {
                p.rows.push(vec![
                    format!("P{:02}", i + 1),
                    (*n).into(),
                    category_of(i).into(),
                    prices[i].to_string(),
                ]);
            }
            tables.insert("sales.csv".to_string(), s);
            tables.insert("stores.csv".to_string(), st);
            tables.insert("products.csv".to_string(), p);
        } else {
            let mut s = Table::new(&[
                "order_id",
                "date",
                "month",
                "region",
                "store_name",
                "category",
                "product_name",
                "qty",
                "unit_price",
                "amount",
                "channel",
            ]);
            for r in &rows {
                s.rows.push(vec![
                    r.order_id.clone(),
                    r.date.clone(),
                    r.month.clone(),
                    region_of(r.store).into(),
                    STORE_NAMES[r.store].into(),
                    category_of(r.product).into(),
                    PRODUCTS[r.product].into(),
                    r.qty.to_string(),
                    prices[r.product].to_string(),
                    r.amount.to_string(),
                    if r.ec { "EC" } else { "店舗" }.into(),
                ]);
            }
            tables.insert("sales.csv".to_string(), s);
        }
        let files = vfs::files(
            tables
                .iter()
                .map(|(k, t)| (k.clone(), t.to_csv()))
                .collect(),
        );
        Sales {
            joined,
            rows,
            opened,
            prices,
            tables: Arc::new(tables),
            files,
            answer: Arc::new(Mutex::new(None)),
        }
    }

    fn expected(&self) -> serde_json::Value {
        let sum_by = |key: &dyn Fn(&Row) -> Option<String>| {
            let mut m: BTreeMap<String, i64> = BTreeMap::new();
            for r in &self.rows {
                if let Some(k) = key(r) {
                    *m.entry(k).or_default() += r.amount;
                }
            }
            m
        };
        if self.joined {
            let category = sum_by(&|r| Some(category_of(r.product).to_string()));
            let region_ec = sum_by(&|r| r.ec.then(|| region_of(r.store).to_string()));
            let with_returns: std::collections::BTreeSet<usize> = self
                .rows
                .iter()
                .filter(|r| r.qty < 0)
                .map(|r| r.store)
                .collect();
            let no_returns: Vec<&str> = (0..STORE_NAMES.len())
                .filter(|s| !with_returns.contains(s))
                .map(|s| STORE_NAMES[s])
                .collect();
            let mut kansai: BTreeMap<usize, i64> = BTreeMap::new();
            for r in self.rows.iter().filter(|r| region_of(r.store) == "関西") {
                *kansai.entry(r.product).or_default() += r.qty;
            }
            let top = kansai
                .iter()
                .max_by_key(|(_, q)| **q)
                .map(|(p, _)| PRODUCTS[*p]);
            let new_amount: i64 = self
                .rows
                .iter()
                .filter(|r| self.opened[r.store].as_str() >= "2025-01-01")
                .map(|r| r.amount)
                .sum();
            json!({"category_amount": category, "region_ec_amount": region_ec,
                   "stores_without_returns": no_returns, "kansai_top_product": top,
                   "new_store_amount": new_amount})
        } else {
            let q3 = sum_by(&|r| {
                (r.date.as_str() >= "2025-07-01" && r.date.as_str() <= "2025-09-30")
                    .then(|| region_of(r.store).to_string())
            });
            let mut qty: BTreeMap<usize, i64> = BTreeMap::new();
            for r in &self.rows {
                *qty.entry(r.product).or_default() += r.qty;
            }
            let mut by_qty: Vec<(usize, i64)> = qty.into_iter().collect();
            by_qty.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let top3: Vec<&str> = by_qty.iter().take(3).map(|(p, _)| PRODUCTS[*p]).collect();
            let mut returns: BTreeMap<String, i64> = BTreeMap::new();
            for r in self.rows.iter().filter(|r| r.qty < 0) {
                *returns
                    .entry(category_of(r.product).to_string())
                    .or_default() += 1;
            }
            let ec_month = sum_by(&|r| r.ec.then(|| r.month.clone()));
            let best = ec_month
                .iter()
                .max_by_key(|(_, v)| **v)
                .map(|(k, _)| k.clone());
            let mut avg: BTreeMap<usize, (i64, i64)> = BTreeMap::new();
            for r in self.rows.iter().filter(|r| r.qty > 0) {
                let e = avg.entry(r.store).or_default();
                e.0 += r.qty;
                e.1 += 1;
            }
            let best_avg = avg
                .iter()
                .max_by(|a, b| {
                    (a.1 .0 as f64 / a.1 .1 as f64)
                        .partial_cmp(&(b.1 .0 as f64 / b.1 .1 as f64))
                        .unwrap()
                })
                .map(|(s, _)| STORE_NAMES[*s]);
            json!({"q3_region_amount": q3, "top3_products_by_qty": top3,
                   "return_rows_by_category": returns, "best_ec_month": best,
                   "highest_avg_qty_store": best_avg})
        }
    }
}

fn int_of(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f.round() as i64))
        .or_else(|| {
            v.as_str()
                .and_then(|s| s.replace([',', '円'], "").trim().parse().ok())
        })
}

fn map_matches(got: &serde_json::Value, want: &serde_json::Value) -> bool {
    let Some(w) = want.as_object() else {
        return false;
    };
    w.iter().all(|(k, v)| int_of(&got[k]) == int_of(v))
}

fn names(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.replace("店", "").trim().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

impl Task for Sales {
    fn system(&self) -> String {
        "あなたはデータ分析の担当者です。CSV はツールで問い合わせて集計し、推測で数値を書かないでください。".into()
    }

    fn prompt(&self) -> String {
        if self.joined {
            "sales.csv（売上の明細・約 2,400 行）、stores.csv（店舗）、products.csv（商品）を分析して、\
             次の 5 つに答えてください。返品は qty と amount が負の行で、合計にはそのまま含めます。\n\
             1. category_amount: 商品分類（products.category）ごとの amount の合計（全期間）\n\
             2. region_ec_amount: 地域（stores.region）ごとの、channel が EC の amount の合計\n\
             3. stores_without_returns: 返品行（qty が負の行）が 1 件も無い店舗の store_name の一覧\n\
             4. kansai_top_product: 関西の店舗での qty の合計（返品を差し引いた純数量）が最大の product_name\n\
             5. new_store_amount: 2025-01-01 以降に開店した（stores.opened が 2025-01-01 以上の）店舗の amount の合計\n\
             最後に submit_answer で、answer に次の形の JSON を渡してください:\n\
             {\"category_amount\": {\"家電\": 0, ...}, \"region_ec_amount\": {\"北海道\": 0, ...}, \
             \"stores_without_returns\": [\"…\"], \"kansai_top_product\": \"…\", \"new_store_amount\": 0}"
                .into()
        } else {
            "sales.csv（1 年分の売上の明細・約 2,400 行）を分析して、次の 5 つに答えてください。\
             返品は qty と amount が負の行で、合計にはそのまま含めます。\n\
             1. q3_region_amount: 2025-07-01〜2025-09-30（両端を含む）の、地域（region）ごとの amount の合計\n\
             2. top3_products_by_qty: qty の合計（返品を差し引いた純数量）が多い商品（product_name）の上位 3 つ（多い順）\n\
             3. return_rows_by_category: 返品行（qty が負の行）の件数を分類（category）ごとに\n\
             4. best_ec_month: channel が EC の amount の合計が最大の月（YYYY-MM）\n\
             5. highest_avg_qty_store: 返品行を除いた 1 行あたりの平均 qty が最も大きい店舗（store_name）\n\
             最後に submit_answer で、answer に次の形の JSON を渡してください:\n\
             {\"q3_region_amount\": {\"北海道\": 0, ...}, \"top3_products_by_qty\": [\"…\", \"…\", \"…\"], \
             \"return_rows_by_category\": {\"家電\": 0, ...}, \"best_ec_month\": \"YYYY-MM\", \
             \"highest_avg_qty_store\": \"…\"}"
                .into()
        }
    }

    fn max_steps(&self) -> usize {
        40
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut t = csvq::tools(&self.tables);
        t.extend(vfs::tools(&self.files, false));
        let a = self.answer.clone();
        t.push(FnTool::new(
            "submit_answer",
            "最終回答を提出する（answer は指定された形の JSON オブジェクト）。",
            json!({"type": "object", "properties": {"answer": {"type": "object"}}, "required": ["answer"]}),
            false,
            move |v| {
                let ans = v.get("answer").cloned().unwrap_or_default();
                if !ans.is_object() {
                    return Err("answer must be a JSON object".into());
                }
                *a.lock().unwrap() = Some(ans);
                Ok("提出を受け付けました。".into())
            },
        ));
        t
    }

    fn grade(&self) -> Grade {
        let want = self.expected();
        let _ = &self.prices;
        let Some(a) = self.answer.lock().unwrap().clone() else {
            return Grade {
                score: 0.0,
                success: false,
                details: json!({"error": "no answer", "expected": want}),
            };
        };
        let mut parts = Vec::new();
        if self.joined {
            parts.push(map_matches(&a["category_amount"], &want["category_amount"]));
            parts.push(map_matches(
                &a["region_ec_amount"],
                &want["region_ec_amount"],
            ));
            let mut got = names(&a["stores_without_returns"]);
            let mut exp = names(&want["stores_without_returns"]);
            got.sort();
            exp.sort();
            parts.push(got == exp);
            parts.push(a["kansai_top_product"] == want["kansai_top_product"]);
            parts.push(int_of(&a["new_store_amount"]) == int_of(&want["new_store_amount"]));
        } else {
            parts.push(map_matches(
                &a["q3_region_amount"],
                &want["q3_region_amount"],
            ));
            parts.push(names(&a["top3_products_by_qty"]) == names(&want["top3_products_by_qty"]));
            parts.push(map_matches(
                &a["return_rows_by_category"],
                &want["return_rows_by_category"],
            ));
            parts.push(a["best_ec_month"] == want["best_ec_month"]);
            parts.push(
                a["highest_avg_qty_store"]
                    .as_str()
                    .map(|s| s.replace('店', ""))
                    == want["highest_avg_qty_store"]
                        .as_str()
                        .map(|s| s.replace('店', "")),
            );
        }
        let score = parts.iter().filter(|p| **p).count() as f64 / parts.len() as f64;
        Grade {
            score,
            success: score > 0.999,
            details: json!({"answer": a, "expected": want, "parts": parts}),
        }
    }
}
