//! Office ファイル（docx/xlsx/pptx）の AI 編集ツール（Task 11.8・`office.edit`）。
//!
//! 実体は [`office::OfficeEditor`]（bytes 取得→worker `/edit`→WOPI ロックで保存分岐）。
//! 実行主体は発話ユーザーの `AuthContext`（昇格しない）。認可エラー・存在なしは
//! 存在秘匿の観測メッセージに畳む（slide_tool と同じ流儀）。
//!
//! 読み取りは既存の RAG（doc_search・Docling パース済み本文）で足りるため
//! `office.read` は新設しない（docs/design.md §4.8 ①「read=Docling パースを正」）。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use office::{OfficeEditor, OfficeError, SavedEdit};
use serde::Deserialize;
use uuid::Uuid;

/// Office ファイルを編集するツール（要確認・破壊的）。
pub struct OfficeEditTool {
    editor: Arc<OfficeEditor>,
}

impl OfficeEditTool {
    pub fn new(editor: Arc<OfficeEditor>) -> Self {
        OfficeEditTool { editor }
    }
}

#[derive(Debug, Deserialize)]
struct EditInput {
    /// 対象ファイル（docx/xlsx/pptx）のノード ID。
    node_id: Uuid,
    /// 編集操作列（順に適用・種別ごとのクローズド集合。worker 側 pydantic が検証する）。
    ops: Vec<serde_json::Value>,
}

/// 認可・存在エラーを存在秘匿の観測メッセージへ畳む（オラクル防止）。
fn denied_outcome(err: &OfficeError) -> ToolOutcome {
    let message = match err {
        OfficeError::NotFound | OfficeError::Forbidden | OfficeError::Unauthorized => {
            "対象が見つからないか、アクセスできません。".to_string()
        }
        other => format!("編集に失敗しました: {other}"),
    };
    ToolOutcome::error(message)
}

#[async_trait::async_trait]
impl Tool for OfficeEditTool {
    fn name(&self) -> &str {
        ToolName::OfficeEdit.as_str()
    }
    fn description(&self) -> &'static str {
        "Office ファイル（docx/xlsx/pptx）を編集する。ops はファイル種別ごとに: \
         docx = replace_text{find,replace} / append_markdown{markdown} / \
         insert_after_heading{heading,markdown}（markdown は # 見出し・- 箇条書き・段落の最小記法）、\
         xlsx = set_cells{sheet,cells:{\"A1\":値}} / insert_rows{sheet,at,rows:[[..]]} / \
         add_sheet{name}、pptx = replace_text{find,replace} / add_slide{title,bullets:[..]} / \
         remove_slide{index}。人間が編集セッション中（WOPI ロック中）の場合は上書きせず\
         「提案バージョン」として保存され、editor がバージョン履歴から採用すると反映される。\
         適用結果（各 op の適用数と warning）が返るので、0 件のときは対象指定を見直すこと。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "対象ファイルのノード ID" },
                "ops": {
                    "type": "array",
                    "description": "編集操作列（順に適用）。op ごとの必須項目は description を参照",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "enum": ["replace_text", "append_markdown", "insert_after_heading",
                                          "set_cells", "insert_rows", "add_sheet",
                                          "add_slide", "remove_slide"]
                            }
                        },
                        "required": ["op"]
                    }
                }
            },
            "required": ["node_id", "ops"]
        })
    }

    /// ファイル内容を書き換える破壊的ツールのため確認対象（承認ゲートの対象）。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: EditInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        if input.ops.is_empty() {
            return Err(ToolError::Invalid("ops が空です".into()));
        }
        let outcome = match self
            .editor
            .edit_file(ctx, input.node_id, &input.ops, trace_id)
            .await
        {
            Ok(outcome) => outcome,
            Err(e @ (OfficeError::Invalid(_) | OfficeError::Worker(_))) => {
                // 恒久（ops 不正・非対応種別）も一時（worker 不達）もモデルへ観測として返す。
                return Ok(ToolOutcome::error(format!("編集に失敗しました: {e}")));
            }
            Err(e) => return Ok(denied_outcome(&e)),
        };

        use std::fmt::Write as _;
        let mut content = match &outcome.saved {
            Some(SavedEdit::NewVersion { version }) => format!(
                "「{}」を編集し、新バージョン v{version} として保存しました。",
                outcome.file_name
            ),
            Some(SavedEdit::Proposal { version }) => format!(
                "「{}」は人間が編集セッション中のため、上書きせず提案バージョン v{version} として\
                 保存しました。編集者がバージョン履歴から採用すると反映されます。",
                outcome.file_name
            ),
            None => format!(
                "「{}」への編集は 1 件も適用されませんでした（保存なし）。対象指定を見直してください。",
                outcome.file_name
            ),
        };
        for r in &outcome.report.results {
            let _ = write!(content, "\n- {}: {} 件適用", r.op, r.applied);
            if let Some(warning) = &r.warning {
                let _ = write!(content, "（{warning}）");
            }
        }
        if outcome.saved.is_none() {
            return Ok(ToolOutcome::error(content));
        }
        Ok(ToolOutcome::ok(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_requires_node_id_and_ops() {
        let err = serde_json::from_value::<EditInput>(serde_json::json!({ "ops": [] }))
            .expect_err("node_id 必須");
        assert!(err.to_string().contains("node_id"));
        let ok: EditInput = serde_json::from_value(serde_json::json!({
            "node_id": "8c4a1d18-1111-2222-3333-444444444444",
            "ops": [{ "op": "replace_text", "find": "a", "replace": "b" }]
        }))
        .expect("正常入力");
        assert_eq!(ok.ops.len(), 1);
    }

    #[test]
    fn denied_outcome_conceals_existence() {
        for err in [
            OfficeError::NotFound,
            OfficeError::Forbidden,
            OfficeError::Unauthorized,
        ] {
            let outcome = denied_outcome(&err);
            assert!(outcome.is_error);
            assert!(outcome.content.contains("見つからないか"));
        }
    }
}
