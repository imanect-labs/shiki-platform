//! ワークスペースの**書込系**ツール（Task 5.4/5.8）: `fs_write` / `fs_edit` / `fs_delete`。
//!
//! いずれも [`WorkspaceStore`] 経由で StorageService を叩き、権限・監査・書込イベント（→自動再索引）を
//! 必ず通す。**破壊的**（作成/上書き/削除）なので `requires_confirmation=true`（承認ゲート/事前許可対象・5.6）。
//! 書込結果は [`ArtifactRef`] として外部化し、UI がワークスペース上のファイルを開けるようにする。

use std::sync::Arc;

use authz::AuthContext;

use super::fs::parse_name;
use super::mime::content_type_for;
use crate::tool::{ArtifactRef, Tool, ToolError, ToolOutcome};
use crate::workspace::{WorkspaceStore, WorkspaceWrite};

/// 書込結果を tool_result（＋成果物）へ整形する共通ヘルパ。
fn write_outcome(w: WorkspaceWrite) -> ToolOutcome {
    let verb = if w.created { "作成" } else { "更新" };
    let mut out = ToolOutcome::ok(format!(
        "{}を{}しました（version {}, node_id: {}）。",
        w.name, verb, w.version, w.node_id
    ));
    out.artifacts = vec![ArtifactRef {
        node_id: w.node_id,
        name: w.name,
    }];
    out
}

/// `fs_write`: ファイルを作成 or 上書き（新版）する。
pub struct FsWriteTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl FsWriteTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        FsWriteTool { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for FsWriteTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "fs_write"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "ワークスペースにファイルを作成/上書きする。既存なら新しいバージョンとして保存される\
         （過去版は復元可能）。保存先はストレージで、書込は自動で再索引される。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "ファイル名" },
                "content": { "type": "string", "description": "ファイル全体の内容" }
            },
            "required": ["name", "content"],
            "additionalProperties": false
        })
    }
    // 上書き（破壊的）なので確認が要る（Task 3.9/5.6）。
    fn requires_confirmation(&self) -> bool {
        true
    }
    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let name = parse_name(&input)?;
        let content = input
            .get("content")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::Invalid("missing 'content'".into()))?;
        let ct = content_type_for(&name);
        let w = self
            .workspace
            .write(ctx, &name, content.as_bytes().to_vec(), ct, trace_id)
            .await?;
        Ok(write_outcome(w))
    }
}

/// `fs_edit`: 既存ファイル内の一意な文字列を置換して新版を書く。
pub struct FsEditTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl FsEditTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        FsEditTool { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for FsEditTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "fs_edit"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "既存ファイル内の `old_string` を `new_string` に置換して新版を保存する。\
         `old_string` はファイル内で一意でなければならない（曖昧なら失敗する）。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "ファイル名" },
                "old_string": { "type": "string", "description": "置換対象（ファイル内で一意）" },
                "new_string": { "type": "string", "description": "置換後の文字列" }
            },
            "required": ["name", "old_string", "new_string"],
            "additionalProperties": false
        })
    }
    fn requires_confirmation(&self) -> bool {
        true
    }
    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let name = parse_name(&input)?;
        let old = str_field(&input, "old_string")?;
        let new = str_field(&input, "new_string")?;

        let bytes = self.workspace.read(ctx, &name, trace_id).await?;
        // バイナリ（非 UTF-8）ファイルは lossy 変換で内容が壊れるため編集を拒否する（安全側）。
        let Ok(text) = String::from_utf8(bytes) else {
            return Ok(ToolOutcome::error(format!(
                "'{name}' はテキストファイルではないため fs_edit で編集できません。"
            )));
        };
        let count = text.matches(&old).count();
        if count == 0 {
            return Ok(ToolOutcome::error(format!(
                "'{name}' に old_string が見つかりません（置換していません）。"
            )));
        }
        if count > 1 {
            return Ok(ToolOutcome::error(format!(
                "old_string が {count} 箇所に一致します。一意になるよう前後を含めて指定してください。"
            )));
        }
        let updated = text.replacen(&old, &new, 1);
        let ct = content_type_for(&name);
        let w = self
            .workspace
            .write(ctx, &name, updated.into_bytes(), ct, trace_id)
            .await?;
        Ok(write_outcome(w))
    }
}

/// `fs_delete`: ワークスペースのファイルを削除する（soft delete・復元可能）。
pub struct FsDeleteTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl FsDeleteTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        FsDeleteTool { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for FsDeleteTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "fs_delete"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "ワークスペースのファイルを削除する（ゴミ箱へ移動＝復元可能）。索引からも外れる。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string", "description": "ファイル名" } },
            "required": ["name"],
            "additionalProperties": false
        })
    }
    fn requires_confirmation(&self) -> bool {
        true
    }
    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let name = parse_name(&input)?;
        self.workspace.delete(ctx, &name, trace_id).await?;
        Ok(ToolOutcome::ok(format!("{name} を削除しました。")))
    }
}

