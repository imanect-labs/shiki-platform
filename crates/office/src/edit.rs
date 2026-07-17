//! AI Office 編集（Task 11.8・docs/design.md §4.8「AI の読み書き」②③）。
//!
//! フロー: StorageService から bytes 取得（単一チョークポイント・直バケット禁止）→
//! ingestion-worker `/edit`（python-docx/openpyxl/python-pptx・ステートレス bytes 入出力）→
//! **WOPI ロックで保存先を分岐**:
//! - 非ロック（編集セッション無し）→ `update_file_content_internal` で通常の新バージョン
//!   （版・監査・書込イベント outbox → RAG 再索引の既存経路）。
//! - ロック中（人間が Collabora で編集中）→ `propose_file_content_internal` で
//!   **提案バージョン**（current を進めない・outbox 無し・PIT-44）。
//!
//! worker はストレージへ一切アクセスしない（bytes は本プロセスが運ぶ）。ops の検証は
//! worker 側 pydantic が正（未知 op・型不一致は 422 → [`OfficeError::Invalid`]）。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use storage::StorageService;
use uuid::Uuid;

use crate::error::OfficeError;
use crate::wopi::lock;

/// AI 編集が対応する content_type（worker `/edit` のクローズド集合と対）。
/// ODF（odt/ods/odp）は Collabora 閲覧・編集のみで AI 編集は未対応。
pub const EDITABLE_CONTENT_TYPES: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
];

/// worker `/edit` の op 単位の適用結果。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EditOpResult {
    pub op: String,
    /// 適用件数（置換数・挿入ブロック数など op 固有）。0 は対象不一致（warning 参照）。
    pub applied: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// worker `/edit` の適用レポート（モデルへそのまま観測として返す）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EditReport {
    /// 1 件以上適用できた op の数。
    pub applied_ops: u32,
    pub results: Vec<EditOpResult>,
}

/// 保存結果（適用 0 件なら保存しない＝None）。
#[derive(Debug, Clone)]
pub enum SavedEdit {
    /// 非ロック時: 通常の新バージョンとして保存（RAG 再索引が流れる）。
    NewVersion { version: i64 },
    /// WOPI ロック中: 提案バージョンとして保存（editor の採用待ち・PIT-44）。
    Proposal { version: i64 },
}

/// AI Office 編集の結果。
#[derive(Debug)]
pub struct EditOutcome {
    pub file_name: String,
    pub report: EditReport,
    pub saved: Option<SavedEdit>,
}

#[derive(Deserialize)]
struct WorkerEditResponse {
    data_base64: String,
    report: EditReport,
}

/// worker の 422 構造化エラー（`{"detail": {"error", "detail"}}`・parse と同形）。
#[derive(Deserialize)]
struct WorkerErrorBody {
    detail: WorkerErrorDetail,
}

#[derive(Deserialize)]
struct WorkerErrorDetail {
    error: String,
    detail: String,
}

/// AI Office 編集の実行体（chat ツール `office.edit` から呼ばれる）。
pub struct OfficeEditor {
    http: reqwest::Client,
    worker_base_url: String,
    storage: Arc<StorageService>,
    authz: Arc<dyn AuthzClient>,
    /// `office_lock` の参照用（WOPI ロック＝編集セッション判定）。
    pool: PgPool,
}

impl OfficeEditor {
    pub fn new(
        http: reqwest::Client,
        worker_base_url: &str,
        storage: Arc<StorageService>,
        authz: Arc<dyn AuthzClient>,
        pool: PgPool,
    ) -> Self {
        OfficeEditor {
            http,
            worker_base_url: worker_base_url.trim_end_matches('/').to_string(),
            storage,
            authz,
            pool,
        }
    }

    /// ops を適用し、ロック状態に応じて新バージョン or 提案バージョンとして保存する。
    ///
    /// 認可: 実行主体（発話ユーザー）の `editor@file` を**先に**確認する（fail-fast・
    /// viewer の bytes を worker へ運ばない）。保存経路（update/propose）でも同じ
    /// relation を再チェックする（チョークポイント側の防衛は残す）。
    pub async fn edit_file(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        ops: &[serde_json::Value],
        trace_id: Option<&str>,
    ) -> Result<EditOutcome, OfficeError> {
        let object = ctx.ns().file(&file_id.to_string());
        let allowed = self
            .authz
            .check(
                &ctx.subject(),
                Relation::Editor,
                &object,
                Consistency::HigherConsistency,
            )
            .await?;
        if !allowed {
            // 読めるかどうかも明かさない（存在秘匿・ツール層で観測メッセージに畳む）。
            return Err(OfficeError::NotFound);
        }

        let (node, bytes) = self
            .storage
            .read_file_internal(ctx, file_id, trace_id)
            .await?;
        let content_type = node.content_type.clone().unwrap_or_default();
        if !EDITABLE_CONTENT_TYPES.contains(&content_type.as_str()) {
            return Err(OfficeError::Invalid(format!(
                "AI 編集非対応のファイル種別です: {content_type}（対応: docx/xlsx/pptx）"
            )));
        }

        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let resp = self
            .http
            .post(format!("{}/edit", self.worker_base_url))
            .json(&serde_json::json!({
                "tenant_id": ctx.tenant_id,
                "content_type": content_type,
                "file_name": node.name,
                "data_base64": encoded,
                "ops": ops,
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

        // 適用 0 件なら版を作らない（無内容のバージョンでノイズを増やさない）。
        if body.report.applied_ops == 0 {
            return Ok(EditOutcome {
                file_name: node.name,
                report: body.report,
                saved: None,
            });
        }
        let edited = base64::engine::general_purpose::STANDARD
            .decode(body.data_base64.as_bytes())
            .map_err(|e| OfficeError::Worker(format!("worker 応答の base64 が不正: {e}")))?;

        // WOPI ロック（＝人間の編集セッション）で保存先を分岐する（PIT-44）。
        let saved = match lock::current_lock(&self.pool, &ctx.tenant_id, file_id).await? {
            Some(_) => {
                let proposal = self
                    .storage
                    .propose_file_content_internal(ctx, file_id, &edited, &content_type, trace_id)
                    .await?;
                SavedEdit::Proposal {
                    version: proposal.version,
                }
            }
            None => {
                let updated = self
                    .storage
                    .update_file_content_internal(ctx, file_id, &edited, &content_type, trace_id)
                    .await?;
                SavedEdit::NewVersion {
                    version: updated.version,
                }
            }
        };
        Ok(EditOutcome {
            file_name: node.name,
            report: body.report,
            saved: Some(saved),
        })
    }
}

/// worker のエラー応答を [`OfficeError`] に写す（422=恒久 → Invalid / その他 → Worker）。
async fn map_worker_error(resp: reqwest::Response) -> OfficeError {
    let status = resp.status();
    if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        return match resp.json::<WorkerErrorBody>().await {
            Ok(body) => {
                OfficeError::Invalid(format!("{}: {}", body.detail.error, body.detail.detail))
            }
            Err(_) => OfficeError::Invalid("worker が編集要求を拒否しました（422）".into()),
        };
    }
    let detail = resp.text().await.unwrap_or_default();
    OfficeError::Worker(format!("HTTP {status}: {detail}"))
}
