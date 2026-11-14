//! 結果の書き出し: `results/<日付>-<段>.json`・`results/summary.md`（全結果から作り直す）・
//! `adjudication/<データ名>.jsonl`（E1 の後・human の裁定の材料）。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::data::{eval_dir, hex, Dataset, Frozen};
use crate::metrics::{primary_key, Acceptance, Errors, Ratio, RepMetrics};
use crate::run::CallResult;

/// データ 1 つの結果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DatasetResult {
    pub(crate) name: String,
    pub(crate) questions_sha256: String,
    pub(crate) rows: usize,
    pub(crate) reps: Vec<RepMetrics>,
    /// 主な質問の反復の一致率（反復が 2 回以上のとき）。
    pub(crate) agreement: Option<Ratio>,
    /// dev の段で選んだしきい値の案。
    pub(crate) suggested_theta: Option<f64>,
    /// test の段の合格基準（凍結したしきい値で・裁定の前は暫定）。
    pub(crate) acceptance: Option<Acceptance>,
}

/// 1 回の起動の結果（`results/<日付>-<段>.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ResultFile {
    pub(crate) label: String,
    pub(crate) stage: String,
    pub(crate) date: String,
    pub(crate) started_at: String,
    pub(crate) finished_at: String,
    pub(crate) model: String,
    /// 最初に答えた版（後方互換。全部の版は [`Self::resolved_models`]）。
    pub(crate) resolved_model: Option<String>,
    /// 答えた版の一覧（重複を除く・凍結した段は全部が凍結の版でなければ合否を出さない）。
    #[serde(default)]
    pub(crate) resolved_models: Vec<String>,
    pub(crate) split: String,
    pub(crate) reps: u32,
    pub(crate) effort: String,
    pub(crate) samples: u8,
    pub(crate) concurrency: usize,
    pub(crate) budget_usd: f64,
    pub(crate) estimate_usd: f64,
    pub(crate) relay_before_usd: f64,
    pub(crate) relay_after_usd: f64,
    /// 中継の累計額の差分（この実行の実費。ほかの呼び出しが同時に走っていれば混ざる）。
    pub(crate) relay_delta_usd: f64,
    /// 会計（llm_usage）に載ったこの実行の分（失敗の `#f` の行を含む）。
    pub(crate) db_rows: i64,
    pub(crate) db_cost_usd: f64,
    pub(crate) calls: usize,
    pub(crate) cost_per_1000_usd: Option<f64>,
    pub(crate) latency_p50_ms: Option<u64>,
    pub(crate) latency_p95_ms: Option<u64>,
    pub(crate) stopped: Option<String>,
    pub(crate) frozen: Option<Frozen>,
    /// 採点に使ったデータ（`*.jsonl`）の sha256（データ名 → 値。この欄より前の結果には無い）。
    #[serde(default)]
    pub(crate) datasets_sha256: BTreeMap<String, String>,
    /// 保存した答えを今のデータで採点し直した結果なら、元の結果の名前（実 LLM は呼んでいない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rescored_from: Option<String>,
    pub(crate) datasets: Vec<DatasetResult>,
    pub(crate) results: Vec<CallResult>,
}

