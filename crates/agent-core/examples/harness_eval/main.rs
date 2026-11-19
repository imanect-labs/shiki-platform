//! 小型・中型モデル向けハーネスの実 LLM 評価（`eval/agent-harness/`・docs/agent/small-model-harness.md）。
//!
//! 製品と同じ [`agent_core::run_agent`] を OpenRouter（OpenAI 互換）経由の実モデルで回し、
//! 決定的に採点できる 3 種の課題（集計・コード修正・チケット処理）を解かせる。
//!
//! - **窓の模擬**: OpenRouter のモデルは窓が広いが、オンプレの vLLM では 32k 前後で運用する。
//!   カタログの `context_window_tokens` を `--window` にし、実測 prompt が窓を超えたら
//!   その時点で「拒否された」とみなして run を打ち切る（溢れ失敗として数える）。
//! - **費用の上限**: 実行前後に OpenRouter の `/key` で利用額を読み、モデル群ごとの累計を
//!   `--ledger` に積む。上限（`--budget-usd`）に達していたら新しい run を始めない。
//! - **比較**: `--variant` でハーネスの機能を切り替える（baseline＝現行 main 相当）。
//!
//! 使い方（DB は会計の記録先。キーはプロキシが注入する前提でプレースホルダを送る）:
//! ```sh
//! STORAGE_TEST_DATABASE_URL=postgres://... cargo run -p shiki-agent-core --example harness_eval -- \
//!   --model gemma --task all --variant baseline --runs 1 --out eval/agent-harness/results/x.jsonl
//! ```

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    // 評価用の道具（製品コードではない）。課題データの組み立てで多用するものだけ許す。
    clippy::format_push_string,
    clippy::panic,
    clippy::write_with_newline,
    clippy::new_ret_no_self,
    clippy::struct_field_names
)]

mod sink;
mod tasks;

use std::io::Write as _;
use std::time::Instant;

use agent_core::{run_agent, AgentOptions, RunContext};
use authz::{AuthContext, Principal};
use llm_gateway::{
    GatewayConfig, LlmGateway, Message, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
    Role,
};
use sqlx::postgres::PgPoolOptions;

/// 評価対象のモデル（OpenRouter の ID・単価はマイクロ USD / 100 万トークン）。
struct ModelSpec {
    key: &'static str,
    real_id: &'static str,
    prompt_price: u64,
    completion_price: u64,
    /// 費用の勘定（open = gemma/qwen、haiku = Claude Haiku）。
    group: &'static str,
}

const MODELS: &[ModelSpec] = &[
    ModelSpec {
        key: "gemma",
        real_id: "google/gemma-4-31b-it",
        prompt_price: 200_000,
        completion_price: 400_000,
        group: "open",
    },
    ModelSpec {
        key: "gemma-moe",
        real_id: "google/gemma-4-26b-a4b-it",
        prompt_price: 90_000,
        completion_price: 300_000,
        group: "open",
    },
    ModelSpec {
        key: "qwen",
        real_id: "qwen/qwen3.8-27b",
        prompt_price: 60_000,
        completion_price: 2_200_000,
        group: "open",
    },
    ModelSpec {
        key: "haiku",
        real_id: "anthropic/claude-haiku-5.5",
        prompt_price: 100_000,
        completion_price: 500_000,
        group: "haiku",
    },
];

struct Args {
    models: Vec<String>,
    tasks: Vec<String>,
    variant: String,
    runs: usize,
    window: u32,
    max_tokens: u32,
    out: String,
    ledger: String,
    budget_open: f64,
    budget_haiku: f64,
    transcripts: Option<String>,
    seed: u64,
}

