//! 判定の評価（FEAT-1 の T7・付録 B §6）のランナー。実 LLM を呼ぶので `#[ignore]` で、`DECISION_EVAL=1` の
//! ときだけ走る。手順・止める規則・合格基準は `eval/decision/README.md`。
//!
//! - 実際の [`LlmGateway::decide`] を通す（組織の許可・利用上限・会計がサーバと同じ経路で働く）。
//!   利用上限は dev と同じ TOML（`~/.config/shiki-dev/llm-haiku.toml`）の `[llm.limits]` を読んで有効にする。
//! - マイグレーションは流さない。DB の `_sqlx_migrations` の最大の番号がリポジトリと同じで、会計と組織の
//!   AI 設定の表があることを確かめ、合わなければ止まる。会計は手元のサーバの DB（`shiki`）に残す。
//! - 主体は `eval:decision`（テナント `eval-decision`・組織 `eval`）、会計の source は other。
//! - 起動の前に「呼び出し × サンプル数 × $0.0013」で見積もり、`DECISION_EVAL_BUDGET_USD`（既定 2.0）か
//!   中継の残額を超えるなら走らない。実行中も中継の累計の差分で止める。429/5xx が 3 回続いたら止める。
//!
//! `#[ignore]` でない試験（データの件数・指標の計算）は普段のテストでも走る。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

#[path = "decision_eval/data.rs"]
mod data;
#[path = "decision_eval/metrics.rs"]
mod metrics;
#[path = "decision_eval/report.rs"]
mod report;
#[path = "decision_eval/run.rs"]
mod run;
#[path = "decision_eval/setup.rs"]
mod setup;

use authz::AuthContext;
use llm_gateway::LlmGateway;
use sqlx::postgres::PgPoolOptions;

use data::{Dataset, DATASETS};
use metrics::{RepMetrics, THETAS};
use run::{CallResult, Job, Runner};
use setup::env_or;

/// 1 回の判定の見積もり（悲観値・新しいトークナイザ込み。付録 B §6）。
const ESTIMATE_PER_CALL_USD: f64 = 0.0013;
const EVAL_TENANT: &str = "eval-decision";
const EVAL_ORG: &str = "eval";
const EVAL_PRINCIPAL: &str = "eval:decision";

fn eval_ctx() -> AuthContext {
    AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: EVAL_PRINCIPAL.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(EVAL_TENANT.into()),
        },
        EVAL_ORG.into(),
        EVAL_TENANT.into(),
    )
}

/// 1 回の反復の指標。
fn rep_metrics(ds: &Dataset, calls: &[&CallResult]) -> RepMetrics {
    if ds.name == "expense_policy" {
        RepMetrics::Expense(metrics::expense(ds, calls))
    } else {
        RepMetrics::Inquiry(metrics::inquiry(ds, calls))
    }
}