/// 結果（`results/<label>.json`）を読む。
pub(crate) fn read_result(label: &str) -> ResultFile {
    let path = eval_dir().join("results").join(format!("{label}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub(crate) fn write_result(file: &ResultFile) -> std::path::PathBuf {
    let dir = eval_dir().join("results");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    let path = dir.join(format!("{}.json", file.label));
    let text = serde_json::to_string_pretty(file).unwrap_or_else(|e| panic!("結果を書けない: {e}"));
    std::fs::write(&path, text + "\n").unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    path
}

fn pct(r: &Ratio) -> String {
    match r.p {
        Some(p) => format!(
            "{:.1}%（{:.1}〜{:.1}）{}/{}",
            p * 100.0,
            r.lo * 100.0,
            r.hi * 100.0,
            r.k,
            r.n
        ),
        None => "—".into(),
    }
}

fn errors(e: &Errors) -> String {
    format!(
        "拒否 {}・途切れ（max_tokens）{}・不正出力 {}（形 {}・要求と不一致 {}）・その他 {}",
        e.refused,
        e.truncated,
        e.invalid(),
        e.invalid_output,
        e.invalid_response,
        e.other
    )
}

fn opt_f(v: Option<f64>, digits: usize) -> String {
    v.map_or_else(|| "—".into(), |x| format!("{x:.digits$}"))
}

/// 1 回の反復の表（データの種類ごと）。
fn render_rep(out: &mut String, rep: usize, m: &RepMetrics) {
    match m {
        RepMetrics::Inquiry(m) => {
            let _ = writeln!(
                out,
                "- 反復 {rep}: n={}・category の正解率 {}（紛らわしいもの {}・普通 {}）・誤り {}",
                m.n,
                pct(&m.accuracy),
                pct(&m.ambiguous_accuracy),
                pct(&m.plain_accuracy),
                errors(&m.errors)
            );
            let _ = writeln!(
                out,
                "  - 優先度の完全一致 {}・±1 段 {}・怒りの再現率 {}・適合率 {}・ECE {}",
                pct(&m.priority_exact),
                pct(&m.priority_within_one),
                pct(&m.angry_recall),
                pct(&m.angry_precision),
                opt_f(m.calibration.ece, 3)
            );
            out.push_str("\n  | θ | 自動処理率 | 自動処理分の精度 | 注入に従った |\n  | --- | --- | --- | --- |\n");
            for t in &m.theta {
                let _ = writeln!(
                    out,
                    "  | {:.1} | {} | {} | {} |",
                    t.theta,
                    pct(&t.auto_rate),
                    pct(&t.auto_precision),
                    t.injection_followed
                );
            }
            out.push_str("\n  | 区分 | 再現率 | 適合率 |\n  | --- | --- | --- |\n");
            for (c, s) in &m.per_class {
                let _ = writeln!(
                    out,
                    "  | {c} | {} | {} |",
                    pct(&s.recall),
                    pct(&s.precision)
                );
            }
        }
        RepMetrics::Expense(m) => {
            let _ = writeln!(
                out,
                "- 反復 {rep}: n={}（中身で判断する行 {}）・誤り {}",
                m.n,
                m.judgment_rows,
                errors(&m.errors)
            );
            let _ = writeln!(
                out,
                "  - AI の needs_review（中身で判断する行）: 正解率 {}・再現率 {}・適合率 {}・ECE {}",
                pct(&m.ai_accuracy),
                pct(&m.ai_recall),
                pct(&m.ai_precision),
                opt_f(m.calibration.ece, 3)
            );
            let _ = writeln!(
                out,
                "  - 目的の不一致の再現率 {}・適合率 {}／私的な利用の再現率 {}・適合率 {}",
                pct(&m.purpose_recall),
                pct(&m.purpose_precision),
                pct(&m.private_recall),
                pct(&m.private_precision)
            );
            out.push_str("\n  | θ | uncertain | 経理確認の再現率 | 適合率 | 自動で決めた分の正解率 | 注入の取りこぼし |\n  | --- | --- | --- | --- | --- | --- |\n");
            for t in &m.theta {
                let _ = writeln!(
                    out,
                    "  | {:.1} | {} | {} | {} | {} | {} |",
                    t.theta,
                    pct(&t.uncertain_rate),
                    pct(&t.routed_recall),
                    pct(&t.routed_precision),
                    pct(&t.auto_precision),
                    t.injection_escaped
                );
            }
        }
    }
    let cal = match m {
        RepMetrics::Inquiry(m) => &m.calibration,
        RepMetrics::Expense(m) => &m.calibration,
    };
    if !cal.bins.is_empty() {
        out.push_str("\n  | 信頼度の区間（等頻度・同じ値は分けない） | 件数 | 平均 | 正解率 |\n  | --- | --- | --- | --- |\n");
        for b in &cal.bins {
            let _ = writeln!(
                out,
                "  | {:.2}〜{:.2} | {} | {:.3} | {} |",
                b.conf_min,
                b.conf_max,
                b.accuracy.n,
                b.conf_mean,
                pct(&b.accuracy)
            );
        }
    }
    out.push('\n');
}

fn render(file: &ResultFile) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## {}\n", file.label);
    let _ = writeln!(
        out,
        "- 段 {}・{}（{} 回反復）・判定用のモデル {}（{}）・effort {}・k={}",
        file.stage,
        file.split,
        file.reps,
        file.model,
        file.resolved_model.as_deref().unwrap_or("—"),
        file.effort,
        file.samples
    );
    let _ = writeln!(
        out,
        "- 呼び出し {} 回・中継の差分 ${:.4}・会計 ${:.4}（{} 行）・1,000 件あたり ${}・p50 {} ms・p95 {} ms",
        file.calls,
        file.relay_delta_usd,
        file.db_cost_usd,
        file.db_rows,
        opt_f(file.cost_per_1000_usd, 3),
        file.latency_p50_ms.map_or("—".into(), |v| v.to_string()),
        file.latency_p95_ms.map_or("—".into(), |v| v.to_string()),
    );
    if let Some(s) = &file.stopped {
        let _ = writeln!(out, "- **途中で止めた**: {s}");
    }
    if file.resolved_models.len() > 1 {
        let _ = writeln!(out, "- 答えた版: {}", file.resolved_models.join("・"));
    }
    if let Some(from) = &file.rescored_from {
        let sha: Vec<String> = file
            .datasets_sha256
            .iter()
            .map(|(k, v)| format!("{k} {}", &v[..v.len().min(12)]))
            .collect();
        let _ = writeln!(
            out,
            "- **採点し直し**: {from} の答えを、今のデータ（{}）と今の指標の計算で採点し直した（実 LLM は呼んでいない）",
            sha.join("・")
        );
    }
    if let Some(f) = &file.frozen {
        let thetas: Vec<String> = f.theta.iter().map(|(k, v)| format!("{k} {v:.1}")).collect();
        let _ = writeln!(
            out,
            "- 凍結: プロンプト {}・θ（{}）・材料 {}",
            f.prompt_version,
            thetas.join("・"),
            f.based_on
        );
    }
    for ds in &file.datasets {
        let _ = writeln!(out, "\n### {}\n", ds.name);
        if let Some(t) = ds.suggested_theta {
            let _ = writeln!(out, "- θ の案（dev）: {t:.1}");
        }
        if let Some(a) = &ds.agreement {
            let _ = writeln!(
                out,
                "- 反復の一致率（{}）: {}",
                primary_key(&ds.name),
                pct(a)
            );
        }
        for (i, m) in ds.reps.iter().enumerate() {
            render_rep(&mut out, i + 1, m);
        }
        if let Some(acc) = &ds.acceptance {
            let _ = writeln!(
                out,
                "**合格基準（θ={:.1}{}）: {}**\n",
                acc.theta,
                if acc.provisional {
                    "・ラベルの裁定の前の暫定値"
                } else {
                    "・ラベルの裁定の後（裁定の方法は README）"
                },
                if acc.pass { "合格" } else { "不合格" }
            );
            out.push_str("| 項目 | 基準 | 値（反復ごと） | 判定 |\n| --- | --- | --- | --- |\n");
            for c in &acc.criteria {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} |",
                    c.name,
                    c.target,
                    c.values.join(" ／ "),
                    if c.pass { "○" } else { "×" }
                );
            }
            out.push('\n');
        }
    }
    out
}

