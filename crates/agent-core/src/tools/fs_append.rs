//! ワークスペースへの**追記**ツール（`fs_append`・#392）。
//!
//! `fs_write` は全文置換しかないため、証拠台帳のような append-only のメモを 1 行足すたびに
//! モデルが全文を出し直すことになり、**トークンが二次で増える**（deep research の証拠台帳は
//! 数十エントリに育つ）。追記を独立した操作にして構造的に避ける。
//!
//! 既存内容の読み出し→連結→新版は [`WorkspaceStore::append`] の実装（本番は StorageService の
//! 1 txn）が書込ロック下で行う。ここで read してから write すると並行追記で片方が消える。

use std::sync::Arc;

use authz::AuthContext;

use super::fs::parse_name;
use super::fs_write::write_outcome;
use super::mime::content_type_for;
use crate::tool::{Tool, ToolError, ToolOutcome};
use crate::workspace::WorkspaceStore;

/// `fs_append`: ファイル末尾へ追記する（無ければ作成・#392）。
///
/// `fs_write` は全文置換なので、証拠台帳のような append-only のメモを伸ばすたびに
/// モデルが全文を出し直す（トークンが二次で増える）。追記を独立操作にして構造的に避ける。
pub struct FsAppendTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl FsAppendTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        FsAppendTool { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for FsAppendTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        crate::vocab::ToolName::FsAppend.as_str()
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "ファイルの末尾に追記する（無ければ作成）。既存内容を渡す必要はない。\
         メモ・ログ・証拠台帳のように行を足していく用途では fs_write（全文置換）ではなく必ずこれを使う。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "ファイル名" },
                "content": { "type": "string", "description": "末尾に足す内容（改行は自分で入れる）" }
            },
            "required": ["name", "content"],
            "additionalProperties": false
        })
    }
    // 既存内容は保持されるが、新版を作る書込なので write と同格に扱う（Task 3.9/5.6）。
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
            .append(ctx, &name, content, ct, trace_id)
            .await?;
        Ok(write_outcome(w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{WorkspaceEntry, WorkspaceWrite};
    use std::sync::Mutex;

    /// 追記だけを見るための最小フェイク（他の操作は使わない）。
    #[derive(Default)]
    struct AppendOnlyWorkspace {
        content: Mutex<Option<String>>,
        /// append に渡された content_type（拡張子から決まることの確認用）。
        last_content_type: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl WorkspaceStore for AppendOnlyWorkspace {
        async fn list(
            &self,
            _c: &AuthContext,
            _t: Option<&str>,
        ) -> Result<Vec<WorkspaceEntry>, ToolError> {
            Ok(vec![])
        }
        async fn read(
            &self,
            _c: &AuthContext,
            _n: &str,
            _t: Option<&str>,
        ) -> Result<Vec<u8>, ToolError> {
            Err(ToolError::Invalid("未使用".into()))
        }
        async fn write(
            &self,
            _c: &AuthContext,
            _n: &str,
            _b: Vec<u8>,
            _ct: &str,
            _t: Option<&str>,
        ) -> Result<WorkspaceWrite, ToolError> {
            panic!("fs_append は write を呼ばない（全文再送を避けるための独立操作）");
        }
        async fn append(
            &self,
            _c: &AuthContext,
            name: &str,
            suffix: &str,
            content_type: &str,
            _t: Option<&str>,
        ) -> Result<WorkspaceWrite, ToolError> {
            *self.last_content_type.lock().unwrap() = Some(content_type.to_string());
            let mut cur = self.content.lock().unwrap();
            let created = cur.is_none();
            cur.get_or_insert_with(String::new).push_str(suffix);
            Ok(WorkspaceWrite {
                node_id: format!("node-{name}"),
                name: name.to_string(),
                version: if created { 1 } else { 2 },
                created,
            })
        }
        async fn delete(
            &self,
            _c: &AuthContext,
            _n: &str,
            _t: Option<&str>,
        ) -> Result<(), ToolError> {
            Ok(())
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
    async fn append_accumulates_without_resending_whole_file() {
        let ws = Arc::new(AppendOnlyWorkspace::default());
        let tool = FsAppendTool::new(ws.clone());
        assert_eq!(tool.name(), "fs_append");
        // 破壊系（新版を作る）として自己申告する。事前許可の判断はポリシ層（chat）が持つ。
        assert!(tool.requires_confirmation());

        let out = tool
            .call(
                &ctx(),
                serde_json::json!({ "name": "notes.md", "content": "E1 | src-a\n" }),
                None,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("作成"), "初回は作成: {}", out.content);
        assert_eq!(out.artifacts.len(), 1, "書込先を成果物として外部化する");

        tool.call(
            &ctx(),
            serde_json::json!({ "name": "notes.md", "content": "E2 | src-b\n" }),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            ws.content.lock().unwrap().as_deref(),
            Some("E1 | src-a\nE2 | src-b\n"),
            "既存内容の末尾へ積む"
        );
        assert_eq!(
            ws.last_content_type.lock().unwrap().as_deref(),
            Some("text/markdown"),
            "content_type はファイル名から決まる"
        );
    }

    /// 必須フィールド欠落はツール側で `Invalid`（モデルが観測して直せる）。
    ///
    /// 名前の封じ込め（`..` / `/` の拒否）は他の fs ツールと同様 StorageService の
    /// `validate_name` が担う（正本は `chat/tests/workspace_containment_it.rs`）。
    /// ここで独自の名前検証を足すと二重定義になるため足さない。
    #[tokio::test]
    async fn append_rejects_missing_fields() {
        let tool = FsAppendTool::new(Arc::new(AppendOnlyWorkspace::default()));
        assert!(matches!(
            tool.call(&ctx(), serde_json::json!({ "name": "a.md" }), None)
                .await,
            Err(ToolError::Invalid(_))
        ));
        assert!(matches!(
            tool.call(&ctx(), serde_json::json!({ "content": "x" }), None)
                .await,
            Err(ToolError::Invalid(_))
        ));
    }
}