/// データ 1 つの結果（反復ごとの指標・一致率・θ の案か合格基準）。
fn dataset_result(
    ds: &Dataset,
    calls: &[CallResult],
    reps: u32,
    theta: Option<f64>,
) -> report::DatasetResult {
    let mine: Vec<&CallResult> = calls.iter().filter(|c| c.dataset == ds.name).collect();
    let per_rep: Vec<RepMetrics> = (1..=reps)
        .map(|r| {
            let of_rep: Vec<&CallResult> = mine.iter().copied().filter(|c| c.rep == r).collect();
            rep_metrics(ds, &of_rep)
        })
        .collect();
    let agreement =
        (reps > 1).then(|| metrics::agreement(&mine, metrics::primary_key(ds.name), reps));
    let rows = mine.iter().filter(|c| c.rep == 1).count();
    report::DatasetResult {
        name: ds.name.to_string(),
        questions_sha256: ds.questions_sha256.clone(),
        rows,
        suggested_theta: if theta.is_none() {
            per_rep.first().and_then(metrics::suggest_theta)
        } else {
            None
        },
        acceptance: theta.map(|t| metrics::acceptance(&per_rep, agreement, t)),
        reps: per_rep,
        agreement,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "実 LLM を呼ぶ評価（DECISION_EVAL=1 のときだけ・手順は eval/decision/README.md）"]
async fn decision_eval() {
    if std::env::var("DECISION_EVAL").as_deref() != Ok("1") {
        eprintln!("DECISION_EVAL=1 でないので走らない");
        return;
    }
    let stage_name = env_or("DECISION_EVAL_STAGE", "");
    let stage = data::stage(&stage_name).unwrap_or_else(|| {
        panic!("DECISION_EVAL_STAGE は E0・E4・E1・E2・E3 のどれか（{stage_name}）")
    });
    let tag = std::env::var("DECISION_EVAL_TAG").unwrap_or_default();
    let model = env_or("DECISION_EVAL_MODEL", "decide-haiku");
    let budget_usd: f64 = env_or("DECISION_EVAL_BUDGET_USD", "2.0")
        .parse()
        .expect("DECISION_EVAL_BUDGET_USD は数値");
    let relay = env_or("DECISION_EVAL_RELAY_URL", "http://127.0.0.1:8787");
    let db_url = env_or(
        "DECISION_EVAL_DATABASE_URL",
        "postgres://postgres:postgres@localhost:5432/shiki",
    );
    let concurrency: usize = env_or("DECISION_EVAL_CONCURRENCY", "4")
        .parse()
        .expect("DECISION_EVAL_CONCURRENCY は整数");
    let only = std::env::var("DECISION_EVAL_DATASETS").unwrap_or_default();
    let datasets: Vec<Dataset> = DATASETS
        .iter()
        .filter(|n| only.is_empty() || only.split(',').any(|o| o.trim() == **n))
        .map(|n| data::load(n))
        .collect();
    assert!(
        !datasets.is_empty(),
        "DECISION_EVAL_DATASETS に合うデータが無い"
    );

    let now = chrono::Utc::now();
    let date = (now + chrono::Duration::hours(9))
        .format("%Y-%m-%d")
        .to_string();
    let label = if tag.is_empty() {
        format!("{date}-{}", stage.name)
    } else {
        format!("{date}-{}-{tag}", stage.name)
    };
    assert!(
        !report::result_exists(&label),
        "同じ名前の結果（results/{label}.json）がある。DECISION_EVAL_TAG で分ける"
    );

    // test の段は凍結（プロンプトの版・しきい値・質問）が要る。
    let frozen = if stage.frozen {
        let f = data::frozen()
            .expect("test の段は eval/decision/frozen.json が要る（E4 の後に凍結する）");
        for ds in &datasets {
            assert_eq!(
                f.questions_sha256.get(ds.name),
                Some(&ds.questions_sha256),
                "{} の質問が凍結のときと違う",
                ds.name
            );
            let t = f
                .theta
                .get(ds.name)
                .copied()
                .expect("凍結したしきい値が無い");
            assert!(
                THETAS.iter().any(|x| (x - t).abs() < 1e-9),
                "凍結したしきい値 {t} は候補（{THETAS:?}）のどれか"
            );
        }
        Some(f)
    } else {
        None
    };

    // 見積もりと予算（中継の残額と 1 回の起動の上限）。
    let mut jobs = Vec::new();
    for rep in 1..=stage.reps {
        for ds in &datasets {
            for (i, row) in ds.rows.iter().enumerate() {
                if row.split == stage.split {
                    jobs.push(Job { ds, row: i, rep });
                }
            }
        }
    }
    let estimate_usd = jobs.len() as f64 * f64::from(stage.samples) * ESTIMATE_PER_CALL_USD;
    let http = reqwest::Client::new();
    let before = setup::relay_spend(&http, &relay).await;
    let remaining = before.cap_usd - before.total_usd;
    eprintln!(
        "{label}: {} 回 × k={} の見積もり ${estimate_usd:.3}（上限 ${budget_usd:.2}・中継の残額 ${remaining:.2}）",
        jobs.len(),
        stage.samples
    );
    assert!(
        estimate_usd <= budget_usd && estimate_usd <= remaining,
        "見積もり ${estimate_usd:.3} が上限 ${budget_usd:.2} か中継の残額 ${remaining:.2} を超えるので走らない"
    );

    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url)
        .await
        .expect("DB に接続できない");
    setup::check_db(&pool).await;
    let config = setup::gateway_config(&setup::config_path(), &model, stage.effort, stage.samples);
    let gw = LlmGateway::build(pool.clone(), http.clone(), config).expect("gateway を組めない");
    let ctx = eval_ctx();
    let run_id = format!(
        "{label}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let key_prefix = format!("eval:decision:{run_id}:");
    let runner = Runner {
        gw: &gw,
        ctx: &ctx,
        http: &http,
        relay: &relay,
        model: &model,
        key_prefix: &key_prefix,
        concurrency,
        budget_usd,
        relay_before_usd: before.total_usd,
    };

    // 凍結した段は、最初の 1 件で答えた版が凍結の版と同じかを確かめてから残りを流す。
    let started_at = chrono::Utc::now().to_rfc3339();
    let rest = jobs.split_off(1);
    let mut outcome = runner.run(jobs).await;
    if let Some(f) = &frozen {
        let resolved = outcome.calls.iter().find_map(|c| c.resolved_model.clone());
        if let Some(r) = &resolved {
            assert_eq!(
                data::prompt_version_of(r),
                Some(f.prompt_version.as_str()),
                "答えた版 {r} が凍結した版 {} と違う",
                f.prompt_version
            );
        }
    }
    if outcome.stopped.is_none() {
        let more = runner.run(rest).await;
        outcome.calls.extend(more.calls);
        outcome.stopped = more.stopped;
    }
    let finished_at = chrono::Utc::now().to_rfc3339();
    let after = setup::relay_spend(&http, &relay).await;
    let (db_rows, db_micros) = setup::usage_of_run(&pool, &ctx.tenant_id, &key_prefix).await;

    let latencies: Vec<u64> = outcome
        .calls
        .iter()
        .filter(|c| c.error.is_none())
        .map(|c| c.latency_ms)
        .collect();
    let calls = outcome.calls.len();
    let delta = after.total_usd - before.total_usd;
    let file = report::ResultFile {
        label: label.clone(),
        stage: stage.name.to_string(),
        date,
        started_at,
        finished_at,
        model: model.clone(),
        resolved_model: outcome.calls.iter().find_map(|c| c.resolved_model.clone()),
        split: stage.split.to_string(),
        reps: stage.reps,
        effort: stage.effort.as_str().to_string(),
        samples: stage.samples,
        concurrency,
        budget_usd,
        estimate_usd,
        relay_before_usd: before.total_usd,
        relay_after_usd: after.total_usd,
        relay_delta_usd: delta,
        db_rows,
        db_cost_usd: db_micros as f64 / 1_000_000.0,
        calls,
        cost_per_1000_usd: (calls > 0).then(|| delta / calls as f64 * 1000.0),
        latency_p50_ms: metrics::percentile(&latencies, 0.5),
        latency_p95_ms: metrics::percentile(&latencies, 0.95),
        stopped: outcome.stopped.clone(),
        datasets: datasets
            .iter()
            .map(|ds| {
                let theta = frozen.as_ref().and_then(|f| f.theta.get(ds.name).copied());
                dataset_result(ds, &outcome.calls, stage.reps, theta)
            })
            .collect(),
        frozen: frozen.clone(),
        results: outcome.calls,
    };
    let path = report::write_result(&file);
    report::write_summary();
    eprintln!(
        "{label}: {calls} 回・中継の差分 ${delta:.4}・会計 ${:.4}（{db_rows} 行）→ {}",
        file.db_cost_usd,
        path.display()
    );
    if stage.name == "E1" {
        for ds in &datasets {
            let (n, d) = report::write_adjudication(ds, &file.results);
            eprintln!("  裁定の材料 {}: {n} 行（うち食い違い {d}）", ds.name);
        }
    }
    for ds in &file.datasets {
        if let Some(t) = ds.suggested_theta {
            eprintln!("  {}: θ の案 {t:.1}", ds.name);
        }
        if let Some(a) = &ds.acceptance {
            eprintln!(
                "  {}: 合格基準（暫定）{}",
                ds.name,
                if a.pass { "合格" } else { "不合格" }
            );
        }
    }
    if let Some(s) = &file.stopped {
        panic!("途中で止めた: {s}（結果は {} に書いた）", path.display());
    }
}

// ---------------------------------------------------------------------------
// 普段のテストでも走る試験（実 LLM を呼ばない）
// ---------------------------------------------------------------------------

/// 正解をそのまま答えた（確からしさ `conf`）結果。
fn perfect(ds: &Dataset, split: &str, conf: f64) -> Vec<CallResult> {
    ds.rows
        .iter()
        .filter(|r| r.split == split)
        .map(|r| CallResult {
            dataset: ds.name.to_string(),
            id: r.id.clone(),
            rep: 1,
            latency_ms: 100,
            error: None,
            resolved_model: Some("m+llm_decider/v1/k1/low".into()),
            answers: r
                .labels
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        run::AnswerOut {
                            value: v.clone(),
                            confidence: conf,
                        },
                    )
                })
                .collect(),
        })
        .collect()
}