/// 入力から必須の文字列フィールドを取り出す。
fn str_field(input: &serde_json::Value, key: &str) -> Result<String, ToolError> {
    input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ToolError::Invalid(format!("missing '{key}'")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{WorkspaceEntry, WorkspaceWrite};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeWorkspace {
        files: Mutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl WorkspaceStore for FakeWorkspace {
        async fn list(
            &self,
            _ctx: &AuthContext,
            _t: Option<&str>,
        ) -> Result<Vec<WorkspaceEntry>, ToolError> {
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .map(|(k, b)| WorkspaceEntry {
                    name: k.clone(),
                    size: b.len() as u64,
                })
                .collect())
        }
        async fn read(
            &self,
            _ctx: &AuthContext,
            name: &str,
            _t: Option<&str>,
        ) -> Result<Vec<u8>, ToolError> {
            self.files
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| ToolError::Invalid(format!("not found: {name}")))
        }
        async fn write(
            &self,
            _ctx: &AuthContext,
            name: &str,
            bytes: Vec<u8>,
            _ct: &str,
            _t: Option<&str>,
        ) -> Result<WorkspaceWrite, ToolError> {
            let created = self
                .files
                .lock()
                .unwrap()
                .insert(name.to_string(), bytes)
                .is_none();
            Ok(WorkspaceWrite {
                node_id: format!("node-{name}"),
                name: name.to_string(),
                version: if created { 1 } else { 2 },
                created,
            })
        }
        async fn delete(
            &self,
            _ctx: &AuthContext,
            name: &str,
            _t: Option<&str>,
        ) -> Result<(), ToolError> {
            self.files
                .lock()
                .unwrap()
                .remove(name)
                .map(|_| ())
                .ok_or_else(|| ToolError::Invalid(format!("not found: {name}")))
        }
    }

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u1".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t1".into()),
            },
            "org1".into(),
            "t1".into(),
        )
    }

    #[tokio::test]
    async fn write_creates_then_updates_and_emits_artifact() {
        let ws = Arc::new(FakeWorkspace::default());
        let tool = FsWriteTool::new(ws.clone());
        assert!(tool.requires_confirmation());
        let out = tool
            .call(
                &ctx(),
                serde_json::json!({"name": "a.txt", "content": "v1"}),
                None,
            )
            .await
            .unwrap();
        assert!(out.content.contains("作成"));
        assert_eq!(out.artifacts.len(), 1);
        assert_eq!(out.artifacts[0].name, "a.txt");
        // 2 回目は更新（新版）。
        let out = tool
            .call(
                &ctx(),
                serde_json::json!({"name": "a.txt", "content": "v2"}),
                None,
            )
            .await
            .unwrap();
        assert!(out.content.contains("更新"));
    }

    #[tokio::test]
    async fn edit_replaces_unique_occurrence() {
        let ws = Arc::new(FakeWorkspace::default());
        FsWriteTool::new(ws.clone())
            .call(
                &ctx(),
                serde_json::json!({"name": "a.txt", "content": "hello world"}),
                None,
            )
            .await
            .unwrap();
        let edit = FsEditTool::new(ws.clone());
        let out = edit
            .call(
                &ctx(),
                serde_json::json!({"name": "a.txt", "old_string": "world", "new_string": "shiki"}),
                None,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let bytes = ws.read(&ctx(), "a.txt", None).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&bytes), "hello shiki");
    }

    #[tokio::test]
    async fn edit_rejects_ambiguous_or_missing() {
        let ws = Arc::new(FakeWorkspace::default());
        FsWriteTool::new(ws.clone())
            .call(
                &ctx(),
                serde_json::json!({"name": "a.txt", "content": "x x x"}),
                None,
            )
            .await
            .unwrap();
        let edit = FsEditTool::new(ws.clone());
        // 複数一致 → エラー観測。
        let out = edit
            .call(
                &ctx(),
                serde_json::json!({"name": "a.txt", "old_string": "x", "new_string": "y"}),
                None,
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("箇所に一致"));
        // 不一致 → エラー観測。
        let out = edit
            .call(
                &ctx(),
                serde_json::json!({"name": "a.txt", "old_string": "zzz", "new_string": "y"}),
                None,
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn delete_removes_file() {
        let ws = Arc::new(FakeWorkspace::default());
        FsWriteTool::new(ws.clone())
            .call(
                &ctx(),
                serde_json::json!({"name": "a.txt", "content": "x"}),
                None,
            )
            .await
            .unwrap();
        let del = FsDeleteTool::new(ws.clone());
        assert!(del.requires_confirmation());
        let out = del
            .call(&ctx(), serde_json::json!({"name": "a.txt"}), None)
            .await
            .unwrap();
        assert!(out.content.contains("削除"));
        assert!(ws.read(&ctx(), "a.txt", None).await.is_err());
    }
}