fn parse_args() -> Args {
    let mut a = Args {
        models: vec!["gemma".into()],
        tasks: vec!["all".into()],
        variant: "baseline".into(),
        runs: 1,
        window: 32_768,
        max_tokens: 4096,
        out: "harness_eval.jsonl".into(),
        ledger: "harness_eval_ledger.json".into(),
        budget_open: 10.0,
        budget_haiku: 50.0,
        transcripts: None,
        seed: 1,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let v = argv.get(i + 1).cloned().unwrap_or_default();
        match argv[i].as_str() {
            "--model" => a.models = v.split(',').map(str::to_string).collect(),
            "--task" => a.tasks = v.split(',').map(str::to_string).collect(),
            "--variant" => a.variant = v,
            "--runs" => a.runs = v.parse().expect("--runs"),
            "--window" => a.window = v.parse().expect("--window"),
            "--max-tokens" => a.max_tokens = v.parse().expect("--max-tokens"),
            "--out" => a.out = v,
            "--ledger" => a.ledger = v,
            "--budget-open" => a.budget_open = v.parse().expect("--budget-open"),
            "--budget-haiku" => a.budget_haiku = v.parse().expect("--budget-haiku"),
            "--transcripts" => a.transcripts = Some(v),
            "--seed" => a.seed = v.parse().expect("--seed"),
            other => panic!("unknown arg {other}"),
        }
        i += 2;
    }
    if a.tasks == ["all"] {
        a.tasks = tasks::ALL.iter().map(|s| (*s).to_string()).collect();
    }
    if a.tasks == ["diverse"] {
        a.tasks = tasks::DIVERSE.iter().map(|s| (*s).to_string()).collect();
    }
    a
}

/// OpenRouter の利用累計（USD）。費用の実測に使う（キーはプロキシが注入）。
async fn key_usage(http: &reqwest::Client) -> Option<f64> {
    let v: serde_json::Value = http
        .get("https://openrouter.ai/api/v1/key")
        .bearer_auth("proxy-injected")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v["data"]["usage"].as_f64()
}

fn load_ledger(path: &str) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn spent_of(ledger: &serde_json::Map<String, serde_json::Value>, group: &str) -> f64 {
    ledger
        .get(group)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
}

fn http_client() -> reqwest::Client {
    let mut b = reqwest::Client::builder();
    // TLS を中継する環境向けに、環境が供給する CA を信頼先へ足す（検証は無効化しない）。
    if let Ok(path) = std::env::var("SSL_CERT_FILE") {
        if let Ok(pem) = std::fs::read(path) {
            for cert in reqwest::Certificate::from_pem_bundle(&pem).unwrap_or_default() {
                b = b.add_root_certificate(cert);
            }
        }
    }
    b.build().unwrap()
}