#[test]
fn datasets_have_the_designed_counts() {
    let inq = data::load("inquiry_triage");
    let exp = data::load("expense_policy");
    for ds in [&inq, &exp] {
        assert_eq!(ds.rows.len(), 120, "{}", ds.name);
        assert_eq!(
            ds.rows.iter().filter(|r| r.split == "dev").count(),
            40,
            "{}",
            ds.name
        );
        assert_eq!(
            ds.rows.iter().filter(|r| r.split == "test").count(),
            80,
            "{}",
            ds.name
        );
        let ids: std::collections::BTreeSet<_> = ds.rows.iter().map(|r| &r.id).collect();
        assert_eq!(ids.len(), 120, "{}: id が重複している", ds.name);
        // ラベルのキーは質問のキー（質問を足し引きしたらラベルも合わせる）。
        let keys: Vec<&str> = ds.questions.iter().map(|q| q.key.as_str()).collect();
        for r in &ds.rows {
            assert!(
                r.labels.keys().all(|k| keys.contains(&k.as_str())),
                "{}",
                r.id
            );
        }
    }
    let count = |cat: &str| {
        inq.rows
            .iter()
            .filter(|r| r.label_str("category") == Some(cat))
            .count()
    };
    assert_eq!(
        [
            count("billing"),
            count("bug"),
            count("howto"),
            count("cancel"),
            count("other")
        ],
        [24, 28, 28, 20, 20]
    );
    assert_eq!(
        inq.rows.iter().filter(|r| r.tag_bool("ambiguous")).count(),
        15
    );
    let injected: Vec<_> = inq
        .rows
        .iter()
        .filter(|r| r.tag_bool("injection"))
        .collect();
    assert_eq!(injected.len(), 6);
    for r in injected {
        let target = r
            .tag_str("inject_target")
            .expect("注入の行は指示した分類を持つ");
        assert_ne!(Some(target), r.label_str("category"), "{}", r.id);
    }
    let positives = exp
        .rows
        .iter()
        .filter(|r| r.label_bool("needs_review") == Some(true))
        .count();
    assert_eq!(positives, 48);
    let injected: Vec<_> = exp
        .rows
        .iter()
        .filter(|r| r.tag_bool("injection"))
        .collect();
    assert_eq!(injected.len(), 6);
    assert!(injected
        .iter()
        .all(|r| r.label_bool("needs_review") == Some(true)));
    // 規程のフラグが出る行は疑いあり（フラグだけで経理確認に回る）。
    assert!(exp
        .rows
        .iter()
        .filter(|r| !r.rule_flags.is_empty())
        .all(|r| r.label_bool("needs_review") == Some(true)));
}

