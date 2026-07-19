//! Markdown → .docx 合成（#332・下書き確定型の「ドライブに保存」経路）。
//!
//! 埋め込み blank.docx テンプレに ingestion-worker `/edit` の `append_markdown`
//! （`office.edit` と同経路・python-docx）で本文を書き込み、bytes を返す。
//! 保存はしない（保存先の認可・監査は呼び出し側の StorageService チョークポイントが担う）。
//! worker はストレージへ一切アクセスしない（bytes は本プロセスが運ぶ・edit.rs と同契約）。

use base64::Engine as _;

use crate::edit::{map_worker_error, WorkerEditResponse};
use crate::error::OfficeError;

/// Word 文書（.docx）の content_type（[`crate::EDITABLE_CONTENT_TYPES`] の先頭と同値）。
pub const DOCX_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// 空の Word 文書テンプレ（ドライブ「新規作成 > ドキュメント」と同一の正本）。
const BLANK_DOCX: &[u8] = include_bytes!("../templates/blank.docx");

/// Markdown から .docx bytes を合成する（保存しない・Collabora 非依存・worker のみ必要）。
pub struct DocxComposer {
    http: reqwest::Client,
    worker_base_url: String,
}

impl DocxComposer {
    pub fn new(http: reqwest::Client, worker_base_url: &str) -> Self {
        DocxComposer {
            http,
            worker_base_url: worker_base_url.trim_end_matches('/').to_string(),
        }
    }

    /// blank.docx テンプレ＋`append_markdown` で .docx bytes を合成する。
    ///
    /// `markdown` が空白のみなら worker を呼ばずテンプレをそのまま返す（空ドキュメントの
    /// 新規作成は worker 非稼働環境でも成立させる・#333 の即作成が依存）。
    pub async fn compose(
        &self,
        tenant_id: &str,
        file_name: &str,
        markdown: &str,
    ) -> Result<Vec<u8>, OfficeError> {
        if markdown.trim().is_empty() {
            return Ok(BLANK_DOCX.to_vec());
        }
        let encoded = base64::engine::general_purpose::STANDARD.encode(BLANK_DOCX);
        let resp = self
            .http
            .post(format!("{}/edit", self.worker_base_url))
            .json(&serde_json::json!({
                "tenant_id": tenant_id,
                "content_type": DOCX_CONTENT_TYPE,
                "file_name": file_name,
                "data_base64": encoded,
                "ops": [{ "op": "append_markdown", "markdown": markdown }],
            }))
            .send()
            .await
            .map_err(|e| OfficeError::Worker(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(map_worker_error(resp).await);
        }
        let body: WorkerEditResponse = resp
            .json()
            .await
            .map_err(|e| OfficeError::Worker(format!("worker 応答の解釈に失敗: {e}")))?;
        base64::engine::general_purpose::STANDARD
            .decode(body.data_base64.as_bytes())
            .map_err(|e| OfficeError::Worker(format!("worker 応答の base64 が不正: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空 markdown はテンプレ直返し（worker 不要）。返る bytes は zip（PK）で始まる docx。
    #[tokio::test]
    async fn blank_markdown_returns_template_without_worker() {
        // 到達不能な worker URL でも空 markdown なら成功する（呼ばない）ことを検証する。
        let composer = DocxComposer::new(reqwest::Client::new(), "http://127.0.0.1:1");
        let bytes = composer
            .compose("default", "x.docx", "   \n")
            .await
            .unwrap();
        assert_eq!(&bytes[..2], b"PK", "docx（zip）ヘッダで始まる");
        assert_eq!(bytes, BLANK_DOCX);
    }

    /// 非空 markdown は worker が要る（接続不能は Worker エラーへ写す）。
    #[tokio::test]
    async fn nonempty_markdown_requires_worker() {
        let composer = DocxComposer::new(reqwest::Client::new(), "http://127.0.0.1:1");
        let err = composer
            .compose("default", "x.docx", "# 見出し")
            .await
            .unwrap_err();
        assert!(matches!(err, OfficeError::Worker(_)));
    }
}
