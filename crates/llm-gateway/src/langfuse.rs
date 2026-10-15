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
    /// **この 1 回の呼び出しを一意に指す id**（会計の冪等キーを流用する）。
    ///
    /// Langfuse の ingestion は id で upsert する。trace_id から機械的に作ると、
    /// **同一リクエスト内の 2 回目以降が 1 回目を上書きして消える**
    /// （実測で 67 呼び出しのうち 22 件が失われていた）。冪等キーは
    /// 呼び出しごとに一意で、かつ再試行では同じ値になるので、
    /// 取りこぼしも重複も同時に防げる。
    pub generation_id: &'a str,
    pub name: &'a str,
    pub model: &'a str,
    pub provider: &'a str,
    pub input: &'a str,
    pub output: &'a str,
    pub usage: Usage,
    /// trace をユーザー別に集計するための主体 id。
    pub user_id: &'a str,
    /// **この呼び出しがリクエスト内の最初か。**
    /// trace の input は最初の 1 回だけ載せる（upsert なので、毎回送ると
    /// 最後の呼び出しの入力で上書きされ、ユーザーの質問が消える）。
    pub first_in_trace: bool,
    /// 呼び出しの開始時刻。未指定なら所要時間が 0 と表示される。
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 呼び出しの終了時刻。**会計より前に確定させたものを渡すこと**
    /// （ここで now() を取ると、会計 DB の待ち時間が所要時間に混ざる）。
    pub ended_at: chrono::DateTime<chrono::Utc>,
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
        let now = t.ended_at.to_rfc3339();

        // trace の body。**upsert なので「毎回送ってよいもの」と
        // 「最初だけ送るもの」を分ける。**
        //   output … 毎回送る（最後の呼び出し＝最終的な応答が残る）
        //   input  … 最初だけ（毎回送るとユーザーの質問が上書きで消える）
        let mut trace_body = json!({
            "id": t.trace_id,
            "name": t.name,
            "userId": t.user_id,
            "output": t.output,
            "metadata": t.metadata,
        });
        if t.first_in_trace {
            trace_body["input"] = json!(t.input);
        }

        let mut gen_body = json!({
            // **trace_id から作らない。** 作ると 1 trace に 1 件しか残らない。
            "id": t.generation_id,
            "traceId": t.trace_id,
            "name": t.name,
            "model": t.model,
            "input": t.input,
            "output": t.output,
            "endTime": now,
            "usage": {
                "input": t.usage.prompt_tokens,
                "output": t.usage.completion_tokens,
                "unit": "TOKENS",
            },
            "metadata": { "provider": t.provider },
        });
        if let Some(started) = t.started_at {
            gen_body["startTime"] = json!(started.to_rfc3339());
        }

        // trace-create ＋ generation-create のバッチ。
        // **batch 要素の id はイベントの id で、body の id とは別物。**
        // 再送で重複しないよう、こちらも呼び出し単位で一意にする。
        let batch = json!({
            "batch": [
                {
                    "id": format!("{}-trace", t.generation_id),
                    "type": "trace-create",
                    "timestamp": now,
                    "body": trace_body,
                },
                {
                    "id": format!("{}-gen", t.generation_id),
                    "type": "generation-create",
                    "timestamp": now,
                    "body": gen_body,
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
