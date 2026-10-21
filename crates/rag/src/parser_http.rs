//! ingestion-worker `/parse` を呼ぶ `DocumentParser` 実装。

use async_trait::async_trait;
use authz::AuthContext;
use base64::Engine as _;
use serde::Deserialize;

use crate::error::RagError;
use crate::parser::{DocumentParser, ParseRequest, ParseSource};
use crate::types::ParsedDocument;

pub struct HttpDocumentParser {
    http: reqwest::Client,
    base_url: String,
}

impl HttpDocumentParser {
    pub fn new(http: reqwest::Client, base_url: &str) -> Self {
        HttpDocumentParser {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

/// worker の 422 構造化エラー（`{"detail": {"error", "detail"}}`）。
#[derive(Deserialize)]
struct WorkerErrorBody {
    detail: WorkerErrorDetail,
}

#[derive(Deserialize)]
struct WorkerErrorDetail {
    error: String,
    detail: String,
}

/// worker のエラー応答を [`RagError`] に写す共通処理。
///
/// 422 = パース失敗等の**恒久エラー**（リトライしない）、それ以外の非 2xx は一時エラー。
pub(crate) async fn map_worker_error(resp: reqwest::Response) -> RagError {
    let status = resp.status();
    if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        match resp.json::<WorkerErrorBody>().await {
            Ok(body) => RagError::Parse {
                code: body.detail.error,
                detail: body.detail.detail,
            },
            Err(_) => RagError::Parse {
                code: "unprocessable".into(),
                detail: "worker が 422 を返しました（詳細不明）".into(),
            },
        }
    } else {
        let detail = resp.text().await.unwrap_or_default();
        RagError::Worker(format!("HTTP {status}: {detail}"))
    }
}

#[async_trait]
impl DocumentParser for HttpDocumentParser {
    async fn parse(
        &self,
        ctx: &AuthContext,
        req: ParseRequest<'_>,
    ) -> Result<ParsedDocument, RagError> {
        let mut body = serde_json::json!({
            "tenant_id": ctx.tenant_id,
            "content_type": req.content_type,
            "file_name": req.file_name,
        });
        // worker 側は source_url / content_base64 の**排他**を検証する（両方/どちらも無しは 422）。
        let (key, value) = match req.source {
            ParseSource::Url(url) => ("source_url", url.to_string()),
            ParseSource::Bytes(bytes) => (
                "content_base64",
                base64::engine::general_purpose::STANDARD.encode(bytes),
            ),
        };
        if let Some(obj) = body.as_object_mut() {
            obj.insert(key.to_string(), serde_json::Value::String(value));
        }
        let resp = self
            .http
            .post(format!("{}/parse", self.base_url))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(map_worker_error(resp).await);
        }
        Ok(resp.json::<ParsedDocument>().await?)
    }
}