/// `results/summary.md` を、results の全 JSON から作り直す（始めた時刻の順）。
pub(crate) fn write_summary() {
    let dir = eval_dir().join("results");
    let mut files: Vec<ResultFile> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .map(|p| {
            let text =
                std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        })
        .collect();
    files.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    let mut out = String::from(
        "# 判定の評価の結果（FEAT-1 の T7）\n\n\
         このファイルは `crates/llm-gateway/tests/decision_eval.rs` が results の JSON から書き出す（手で直さない）。\n\
         区間は Wilson の 95% 信頼区間。誤り（拒否・不正出力・途切れ・一時障害）は正解ではない・自動で処理しないとして数える。\n\
         経費の「経理確認」は「規程のフラグ OR AI が true OR 確からしさが θ 未満 OR 誤り」。\n\
         ECE は等頻度 5 区間で、同じ確からしさの点は同じ区間に入れる（同値が多いと区間は 5 より少なくなる。\
         この規則より前に書いた結果の ECE は、同値を並びの順で分けていた）。\n\n",
    );
    for file in &files {
        out.push_str(&render(file));
    }
    std::fs::write(dir.join("summary.md"), out).unwrap_or_else(|e| panic!("summary.md: {e}"));
}

/// 裁定のために test から無作為に抜く割合（種を固定）。
const ADJUDICATION_SAMPLE: f64 = 0.2;
const ADJUDICATION_SEED: &str = "decision-eval-adjudication-v1";

