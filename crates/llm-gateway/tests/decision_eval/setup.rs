//! 評価の前提の確かめ: 設定（dev と同じ TOML）・DB（マイグレーションは流さない）・中継の累計額。

use std::path::{Path, PathBuf};

use llm_gateway::{
    AutoConfig, DecisionTuning, Effort, GatewayConfig, ModelCatalog, ModelEntry, ProviderConfig,
    ProviderKind, UsageLimitsConfig, DEFAULT_PROVIDER_ID,
};
use serde::Deserialize;
use sqlx::PgPool;

/// 環境変数（無ければ既定）。
pub(crate) fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// dev の TOML（`~/.config/shiki-dev/llm-haiku.toml`）。
pub(crate) fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("DECISION_EVAL_CONFIG") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    Path::new(&home).join(".config/shiki-dev/llm-haiku.toml")
}

/// TOML の `[llm]` のうち、評価に要る部分（既定プロバイダ・追加プロバイダ・モデル・Auto・利用上限）。
///
/// サーバは `api::config::LlmConfig` → `wiring::build_gateway` で同じ TOML を読む。llm-gateway は api に
/// 依存できないので、gateway 側の型（`ModelEntry`・`ProviderKind`・`UsageLimitsConfig`・`AutoConfig`）の
/// serde でそのまま読み、プロバイダの `backend` だけを gateway の種別名として読む（評価は anthropic と stub だけ）。
#[derive(Deserialize)]
struct TomlLlm {
    backend: ProviderKind,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    default_model: String,
    #[serde(default)]
    auto: Option<AutoConfig>,
    #[serde(default)]
    providers: Vec<TomlProvider>,
    models: Vec<ModelEntry>,
    #[serde(default)]
    limits: UsageLimitsConfig,
}

#[derive(Deserialize)]
struct TomlProvider {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
    backend: ProviderKind,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
}

/// TOML から gateway の設定を組み、判定用のモデル `model` の出し方を段に合わせる。
///
/// 利用上限（`[llm.limits]`）が無効なら止める（評価は本番と同じ枠の検査を通す）。
pub(crate) fn gateway_config(
    path: &Path,
    model: &str,
    effort: Effort,
    samples: u8,
) -> GatewayConfig {
    use figment::providers::{Format, Toml};
    assert!(path.exists(), "設定が無い: {}", path.display());
    let llm: TomlLlm = figment::Figment::from(Toml::file_exact(path))
        .extract_inner("llm")
        .unwrap_or_else(|e| panic!("{} の [llm] を読めない: {e}", path.display()));
    assert!(
        llm.limits.enabled && !llm.limits.is_noop(),
        "{} の [llm.limits] が無効（評価は枠の検査を有効にして走る）",
        path.display()
    );
    let mut models = llm.models;
    let entry = models
        .iter_mut()
        .find(|m| m.id == model)
        .unwrap_or_else(|| panic!("判定用のモデル {model} が設定に無い"));
    assert!(entry.is_decision(), "{model} は判定用のモデルではない");
    entry.decision = Some(DecisionTuning { samples, effort });
    GatewayConfig {
        provider: ProviderConfig {
            id: DEFAULT_PROVIDER_ID.to_string(),
            kind: llm.backend,
            base_url: llm.base_url,
            api_key: llm.api_key,
            ..Default::default()
        },
        extra_providers: llm
            .providers
            .into_iter()
            .map(|p| ProviderConfig {
                id: p.id,
                display_name: p.display_name,
                kind: p.backend,
                base_url: p.base_url,
                api_key: p.api_key.filter(|k| !k.is_empty()),
                ..Default::default()
            })
            .collect(),
        catalog: ModelCatalog {
            default_model: llm.default_model,
            models,
        },
        auto: llm.auto.unwrap_or_default(),
        limits: llm.limits,
        ..Default::default()
    }
}

/// リポジトリの migrations の最大の番号。
fn latest_migration() -> i64 {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{} を読めない: {e}", dir.display()))
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<i64>().ok()
        })
        .max()
        .unwrap_or(0)
}

/// DB がこのブランチの migrations まで当たっていて、会計と組織の AI 設定の表があるか（マイグレーションは
/// 流さない・合わなければ止める）。
pub(crate) async fn check_db(pool: &PgPool) {
    let want = latest_migration();
    let (got, failed): (Option<i64>, i64) = sqlx::query_as(
        "SELECT max(version), count(*) FILTER (WHERE NOT success) FROM _sqlx_migrations",
    )
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| {
        panic!("_sqlx_migrations を読めない（マイグレーションの済んだ DB を指す）: {e}")
    });
    assert_eq!(
        got,
        Some(want),
        "DB のマイグレーションの最大の番号がリポジトリ（{want}）と合わない（サーバを起動して当ててから流す）"
    );
    assert_eq!(failed, 0, "失敗したマイグレーションがある");
    for table in ["llm_usage", "tenant_llm_settings", "tenant_llm_model"] {
        let (found,): (Option<String>,) = sqlx::query_as("SELECT to_regclass($1)::text")
            .bind(table)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("{table} を確かめられない: {e}"));
        assert!(found.is_some(), "表 {table} が無い");
    }
}

/// 中継（`anthropic-relay.py`）の累計額と上限。
#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct RelaySpend {
    pub(crate) total_usd: f64,
    pub(crate) cap_usd: f64,
}

pub(crate) async fn relay_spend(http: &reqwest::Client, base: &str) -> RelaySpend {
    let url = format!("{}/spend", base.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .send()
        .await
        .unwrap_or_else(|e| panic!("中継の累計額を読めない（{url}）: {e}"));
    resp.json()
        .await
        .unwrap_or_else(|e| panic!("中継の累計額の形が違う（{url}）: {e}"))
}

/// 会計（llm_usage）に載ったこの実行の分（行数・額）。
pub(crate) async fn usage_of_run(pool: &PgPool, tenant: &str, key_prefix: &str) -> (i64, i64) {
    sqlx::query_as(
        "SELECT count(*), COALESCE(sum(cost_usd_micros), 0)::bigint FROM llm_usage \
         WHERE tenant_id = $1 AND starts_with(idempotency_key, $2)",
    )
    .bind(tenant)
    .bind(key_prefix)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("会計を読めない: {e}"))
}
