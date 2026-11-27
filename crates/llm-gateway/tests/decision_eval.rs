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

// 普段のテストでも走る試験（実 LLM を呼ばない）。
#[path = "decision_eval/checks.rs"]
mod checks;
#[path = "decision_eval/data.rs"]
mod data;
#[path = "decision_eval/metrics.rs"]
mod metrics;
#[path = "decision_eval/report.rs"]
mod report;
#[path = "decision_eval/run.rs"]
mod run;
#[path = "decision_eval/score.rs"]
mod score;
#[path = "decision_eval/setup.rs"]
mod setup;

use std::collections::VecDeque;

use authz::AuthContext;
use llm_gateway::providers::llm_decider::PROMPT_VERSION;
use llm_gateway::LlmGateway;
use sqlx::postgres::PgPoolOptions;

use data::{Dataset, DATASETS};
use metrics::THETAS;
use run::{Job, Outcome, Runner};

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
        authz::TenantId::parse(EVAL_TENANT).unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "実 LLM を呼ぶ評価（DECISION_EVAL=1 のときだけ・手順は eval/decision/README.md）"]
async fn decision_eval() {
    if std::env::var("DECISION_EVAL").as_deref() != Ok("1") {
        eprintln!("DECISION_EVAL=1 でないので走らない");
        return;
    }
    let stage_name = env_or("DECISION_EVAL_STAGE", "");
    if stage_name == "summary" {
        // 実 LLM を呼ばずに、results の JSON から summary.md だけを書き直す。
        report::write_summary();
        return;
    }
    if stage_name == "rescore" {
        // 実 LLM を呼ばずに、保存した結果の答えを今のデータで採点し直す（裁定の後のラベルの直しを反映する）。
        let from = std::env::var("DECISION_EVAL_RESULT")
            .expect("DECISION_EVAL_RESULT に採点し直す結果の名前（例: 2026-10-27-E1）を渡す");
        let file = score::rescore(&from);
        let path = report::write_result(&file);
        report::write_summary();
        eprintln!("{from} を採点し直した → {}", path.display());
        for ds in &file.datasets {
            if let Some(a) = &ds.acceptance {
                eprintln!(
                    "  {}: 合格基準（{}）{}",
                    ds.name,
                    if a.provisional {
                        "暫定"
                    } else {
                        "裁定の後"
                    },
                    if a.pass { "合格" } else { "不合格" }
                );
            }
        }
        return;
    }
    let stage = data::stage(&stage_name).unwrap_or_else(|| {
        panic!(
            "DECISION_EVAL_STAGE は E0・E4・E1・E2・E3・summary・rescore のどれか（{stage_name}）"
        )
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
        // コードのプロンプトの版が凍結と違えば、1 件も呼ばずに止める（PIT-81）。
        assert_eq!(
            f.prompt_version, PROMPT_VERSION,
            "コードのプロンプトの版 {PROMPT_VERSION} が凍結した版 {} と違う（凍結し直すか、版を戻す）",
            f.prompt_version
        );
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
    let config = setup::gateway_config(
        &setup::config_path(),
        &model,
        stage.effort,
        stage.samples,
        &relay,
    );
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

    let started_at = chrono::Utc::now().to_rfc3339();
    let mut rest = VecDeque::from(jobs);
    let mut outcome = if frozen.is_some() {
        run::probe_version(|job| runner.run(vec![job]), &mut rest, frozen.as_ref()).await
    } else {
        Outcome {
            calls: Vec::new(),
            stopped: None,
        }
    };
    if outcome.stopped.is_none() && !rest.is_empty() {
        let more = runner.run(rest.into_iter().collect()).await;
        outcome.calls.extend(more.calls);
        outcome.stopped = more.stopped;
    }
    // 流した後にも、答えた版がすべて凍結の版かを確かめる。違う版が混ざれば合否を出さない。
    let resolved_models = data::resolved_models(&outcome.calls);
    let mismatch = data::version_mismatch(&resolved_models, frozen.as_ref());
    if let Some(m) = &mismatch {
        outcome
            .stopped
            .get_or_insert_with(|| format!("{m}ので合否を出さない"));
    }
    let finished_at = chrono::Utc::now().to_rfc3339();
    let after = setup::relay_spend(&http, &relay).await;
    let (db_rows, db_micros) = setup::usage_of_run(&pool, ctx.tenant_id(), &key_prefix).await;

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
        resolved_models,
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
                let theta = frozen
                    .as_ref()
                    .filter(|_| mismatch.is_none())
                    .and_then(|f| f.theta.get(ds.name).copied());
                score::dataset_result(ds, &outcome.calls, stage.reps, theta, frozen.is_none())
            })
            .collect(),
        frozen: frozen.clone(),
        datasets_sha256: datasets
            .iter()
            .map(|ds| (ds.name.to_string(), ds.rows_sha256.clone()))
            .collect(),
        rescored_from: None,
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
            let (to, n, d) = report::write_adjudication(ds, &file.results, &label);
            eprintln!(
                "  裁定の材料 {}: {n} 行（うち食い違い {d}）→ {}",
                ds.name,
                to.display()
            );
        }
    }
    for ds in &file.datasets {
        if let Some(t) = ds.suggested_theta {
            eprintln!("  {}: θ の案 {t:.1}", ds.name);
        }
        if let Some(a) = &ds.acceptance {
            eprintln!(
                "  {}: 合格基準（{}）{}",
                ds.name,
                if a.provisional {
                    "暫定"
                } else {
                    "裁定の後"
                },
                if a.pass { "合格" } else { "不合格" }
            );
        }
    }
    if let Some(s) = &file.stopped {
        panic!("途中で止めた: {s}（結果は {} に書いた）", path.display());
    }
}