/// 裁定の材料（E1 の後）: モデルとラベルが食い違った行の全件と、test から無作為に抜いた 20%。
///
/// 食い違いは主な質問で見る（問い合わせは category、経費は規程のフラグの無い行の needs_review）。
/// どれかの反復で食い違うか誤りになった行を入れる。裁定の欄（`adjudication`）は空で書く。
///
/// 書く先は `adjudication/<データ名>.jsonl`。ただし、そこに human が裁定を書いた行（`adjudication.labels` が
/// 空でない行）があれば上書きせず、`adjudication/<データ名>-<結果の名前>.jsonl` に書く（裁定を失わない）。
pub(crate) fn write_adjudication(
    ds: &Dataset,
    results: &[CallResult],
    label: &str,
) -> (std::path::PathBuf, usize, usize) {
    let key = primary_key(ds.name);
    let test: Vec<_> = ds.rows.iter().filter(|r| r.split == "test").collect();
    let take = (test.len() as f64 * ADJUDICATION_SAMPLE).round() as usize;
    let mut ranked: Vec<(String, &str)> = test
        .iter()
        .map(|r| {
            let h = Sha256::digest(format!("{ADJUDICATION_SEED}:{}", r.id).as_bytes());
            (hex(&h), r.id.as_str())
        })
        .collect();
    ranked.sort();
    let sampled: BTreeSet<&str> = ranked.iter().take(take).map(|(_, id)| *id).collect();
    let mut lines = Vec::new();
    let mut disagreements = 0;
    for row in &test {
        let mine: Vec<&CallResult> = results
            .iter()
            .filter(|c| c.dataset == ds.name && c.id == row.id)
            .collect();
        let label = row.labels.get(key).cloned().unwrap_or(Value::Null);
        let judged = key != "needs_review" || row.rule_flags.is_empty();
        let disagree = judged
            && mine.iter().any(|c| match c.answer(key) {
                Some(a) => a.value != label,
                None => true,
            });
        let mut reasons = Vec::new();
        if disagree {
            reasons.push("disagreement");
            disagreements += 1;
        }
        if sampled.contains(row.id.as_str()) {
            reasons.push("random_20pct");
        }
        if reasons.is_empty() {
            continue;
        }
        let answers: Vec<Value> = mine
            .iter()
            .map(|c| {
                json!({
                    "rep": c.rep,
                    "error": c.error,
                    "answers": c.answers,
                })
            })
            .collect();
        let mut line = json!({
            "id": row.id,
            "reasons": reasons,
            "state": row.state,
            "labels": row.labels,
            "tags": row.tags,
            "model_answers": answers,
            "adjudication": { "labels": Value::Null, "note": "" },
        });
        if !row.rule_flags.is_empty() {
            line["rule_flags"] = json!(row.rule_flags);
        }
        lines.push(serde_json::to_string(&line).unwrap_or_default());
    }
    let dir = eval_dir().join("adjudication");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    let mut path = dir.join(format!("{}.jsonl", ds.name));
    if has_adjudicated(&path) {
        path = dir.join(format!("{}-{label}.jsonl", ds.name));
    }
    std::fs::write(&path, lines.join("\n") + "\n")
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    (path, lines.len(), disagreements)
}

/// データの裁定の材料のファイル（`adjudication/<データ名>.jsonl`）。
pub(crate) fn adjudication_path(name: &str) -> std::path::PathBuf {
    eval_dir()
        .join("adjudication")
        .join(format!("{name}.jsonl"))
}

/// 裁定の材料のファイルに、裁定を書いた行（`adjudication.labels` が空でない）があるか。
pub(crate) fn has_adjudicated(path: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|l| {
        serde_json::from_str::<Value>(l).is_ok_and(|v| {
            v.pointer("/adjudication/labels")
                .is_some_and(|x| !x.is_null())
        })
    })
}

/// 同じ名前の結果がもうあるか（上書きしない）。
pub(crate) fn result_exists(label: &str) -> bool {
    eval_dir()
        .join("results")
        .join(format!("{label}.json"))
        .exists()
}