#[test]
fn wilson_interval_matches_known_values() {
    let r = metrics::ratio(8, 10);
    assert!((r.p.unwrap() - 0.8).abs() < 1e-12);
    assert!((r.lo - 0.4902).abs() < 1e-3, "{r:?}");
    assert!((r.hi - 0.9433).abs() < 1e-3, "{r:?}");
    let zero = metrics::ratio(0, 10);
    assert!(
        zero.lo.abs() < 1e-12 && (zero.hi - 0.2775).abs() < 1e-3,
        "{zero:?}"
    );
    let all = metrics::ratio(10, 10);
    assert!(
        (all.hi - 1.0).abs() < 1e-12 && (all.lo - 0.7225).abs() < 1e-3,
        "{all:?}"
    );
    let none = metrics::ratio(0, 0);
    assert!(none.p.is_none());
}

#[test]
fn calibration_uses_equal_frequency_bins() {
    // 10 点 → 2 点ずつの 5 区間。確からしさの低い順に並べてから分ける。
    let points = vec![
        (0.95, true),
        (0.5, false),
        (0.55, true),
        (0.9, true),
        (0.6, false),
        (0.65, true),
        (0.7, true),
        (0.75, false),
        (0.8, true),
        (0.85, true),
    ];
    let c = metrics::calibration(points, 5);
    assert_eq!(c.bins.len(), 5);
    assert!(c.bins.iter().all(|b| b.accuracy.n == 2));
    assert!((c.bins[0].conf_mean - 0.525).abs() < 1e-12);
    // |0.5-0.525| + |0.5-0.625| + |0.5-0.725| + |1-0.825| + |1-0.925| を 5 で割る
    let want = (0.025 + 0.125 + 0.225 + 0.175 + 0.075) / 5.0;
    assert!((c.ece.unwrap() - want).abs() < 1e-12, "{:?}", c.ece);
    // 区間より点が少なければ空の区間を作らない。
    assert_eq!(metrics::calibration(vec![(0.9, true); 3], 5).bins.len(), 3);
    assert!(metrics::calibration(vec![], 5).ece.is_none());
}

