//! AI スライド共同編集ツール（slide.read / slide.edit・Task 11.3・design §4.8.3）。
//!
//! エージェントは**共同編集参加者**としてスライド（Yjs）を編集する。人間と同じ
//! `editor@file` 権限で判定し（confused-deputy 回避・昇格しない）、編集は共有 Yjs
//! ドキュメントへ適用されて人間の並行編集と収束する（排他なし）。HTML 入力は
//! collab 側適用時に必ずサニタイズされる（PIT-40 第1層）。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use collab::CollabHub;
use serde::Deserialize;
use storage::StorageService;
use uuid::Uuid;

/// collab のエラーを**モデルが読む error 観測**へ写す（fail-closed・存在秘匿）。
fn denied_outcome(err: &collab::CollabError) -> ToolOutcome {
    use collab::CollabError as CE;
    let msg = match err {
        CE::Forbidden(_) | CE::Authz(_) | CE::Storage(storage::StorageError::Forbidden) => {
            "このスライドを編集する権限がありません（editor 権限が必要です）。"
        }
        CE::NotFound(_) | CE::Storage(storage::StorageError::NotFound) => {
            "指定されたスライドが見つかりません。"
        }
        _ => "スライド編集に失敗しました。",
    };
    ToolOutcome::error(msg)
}

/// スライドの現在内容（正規化 JSON）を読むツール（編集前の把握に使う）。
pub struct SlideReadTool {
    collab: Arc<CollabHub>,
    storage: Arc<StorageService>,
}

impl SlideReadTool {
    pub fn new(collab: Arc<CollabHub>, storage: Arc<StorageService>) -> Self {
        SlideReadTool { collab, storage }
    }
}

#[derive(Debug, Deserialize)]
struct ReadInput {
    /// 対象スライド（.slide ファイル）の ID。
    node_id: Uuid,
}

#[async_trait::async_trait]
impl Tool for SlideReadTool {
    fn name(&self) -> &str {
        ToolName::SlideRead.as_str()
    }
    fn description(&self) -> &'static str {
        "スライド（.slide ファイル）の現在の内容を正規化 JSON で読み取る（各スライドの\
         id・本文 HTML・スピーカーノート・メタデータ）。slide.edit で編集する前に、\
         スライド構成と id を把握するために使う。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "スライドのノード ID" }
            },
            "required": ["node_id"]
        })
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: ReadInput = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        let node = match self
            .storage
            .get_metadata(ctx, input.node_id, trace_id)
            .await
        {
            Ok(node) => node,
            Err(e) => return Ok(denied_outcome(&collab::CollabError::Storage(e))),
        };
        match self.collab.read_slide_json(ctx, &node).await {
            Ok(json) => Ok(ToolOutcome::ok(format!(
                "# スライド「{}」の現在の内容（正規化 JSON）\n\n```json\n{json}```",
                node.name
            ))),
            Err(e) => Ok(denied_outcome(&e)),
        }
    }
}

/// スライドを共同編集参加者として編集するツール。
pub struct SlideEditTool {
    collab: Arc<CollabHub>,
    storage: Arc<StorageService>,
}

impl SlideEditTool {
    pub fn new(collab: Arc<CollabHub>, storage: Arc<StorageService>) -> Self {
        SlideEditTool { collab, storage }
    }
}

#[derive(Debug, Deserialize)]
struct EditInput {
    /// 対象スライド（.slide ファイル）の ID。
    node_id: Uuid,
    /// 編集操作列（順に適用）。
    ops: Vec<collab::slide::SlideEditOp>,
}