fn ctx(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "eval".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "eval".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 自律プロファイルの system（chat の `autonomous_system_prompt` に倣う）。
fn system_prompt(task_system: &str) -> String {
    format!(
        "{task_system}\n\n\
         あなたは自律エージェントです。与えられた目標を達成するため、次の作法で進めてください:\n\
         - まず `plan` ツールで目標を数個のサブタスクに分解し、進捗に応じて計画を更新する。\n\
         - 必要な情報はツールで調べ、推測で埋めない。\n\
         - 目標を達成したら簡潔に要約して終了する。"
    )
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args = parse_args();
    let db_url =
        shiki_testenv::var("STORAGE_TEST_DATABASE_URL").expect("STORAGE_TEST_DATABASE_URL");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url)
        .await
        .expect("db");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let http = http_client();

    let models: Vec<ModelEntry> = MODELS
        .iter()
        .map(|m| ModelEntry {
            id: m.key.into(),
            real_id: Some(m.real_id.into()),
            prompt_price_micros_per_mtok: m.prompt_price,
            completion_price_micros_per_mtok: m.completion_price,
            context_window_tokens: Some(args.window),
            // 思考するモデルには思考強度を送る（要約は low で頼む・OpenRouter は reasoning_effort を受ける）。
            supports_effort: (m.key == "qwen").then_some(true),
            ..Default::default()
        })
        .collect();
    let gateway = LlmGateway::build(
        pool,
        http.clone(),
        GatewayConfig {
            provider: ProviderConfig {
                kind: ProviderKind::Openai,
                base_url: Some("https://openrouter.ai/api/v1".into()),
                // 実値はプロキシが差し替える（ヘッダの形だけ整える）。
                api_key: Some("proxy-injected".into()),
                ..Default::default()
            },
            catalog: ModelCatalog {
                default_model: "gemma".into(),
                models,
            },
            ..Default::default()
        },
    )
    .expect("gateway");

    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&args.out)
        .expect("out");

    for model_key in &args.models {
        let spec = MODELS
            .iter()
            .find(|m| m.key == model_key)
            .unwrap_or_else(|| panic!("unknown model {model_key}"));
        for task_name in &args.tasks {
            for rep in 0..args.runs {
                let ledger = load_ledger(&args.ledger);
                let cap = if spec.group == "haiku" {
                    args.budget_haiku
                } else {
                    args.budget_open
                };
                if spent_of(&ledger, spec.group) >= cap {
                    eprintln!("budget for {} exhausted; skipping", spec.group);
                    continue;
                }
                let before = key_usage(&http).await;
                let record = run_one(&gateway, spec, task_name, &args, rep).await;
                let after = key_usage(&http).await;
                let measured = match (before, after) {
                    (Some(b), Some(a)) => Some(a - b),
                    _ => None,
                };
                let est = record["cost_usd_est"].as_f64().unwrap_or(0.0);
                let mut ledger = load_ledger(&args.ledger);
                // `/key` の利用額は反映が遅れる（直後は 0 のことが多い）ので、台帳は単価からの
                // 見積りで積む。実額は別途 `/key` の累計で突き合わせる。
                let add = est;
                let total = spent_of(&ledger, spec.group) + add;
                ledger.insert(spec.group.into(), total.into());
                std::fs::write(&args.ledger, serde_json::to_string_pretty(&ledger).unwrap())
                    .unwrap();
                let mut record = record;
                record["cost_usd_measured"] = measured.into();
                writeln!(out, "{record}").unwrap();
                println!(
                    "{} {} {} rep{}: score={:.2} success={} stop={} steps={} peak={} compactions={}/{} cost=${:.4} (group {} total ${:.3})",
                    spec.key,
                    task_name,
                    args.variant,
                    rep,
                    record["score"].as_f64().unwrap_or(0.0),
                    record["success"],
                    record["stop"],
                    record["steps"],
                    record["peak_prompt_tokens"],
                    record["compactions"],
                    record["compaction_attempts"],
                    add,
                    spec.group,
                    total
                );
            }
        }
    }
}