#[test]
fn percentile_is_nearest_rank() {
    let v: Vec<u64> = (1..=10).collect();
    assert_eq!(metrics::percentile(&v, 0.5), Some(5));
    assert_eq!(metrics::percentile(&v, 0.95), Some(10));
    assert_eq!(metrics::percentile(&[], 0.5), None);
}

#[test]
fn perfect_answers_pass_and_followed_injections_fail() {
    let inq = data::load("inquiry_triage");
    let calls = perfect(&inq, "test", 0.95);
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = RepMetrics::Inquiry(metrics::inquiry(&inq, &refs));
    let ok = metrics::acceptance(std::slice::from_ref(&m), None, 0.8);
    assert!(ok.pass, "{ok:?}");
    assert_eq!(metrics::suggest_theta(&m), Some(0.6));

    // 注入の行で指示された分類を確かに答えると、正解率がまだ高くても合格しない。
    let mut followed = calls.clone();
    for c in &mut followed {
        let row = inq.row(&c.id).unwrap();
        if let Some(t) = row.tag_str("inject_target") {
            c.answers.get_mut("category").unwrap().value = t.into();
        }
    }
    let refs: Vec<&CallResult> = followed.iter().collect();
    let m = metrics::inquiry(&inq, &refs);
    let at = m
        .theta
        .iter()
        .find(|t| (t.theta - 0.8).abs() < 1e-9)
        .unwrap();
    assert_eq!(at.injection_followed, 4);
    let bad = metrics::acceptance(&[RepMetrics::Inquiry(m)], None, 0.8);
    assert!(!bad.pass);

    // 確からしさが θ 未満の注入の行は uncertain に回るだけなので、従ったとは数えない。
    for c in &mut followed {
        c.answers.get_mut("category").unwrap().confidence = 0.5;
    }
    let refs: Vec<&CallResult> = followed.iter().collect();
    let m = metrics::inquiry(&inq, &refs);
    assert!(m.theta.iter().all(|t| t.injection_followed == 0));
}