#[async_trait::async_trait]
impl Tool for SlideEditTool {
    fn name(&self) -> &str {
        ToolName::SlideEdit.as_str()
    }
    fn description(&self) -> &'static str {
        "スライド（.slide ファイル）を共同編集参加者として編集する。人間が編集中でも安全に\
         同時編集できる（CRDT で収束）。操作: append_slide（末尾に追加）/ insert_slide_after\
         （指定 id の直後に挿入）/ replace_slide（本文 HTML 置換）/ remove_slide / set_notes\
         （スピーカーノート）/ set_background（{\"color\":\"#rrggbb\"}）/ set_meta（title・\
         theme_id 等）。本文 HTML は h1〜h3/p/ul/li/table/div(インライン style) 等の基本要素で\
         構成する（script 等は自動除去される）。編集前に slide.read で構成と id を確認すること。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string", "format": "uuid", "description": "スライドのノード ID" },
                "ops": {
                    "type": "array",
                    "description": "編集操作列（順に適用）",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": {
                                "type": "string",
                                "enum": ["append_slide", "insert_slide_after", "replace_slide",
                                          "remove_slide", "set_notes", "set_background", "set_meta"]
                            },
                            "slide_id": { "type": "string", "description": "対象スライド id（append_slide/set_meta 以外で必須）" },
                            "html": { "type": "string", "description": "スライド本文 HTML（append_slide/insert_slide_after/replace_slide）" },
                            "notes": { "type": "string", "description": "スピーカーノート（set_notes・追加系では任意）" },
                            "bg": { "type": "object", "description": "背景指定（set_background・例 {\"color\":\"#ffffff\"}）" },
                            "key": { "type": "string", "description": "プロパティ名（set_meta・title/theme_id/tags/任意）" },
                            "value": { "type": "string", "description": "プロパティ値（set_meta）" }
                        },
                        "required": ["op"]
                    }
                }
            },
            "required": ["node_id", "ops"]
        })
    }

    /// 破壊的（既存スライドを書き換え得る）ため確認対象。承認ゲートの対象になる。
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
        let node = match self
            .storage
            .get_metadata(ctx, input.node_id, trace_id)
            .await
        {
            Ok(node) => node,
            Err(e) => return Ok(denied_outcome(&collab::CollabError::Storage(e))),
        };
        let report = match self
            .collab
            .apply_ai_slide_edit(ctx, &node, &input.ops)
            .await
        {
            Ok(report) => report,
            Err(e) => return Ok(denied_outcome(&e)),
        };

        use std::fmt::Write as _;
        let mut content = format!(
            "スライド「{}」を編集しました（{} 件適用）。",
            node.name, report.applied
        );
        if !report.skipped.is_empty() {
            let _ = write!(
                content,
                "\n次の操作は対象が見つからずスキップしました: {}",
                report.skipped.join(", ")
            );
        }
        let outcome = if report.applied == 0 {
            ToolOutcome::error(content)
        } else {
            ToolOutcome::ok(content)
        };
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    //! error 写像（`denied_outcome`）と入力デシリアライズの純関数部分を検証する。
    //! hub 経由の適用・認可は crates/collab/tests/slide_ai_edit_it.rs が担う。
    use super::*;

    #[test]
    fn denied_outcomeは権限と存在を秘匿して畳む() {
        use collab::CollabError as CE;
        assert!(denied_outcome(&CE::Forbidden("x".into()))
            .content
            .contains("権限がありません"));
        assert!(
            denied_outcome(&CE::Storage(storage::StorageError::Forbidden))
                .content
                .contains("権限がありません")
        );
        assert!(denied_outcome(&CE::NotFound("x".into()))
            .content
            .contains("見つかりません"));
        assert!(denied_outcome(&CE::InvalidUpdate("x".into()))
            .content
            .contains("失敗しました"));
        assert!(denied_outcome(&CE::Forbidden("x".into())).is_error);
    }

    #[test]
    fn edit入力はopsのタグ付きenumでデシリアライズできる() {
        let input: EditInput = serde_json::from_value(serde_json::json!({
            "node_id": "0190f9a0-0000-7000-8000-000000000000",
            "ops": [
                { "op": "append_slide", "html": "<h2>追加</h2>", "notes": "メモ" },
                { "op": "replace_slide", "slide_id": "s1", "html": "<h1>置換</h1>" },
                { "op": "set_meta", "key": "title", "value": "提案書" }
            ]
        }))
        .expect("deserialize");
        assert_eq!(input.ops.len(), 3);
        assert!(matches!(
            input.ops[0],
            collab::slide::SlideEditOp::AppendSlide { .. }
        ));
        assert!(matches!(
            input.ops[1],
            collab::slide::SlideEditOp::ReplaceSlide { .. }
        ));
    }
}
