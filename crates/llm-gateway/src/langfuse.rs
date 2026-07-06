//! Langfuse（self-host）計装（best-effort）。計装の正本は Rust の gateway 層（#97）。
//!
//! LLM 呼び出しごとに Langfuse へ 1 generation（trace 配下）を送る。OTel の trace_id を
//! Langfuse trace id に使い、**監査ログ↔Langfuse↔OTel を同一 trace_id で突合**できるようにする。
//! 送信は best-effort（失敗は warn ログのみ・生成本体を止めない）。未設定なら no-op。

use serde_json::{json, Value};

use crate::config::LangfuseConfig;
use crate::model::Usage;

/// Langfuse ingestion クライアント。
#[derive(Clone)]
pub struct LangfuseClient {
    http: reqwest::Client,
    base_url: String,
    public_key: String,
    secret_key: String,
}

/// 1 generation の計装ペイロード。
pub struct GenerationTrace<'a> {
    pub trace_id: &'a str,
    pub name: &'a str,
    pub model: &'a str,
    pub provider: &'a str,
    pub input: &'a str,
    pub output: &'a str,
    pub usage: Usage,
    /// tenant/user 別コスト可視化のためのメタ。
    pub metadata: Value,
}

impl LangfuseClient {
    pub fn new(http: reqwest::Client, cfg: &LangfuseConfig) -> Self {
        LangfuseClient {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            public_key: cfg.public_key.clone(),
            secret_key: cfg.secret_key.clone(),
        }
    }

    /// 1 generation を trace 配下に送る（best-effort・非同期・エラーは warn）。
    pub async fn record_generation(&self, t: GenerationTrace<'_>) {
        let now = chrono::Utc::now().to_rfc3339();
        // trace-create（id = OTel trace_id）＋ generation-create のバッチ。
        let batch = json!({
            "batch": [
                {
                    "id": format!("{}-trace", t.trace_id),
                    "type": "trace-create",
                    "timestamp": now,
                    "body": {
                        "id": t.trace_id,
                        "name": t.name,
                        "metadata": t.metadata,
                    }
                },
                {
                    "id": format!("{}-gen", t.trace_id),
                    "type": "generation-create",
                    "timestamp": now,
                    "body": {
                        "id": format!("{}-gen", t.trace_id),
                        "traceId": t.trace_id,
                        "name": t.name,
                        "model": t.model,
                        "input": t.input,
                        "output": t.output,
                        "usage": {
                            "input": t.usage.prompt_tokens,
                            "output": t.usage.completion_tokens,
                            "unit": "TOKENS",
                        },
                        "metadata": { "provider": t.provider },
                    }
                }
            ]
        });
        let url = format!("{}/api/public/ingestion", self.base_url);
        let res = self
            .http
            .post(&url)
            .basic_auth(&self.public_key, Some(&self.secret_key))
            .json(&batch)
            .send()
            .await;
        match res {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => tracing::warn!(status = %r.status(), "langfuse ingestion non-success"),
            Err(e) => tracing::warn!(error = %e, "langfuse ingestion failed"),
        }
    }
}