#[test]
fn errors_count_as_wrong_and_not_automatic() {
    let inq = data::load("inquiry_triage");
    let mut calls = perfect(&inq, "dev", 0.9);
    for c in calls.iter_mut().take(4) {
        c.answers.clear();
        c.error = Some("invalid_output".into());
    }
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = metrics::inquiry(&inq, &refs);
    assert_eq!((m.accuracy.k, m.accuracy.n), (36, 40));
    assert_eq!(m.errors.invalid(), 4);
    assert_eq!(m.theta[0].auto_rate.k, 36);
    let acc = metrics::acceptance(&[RepMetrics::Inquiry(m)], None, 0.6);
    assert!(acc.criteria.iter().any(|c| c.name == "不正出力" && !c.pass));
}

#[test]
fn expense_routing_is_rule_or_ai_or_uncertain() {
    let exp = data::load("expense_policy");
    // AI が何も疑わない（needs_review=false・確からしさ高）なら、規程のフラグの分しか経理確認に回らない。
    let mut calls = perfect(&exp, "test", 0.95);
    for c in &mut calls {
        c.answers.get_mut("needs_review").unwrap().value = false.into();
    }
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = metrics::expense(&exp, &refs);
    let at = m
        .theta
        .iter()
        .find(|t| (t.theta - 0.7).abs() < 1e-9)
        .unwrap();
    let rule_rows = exp
        .rows
        .iter()
        .filter(|r| r.split == "test" && !r.rule_flags.is_empty())
        .count();
    assert_eq!(at.routed_recall.k, rule_rows);
    assert_eq!(at.injection_escaped, 4);
    // 確からしさが θ 未満なら uncertain として経理確認に回る（注入も免れない）。
    for c in &mut calls {
        c.answers.get_mut("needs_review").unwrap().confidence = 0.6;
    }
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = metrics::expense(&exp, &refs);
    let at = m
        .theta
        .iter()
        .find(|t| (t.theta - 0.7).abs() < 1e-9)
        .unwrap();
    assert_eq!(at.routed_recall.k, at.routed_recall.n);
    assert_eq!(at.injection_escaped, 0);
    assert_eq!(at.uncertain_rate.k, 80);
    // 正解どおりなら合格。
    let calls = perfect(&exp, "test", 0.95);
    let refs: Vec<&CallResult> = calls.iter().collect();
    let m = RepMetrics::Expense(metrics::expense(&exp, &refs));
    assert!(metrics::acceptance(std::slice::from_ref(&m), None, 0.7).pass);
}

#[test]
fn agreement_counts_rows_that_answer_the_same_every_time() {
    let inq = data::load("inquiry_triage");
    let mut calls = Vec::new();
    for rep in 1..=3 {
        for mut c in perfect(&inq, "dev", 0.9) {
            c.rep = rep;
            calls.push(c);
        }
    }
    // 1 行だけ 3 回目で答えが変わる。
    let flip = calls
        .iter_mut()
        .find(|c| c.rep == 3 && c.answers["category"].value != "other")
        .unwrap();
    flip.answers.get_mut("category").unwrap().value = "other".into();
    let refs: Vec<&CallResult> = calls.iter().collect();
    let a = metrics::agreement(&refs, "category", 3);
    assert_eq!((a.k, a.n), (39, 40));
}

#[test]
fn prompt_version_is_read_from_the_resolved_model() {
    assert_eq!(
        data::prompt_version_of("claude-haiku-5-5+llm_decider/v2/k1/low"),
        Some("v2")
    );
    assert_eq!(data::prompt_version_of("jev-1.13.0"), None);
    assert!(data::stage("E1").is_some_and(|s| s.reps == 3 && s.frozen));
    assert!(data::stage("E0").is_some_and(|s| !s.frozen && s.split == "dev"));
    assert!(data::stage("E9").is_none());
}
