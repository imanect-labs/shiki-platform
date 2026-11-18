//! 集計課題: 12 か月分の長い月次報告を読み、地域別の売上合計などを答える。
//!
//! 報告 1 本が日本語 3,000 字前後（数値表は本文の中ほど）で、全部を読むと 32k 窓を超える。
//! 古い報告を畳む（剪定）だけだと、モデルが数値を本文に書き留めていない限り失われる。
//! 途中の報告に「前月の訂正」を 1 件入れ、後から来た情報を反映できるかも見る。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use agent_core::Tool;
use serde_json::json;

use super::{arg_str, FnTool, Grade, Rng, Task};

const REGIONS: &[(&str, [&str; 2])] = &[
    ("北海道", ["札幌", "旭川"]),
    ("関東", ["東京", "横浜"]),
    ("関西", ["大阪", "神戸"]),
    ("九州", ["福岡", "熊本"]),
];

const MONTHS: &[&str] = &[
    "2025-04", "2025-05", "2025-06", "2025-07", "2025-08", "2025-09", "2025-10", "2025-11",
    "2025-12", "2026-01", "2026-02", "2026-03",
];

const TOPICS: &[&str] = &[
    "新規顧客向けの提案資料の改訂",
    "在庫回転率の改善施策",
    "店舗レイアウトの見直し",
    "季節商品の仕入れ計画",
    "人員配置とシフトの最適化",
    "競合店の出店動向",
    "会員アプリの利用促進",
    "物流拠点との配送時間の調整",
    "法人顧客の契約更新",
    "接客研修の実施状況",
    "販促キャンペーンの効果測定",
    "電力使用量の削減",
];

const DETAILS: &[&str] = &[
    "担当者間で論点を整理し、次回までに具体案を持ち寄ることになった",
    "現場からは運用負荷が高いという声があり、手順の簡素化を検討する",
    "前年同期と比べて大きな変化はなく、引き続き経過を観察する",
    "本部の方針と整合させるため、来月の定例で再度確認する",
    "顧客アンケートの自由記述を分類し、改善点を三つに絞り込んだ",
    "取引先との調整に時間がかかっており、スケジュールを一週間延ばした",
    "効果は限定的だったが、一部の時間帯では明確な改善が見られた",
    "費用対効果の試算を経理と共有し、承認手続きに入った",
];

pub(crate) struct Ledger {
    reports: Vec<(String, String)>,
    /// 正解（訂正反映後）。
    revenue: BTreeMap<(String, String), u64>,
    outage_months: usize,
    answer: Arc<Mutex<Option<serde_json::Value>>>,
}

impl Ledger {
    pub(crate) fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let mut revenue = BTreeMap::new();
        let mut reported = BTreeMap::new();
        for m in MONTHS {
            for (_, branches) in REGIONS {
                for b in branches {
                    let v = rng.range(800, 2600);
                    revenue.insert(((*b).to_string(), (*m).to_string()), v);
                    reported.insert(((*b).to_string(), (*m).to_string()), v);
                }
            }
        }
        // 2025-06 の神戸を報告時は誤り、2025-09 の報告で訂正する。
        let key = ("神戸".to_string(), "2025-06".to_string());
        let correct = revenue[&key];
        let wrong = correct + 700;
        reported.insert(key.clone(), wrong);
        // 実際の障害がある月（訓練だけの月は数えない）。
        let outages: Vec<usize> = vec![1, 4, 9];
        let drills: Vec<usize> = vec![3, 7];
        let mut reports = Vec::new();
        for (mi, m) in MONTHS.iter().enumerate() {
            let mut text = format!("# 月次営業報告 {m}\n\n## 概況\n\n");
            for _ in 0..6 {
                let (_, branches) = rng.pick(REGIONS);
                let b = rng.pick(branches);
                text.push_str(&format!(
                    "{b}支店では{}について打ち合わせを行い、{}。",
                    rng.pick(TOPICS),
                    rng.pick(DETAILS)
                ));
            }
            text.push_str("\n\n## 各支店の取り組み\n\n");
            for (_, branches) in REGIONS {
                for b in branches {
                    text.push_str(&format!("### {b}支店\n"));
                    for _ in 0..2 {
                        text.push_str(&format!(
                            "{}については、{}。また、{}に関して{}。",
                            rng.pick(TOPICS),
                            rng.pick(DETAILS),
                            rng.pick(TOPICS),
                            rng.pick(DETAILS)
                        ));
                    }
                    text.push('\n');
                }
            }
            text.push_str(
                "\n## 売上実績（単位: 万円）\n\n| 地域 | 支店 | 売上 |\n| --- | --- | --- |\n",
            );
            for (region, branches) in REGIONS {
                for b in branches {
                    let v = reported[&((*b).to_string(), (*m).to_string())];
                    text.push_str(&format!("| {region} | {b} | {v} |\n"));
                }
            }
            if mi == 5 {
                text.push_str(&format!(
                    "\n※訂正: 2025-06 の報告で神戸支店の売上を {wrong} 万円としていましたが、\
                     集計誤りでした。正しくは {correct} 万円です。集計の際はこちらを用いてください。\n"
                ));
            }
            text.push_str("\n## 設備・システム\n\n");
            if outages.contains(&mi) {
                let b = rng.pick(&REGIONS[mi % 4].1);
                text.push_str(&format!(
                    "{b}支店で POS システムの障害が発生し、約 3 時間レジが利用できなかった。\
                     復旧後に原因を調査中。\n"
                ));
            } else if drills.contains(&mi) {
                text.push_str("全支店で障害対応訓練を実施した（実際の障害は発生していない）。\n");
            } else {
                text.push_str("特記事項なし。\n");
            }
            text.push_str("\n## その他\n\n");
            for _ in 0..8 {
                let (_, branches) = rng.pick(REGIONS);
                let b = rng.pick(branches);
                text.push_str(&format!(
                    "{b}支店の{}は、{}。",
                    rng.pick(TOPICS),
                    rng.pick(DETAILS)
                ));
            }
            text.push('\n');
            reports.push(((*m).to_string(), text));
        }
        Ledger {
            reports,
            revenue,
            outage_months: outages.len(),
            answer: Arc::new(Mutex::new(None)),
        }
    }

    fn region_totals(&self) -> BTreeMap<String, u64> {
        let mut t = BTreeMap::new();
        for (region, branches) in REGIONS {
            let sum: u64 = self
                .revenue
                .iter()
                .filter(|((b, _), _)| branches.contains(&b.as_str()))
                .map(|(_, v)| *v)
                .sum();
            t.insert((*region).to_string(), sum);
        }
        t
    }

    fn peak(&self) -> (String, String) {
        let ((b, m), _) = self
            .revenue
            .iter()
            .max_by_key(|(_, v)| **v)
            .expect("nonempty");
        (b.clone(), m.clone())
    }
}

