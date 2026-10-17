//! Excel をレイアウトまで含めて読み書きするツール（office.read / office.excel_edit・#725）。
//!
//! 実体は [`office::engine::OfficeEngine`]。AI は Collabora セッションの headless 参加者として
//! 接続し、kit 内の固定エンジンへ検証済みの JSON を渡す（LLM のコードは kit に入れない・PIT-68）。
//! 権限は毎回 OpenFGA で再判定され（読み取り=viewer・編集=editor）、編集は全部成功か全部取り消し。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use office::engine::{OfficeEngineError, SheetEditReport, SheetReadRequest};
use office::live::{LiveEditError, LiveSaveResult};
use serde::Deserialize;
use uuid::Uuid;

/// テスト差し替え用の薄い境界（本体は [`office::engine::OfficeEngine`]）。
#[async_trait::async_trait]
pub(crate) trait SheetEngine: Send + Sync {
    async fn read_sheet(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        req: &SheetReadRequest,
    ) -> Result<String, OfficeEngineError>;

    async fn edit_sheet(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        label: &str,
        default_sheet: Option<&str>,
        ops: &[serde_json::Value],
    ) -> Result<SheetEditReport, OfficeEngineError>;
}

#[async_trait::async_trait]
impl SheetEngine for office::engine::OfficeEngine {
    async fn read_sheet(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        req: &SheetReadRequest,
    ) -> Result<String, OfficeEngineError> {
        office::engine::OfficeEngine::read_sheet(self, ctx, file_id, req).await
    }

    async fn edit_sheet(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        label: &str,
        default_sheet: Option<&str>,
        ops: &[serde_json::Value],
    ) -> Result<SheetEditReport, OfficeEngineError> {
        office::engine::OfficeEngine::edit_sheet(self, ctx, file_id, label, default_sheet, ops)
            .await
    }
}

/// エンジンの失敗をモデルへの観測に写す（存在秘匿・直し方つき）。
fn engine_error(e: OfficeEngineError) -> Result<ToolOutcome, ToolError> {
    Ok(ToolOutcome::error(match e {
        OfficeEngineError::Invalid(op) => format!("操作を実行していません（文書は変わっていません）。{op}"),
        OfficeEngineError::Unsupported => {
            "この環境の Office エンジンは古く、この道具は使えません。Excel の値の書き込みは office.live_edit の set_cells、\
             シートの追加などは office.edit を使ってください。".into()
        }
        OfficeEngineError::Range(r) => format!("{r}"),
        OfficeEngineError::Session(LiveEditError::Denied) => {
            "指定された文書にアクセスできません（存在しないか、権限がありません）。".into()
        }
        OfficeEngineError::Session(LiveEditError::Unsupported) => {
            "この道具は Excel（.xlsx / .ods）用です。Word は office.live_edit・doc_read を使ってください。".into()
        }
        OfficeEngineError::Session(LiveEditError::Busy) => {
            "この文書では別の AI 編集が進行中です。少し待ってから再実行してください。".into()
        }
        OfficeEngineError::Session(LiveEditError::Session(e)) => {
            format!("編集セッションを開始できませんでした（{e}）。しばらくして再実行してください。")
        }
        OfficeEngineError::Session(LiveEditError::Internal(e)) => return Err(ToolError::Internal(e)),
        OfficeEngineError::Engine(e) => {
            format!("文書エンジンが処理を完了できませんでした（{e}）。office.read で現在の状態を確かめてから、必要な分だけやり直してください。")
        }
    }))
}

/// Excel を読むツール。
pub struct OfficeReadTool {
    engine: Arc<dyn SheetEngine>,
}

impl OfficeReadTool {
    pub fn new(engine: Arc<office::engine::OfficeEngine>) -> Self {
        OfficeReadTool { engine }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    node_id: Uuid,
    #[serde(default)]
    range: Option<String>,
    #[serde(default)]
    sheet: Option<String>,
    #[serde(default)]
    details: bool,
    #[serde(default)]
    max_cells: Option<u32>,
}

#[async_trait::async_trait]
impl Tool for OfficeReadTool {
    fn name(&self) -> &str {
        ToolName::OfficeRead.as_str()
    }

    fn description(&self) -> &'static str {
        office::engine::calc::describe::READ_DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        office::engine::calc::describe::read_schema()
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: ReadInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let req = SheetReadRequest {
            range: input.range,
            sheet: input.sheet,
            max_cells: input.max_cells,
            details: input.details,
        };
        match self.engine.read_sheet(ctx, input.node_id, &req).await {
            Ok(text) => Ok(ToolOutcome::ok(text)),
            Err(e) => engine_error(e),
        }
    }
}

/// Excel を編集するツール。
pub struct OfficeExcelEditTool {
    engine: Arc<dyn SheetEngine>,
}

impl OfficeExcelEditTool {
    pub fn new(engine: Arc<office::engine::OfficeEngine>) -> Self {
        OfficeExcelEditTool { engine }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    node_id: Uuid,
    summary: String,
    #[serde(default)]
    sheet: Option<String>,
    ops: Vec<serde_json::Value>,
}

#[async_trait::async_trait]
impl Tool for OfficeExcelEditTool {
    fn name(&self) -> &str {
        ToolName::OfficeExcelEdit.as_str()
    }

    fn description(&self) -> &'static str {
        office::engine::calc::describe::EXCEL_EDIT_DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        office::engine::calc::describe::excel_edit_schema()
    }

    /// 文書を書き換えるため確認対象（承認ゲート・human-in-the-loop）。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: EditInput = match serde_json::from_value(input) {
            Ok(i) => i,
            // 語彙外の op・項目名の誤りはモデルが直せるよう観測として返す（文書には触れていない）。
            Err(e) => {
                return Ok(ToolOutcome::error(format!(
                    "入力を解釈できませんでした（文書は変わっていません）: {e}。op 名と項目名を説明文のとおりに直してください。"
                )))
            }
        };
        if input.ops.is_empty() {
            return Err(ToolError::Invalid("ops が空です".into()));
        }
        let summary: String = input.summary.chars().take(80).collect();
        let report = match self
            .engine
            .edit_sheet(
                ctx,
                input.node_id,
                &summary,
                input.sheet.as_deref(),
                &input.ops,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return engine_error(e),
        };
        let mut content = format!("「{}」: {}", report.file_name, report.text);
        let save_note = match &report.save {
            LiveSaveResult::Saved { version } => format!("保存: 新しい版 v{version}"),
            LiveSaveResult::Unverified => {
                "保存: 編集は文書に反映済み（保存の確認は取れていません。開いている人の保存か自動保存で確定します）".into()
            }
            LiveSaveResult::Failed(reason) => format!("保存: 失敗しました（{reason}）。編集は開いている文書には残っています"),
            LiveSaveResult::NotAttempted => "保存: 変更が無いため保存していません".into(),
        };
        content.push_str("\n\n");
        content.push_str(&save_note);
        if !report.applied {
            return Ok(ToolOutcome::error(content));
        }
        let mut outcome = ToolOutcome::ok(content);
        let version = match &report.save {
            LiveSaveResult::Saved { version } => Some(*version),
            _ => None,
        };
        outcome.document_refs.push(crate::document_ref::payload(
            input.node_id,
            &report.file_name,
            version,
        ));
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "office_engine_tool/tests.rs"]
mod tests;