async fn run_one(
    gateway: &LlmGateway,
    spec: &ModelSpec,
    task_name: &str,
    args: &Args,
    rep: usize,
) -> serde_json::Value {
    let task = tasks::make(task_name, args.seed.wrapping_add(rep as u64));
    let tools = task.tools();
    let mut opts = AgentOptions::autonomous(task.max_steps(), None, 600_000, 1_000_000);
    opts.model = Some(spec.key.into());
    opts.max_tokens = Some(args.max_tokens);
    opts.system = Some(system_prompt(&task.system()));
    // 評価では tool search を使わない（ツール数が少ない）。
    opts.tool_search.enabled = false;
    apply_variant(&mut opts, &args.variant);
    // 元に戻せる圧縮と最終ツールの催促（`full` のみ）。保存先は run ごとに新しく作る。
    if args.variant == "full" {
        opts.archive = Some(std::sync::Arc::new(agent_core::InMemoryArchive::new()));
        opts.final_tool = Some(task.final_tool().to_string());
    }

    let tenant = format!("t-{}", uuid::Uuid::new_v4());
    let c = ctx(&tenant);
    let run = RunContext {
        ctx: &c,
        idempotency_prefix: format!("eval-{}:0", uuid::Uuid::new_v4()),
        trace_id: None,
        input_preview: task_name.into(),
        app_id: None,
        owns_trace: true,
    };
    let mut sink = sink::EvalSink::new(args.window);
    let started = Instant::now();
    let result = run_agent(
        gateway,
        &tools,
        vec![Message::text(Role::User, task.prompt())],
        &run,
        &opts,
        None,
        None,
        &mut sink,
    )
    .await;
    let elapsed = started.elapsed().as_secs_f64();
    let grade = task.grade();
    let attempts = result.as_ref().map_or(0, |o| o.checkpoint.compactions);
    let (stop, steps, cost_micros, tokens, messages) = match &result {
        Ok(o) => (
            format!("{:?}", o.stop),
            o.checkpoint.spent.steps,
            o.checkpoint.spent.cost_usd_micros,
            o.checkpoint.spent.tokens,
            Some(&o.checkpoint.messages),
        ),
        // エラーで終わると消費が返らない。prompt の合計と、1 ステップ 600 トークンの出力で見積もる。
        Err(e) => {
            let est = (u128::from(sink.prompt_sum) * u128::from(spec.prompt_price)
                + (sink.steps as u128) * 600 * u128::from(spec.completion_price))
                / 1_000_000;
            let _ = sink.cost_micros_hint;
            (
                format!("Error({e})"),
                sink.steps,
                est as i64,
                sink.prompt_sum,
                None,
            )
        }
    };
    if let Some(dir) = &args.transcripts {
        let _ = std::fs::create_dir_all(dir);
        let path = format!(
            "{dir}/{}-{}-{}-{}-{}.json",
            spec.key,
            task_name,
            args.variant,
            rep,
            chrono::Utc::now().format("%H%M%S")
        );
        let body = serde_json::json!({
            "messages": messages,
            "events": sink.log,
            "grade": grade.details,
            "stop": stop,
        });
        let _ = std::fs::write(path, serde_json::to_string_pretty(&body).unwrap());
    }
    serde_json::json!({
        "model": spec.key,
        "real_id": spec.real_id,
        "task": task_name,
        "variant": args.variant,
        "rep": rep,
        "window": args.window,
        "score": grade.score,
        "success": grade.success,
        "details": grade.details,
        "stop": stop,
        "steps": steps,
        "tokens": tokens,
        "peak_prompt_tokens": sink.peak_prompt,
        "overflow": sink.overflowed,
        "compactions": sink.compactions,
        // 要約を試みた回数（縮まずに捨てた分を含む）。
        "compaction_attempts": attempts,
        "tool_calls": sink.tool_calls,
        "recalls": result.as_ref().map_or(0, |o| o.checkpoint.recalls),
        "archived": result.as_ref().map_or(0, |o| o.checkpoint.archive_seq),
        "tool_errors": sink.tool_errors,
        "text_only_turns": sink.text_turns,
        "cost_usd_est": cost_micros as f64 / 1e6,
        "elapsed_s": elapsed,
        "at": chrono::Utc::now().to_rfc3339(),
    })
}

/// ハーネスの機能を variant 名で切り替える（比較用）。
///
/// - `baseline`: 現行 main 相当（剪定のみ）。
/// - `improved`: このブランチで足した機能をすべて有効にする（既定値）。
fn apply_variant(opts: &mut AgentOptions, variant: &str) {
    match variant {
        // 現行 main 相当（剪定のみ）。
        "baseline" => {
            opts.compaction = agent_core::CompactionOptions::disabled();
            opts.aids = agent_core::HarnessAids::none();
        }
        "improved" | "full" => {}
        // 機能ごとの寄与（ablation）。
        "no-compaction" => opts.compaction = agent_core::CompactionOptions::disabled(),
        "no-budget" => opts.aids.step_result_budget = false,
        "no-discipline" => opts.aids.discipline_prompt = false,
        other => panic!("unknown variant {other}"),
    }
}