impl Task for Ledger {
    fn system(&self) -> String {
        "あなたは小売チェーンの経営企画担当のアシスタントです。".into()
    }

    fn prompt(&self) -> String {
        "2025 年度（2025-04〜2026-03）の月次営業報告がすべて揃っています。報告を全部読んで、\
         次を求め、submit_answer で提出してください。\n\
         1. 地域ごとの年間売上合計（地域に属する 2 支店 × 12 か月の合計・万円）\n\
         2. 年間売上合計が最大の地域\n\
         3. 単月の売上が最も大きかった支店と月\n\
         4. 実際にシステム障害が発生した月の数（訓練は数えない）\n\
         報告の中に訂正があれば、訂正後の値で集計してください。"
            .into()
    }

    fn max_steps(&self) -> usize {
        30
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let ids: Vec<String> = self.reports.iter().map(|(m, _)| m.clone()).collect();
        let reports = self.reports.clone();
        let answer = Arc::clone(&self.answer);
        vec![
            FnTool::new(
                "list_reports",
                "月次営業報告の一覧（報告 ID）を返す。",
                json!({"type": "object", "properties": {}}),
                true,
                move |_| Ok(format!("報告 ID: {}", ids.join(", "))),
            ),
            FnTool::new(
                "read_report",
                "月次営業報告 1 本の全文を返す。id は list_reports の報告 ID（例: 2025-04）。",
                json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
                true,
                move |v| {
                    let id = arg_str(v, "id")?;
                    reports
                        .iter()
                        .find(|(m, _)| m == id)
                        .map(|(_, t)| t.clone())
                        .ok_or_else(|| format!("報告 {id} は見つかりません"))
                },
            ),
            FnTool::new(
                "calculate",
                "四則演算の式（+ - * / と括弧）を計算して結果を返す。例: 1801+1546+1417",
                json!({"type": "object", "properties": {"expression": {"type": "string"}}, "required": ["expression"]}),
                true,
                |v| {
                    let e = arg_str(v, "expression")?;
                    super::calculate(e)
                        .map(|x| format!("{e} = {x}"))
                        .map_err(|m| format!("式を読めません: {m}"))
                },
            ),
            FnTool::new(
                "submit_answer",
                "最終回答を提出する（1 回だけ）。region_totals は地域名→合計（整数・万円）。",
                json!({
                    "type": "object",
                    "properties": {
                        "region_totals": {"type": "object", "additionalProperties": {"type": "integer"}},
                        "top_region": {"type": "string"},
                        "peak_branch": {"type": "string", "description": "支店名（例: 札幌）"},
                        "peak_month": {"type": "string", "description": "YYYY-MM"},
                        "outage_months": {"type": "integer"}
                    },
                    "required": ["region_totals", "top_region", "peak_branch", "peak_month", "outage_months"]
                }),
                false,
                move |v| {
                    let mut a = answer.lock().unwrap();
                    if a.is_some() {
                        return Err("回答は提出済みです".into());
                    }
                    if !v
                        .get("region_totals")
                        .is_some_and(serde_json::Value::is_object)
                    {
                        return Err("invalid tool input: `region_totals` must be an object".into());
                    }
                    *a = Some(v.clone());
                    Ok("回答を受け付けました。".into())
                },
            ),
        ]
    }

    fn grade(&self) -> Grade {
        let totals = self.region_totals();
        let (pb, pm) = self.peak();
        let expected = json!({
            "region_totals": totals, "top_region": totals.iter().max_by_key(|(_, v)| **v).map(|(k, _)| k.clone()),
            "peak_branch": pb, "peak_month": pm, "outage_months": self.outage_months,
        });
        let Some(a) = self.answer.lock().unwrap().clone() else {
            return Grade {
                score: 0.0,
                success: false,
                details: json!({"error": "no answer", "expected": expected}),
            };
        };
        let mut score = 0.0;
        for (region, total) in &totals {
            let got = a["region_totals"][region].as_u64().or_else(|| {
                a["region_totals"][region]
                    .as_str()
                    .and_then(|s| s.replace(',', "").parse().ok())
            });
            if got == Some(*total) {
                score += 0.15;
            }
        }
        if a["top_region"].as_str() == expected["top_region"].as_str() {
            score += 0.1;
        }
        let branch = a["peak_branch"].as_str().unwrap_or("").replace("支店", "");
        if branch == pb && a["peak_month"].as_str() == Some(pm.as_str()) {
            score += 0.15;
        }
        if a["outage_months"].as_u64() == Some(self.outage_months as u64) {
            score += 0.15;
        }
        Grade {
            score,
            success: score > 0.999,
            details: json!({"answer": a, "expected": expected}),
        }
    }
}
