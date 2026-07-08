//! ワークスペースの**読み取り系**ツール（Task 5.4）: `fs_list` / `fs_read` / `grep`。
//!
//! いずれも [`WorkspaceStore`] 経由で StorageService を**直読み**する（PIT-5 の「ワークスペース直読み経路」
//! ＝RAG 非同期索引とは別経路・read-after-write が成立）。破壊対象が無いため確認不要。

use std::sync::Arc;

use authz::AuthContext;

use super::sandbox_exec::truncate;
use crate::tool::{Tool, ToolError, ToolOutcome};
use crate::workspace::WorkspaceStore;

/// grep が走査するファイル数の上限（暴走防止）。
const GREP_MAX_FILES: usize = 200;
/// grep が返すマッチ行数の上限。
const GREP_MAX_MATCHES: usize = 200;

/// `fs_list`: ワークスペース直下のファイル一覧を返す。
pub struct FsListTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl FsListTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        FsListTool { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for FsListTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "fs_list"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "作業ディレクトリ（ワークスペース）のファイル一覧を返す。ファイル名とサイズを表示する。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }
    async fn call(
        &self,
        ctx: &AuthContext,
        _input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let entries = self.workspace.list(ctx, trace_id).await?;
        if entries.is_empty() {
            return Ok(ToolOutcome::ok("（ワークスペースは空です）"));
        }
        let mut body = String::new();
        for e in entries {
            use std::fmt::Write as _;
            let _ = writeln!(body, "{} ({} バイト)", e.name, e.size);
        }
        Ok(ToolOutcome::ok(body))
    }
}

/// `fs_read`: ワークスペースの 1 ファイルを読む。
pub struct FsReadTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl FsReadTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        FsReadTool { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for FsReadTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "fs_read"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "ワークスペースのファイルを 1 つ読み、内容を返す（大きいファイルは末尾を切り詰める）。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string", "description": "ファイル名" } },
            "required": ["name"],
            "additionalProperties": false
        })
    }
    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let name = required_name(&input)?;
        let bytes = self.workspace.read(ctx, &name, trace_id).await?;
        let text = String::from_utf8_lossy(&bytes);
        Ok(ToolOutcome::ok(truncate(&text)))
    }
}

/// `grep`: ワークスペース内のファイルを部分文字列で検索し、一致行を返す。
pub struct GrepTool {
    workspace: Arc<dyn WorkspaceStore>,
}

impl GrepTool {
    pub fn new(workspace: Arc<dyn WorkspaceStore>) -> Self {
        GrepTool { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for GrepTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "grep"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "ワークスペースのファイル群を部分文字列で検索し、一致した `ファイル名:行番号: 行` を返す。\
         `name` を指定すると 1 ファイルに絞る。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "検索する部分文字列" },
                "name": { "type": "string", "description": "省略時は全ファイル。指定で 1 ファイルに限定" }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }
    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let pattern = input
            .get("pattern")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::Invalid("missing 'pattern'".into()))?;

        // 対象ファイルを決める（name 指定＝単一 / 省略＝全ファイル）。
        let targets: Vec<String> = match input.get("name").and_then(serde_json::Value::as_str) {
            Some(n) => vec![n.to_string()],
            None => self
                .workspace
                .list(ctx, trace_id)
                .await?
                .into_iter()
                .map(|e| e.name)
                .take(GREP_MAX_FILES)
                .collect(),
        };

        let mut matches = Vec::new();
        for name in targets {
            if matches.len() >= GREP_MAX_MATCHES {
                break;
            }
            // 読めないファイル（削除済み等）はスキップし、検索全体は失敗させない。
            let Ok(bytes) = self.workspace.read(ctx, &name, trace_id).await else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            for (i, line) in text.lines().enumerate() {
                if line.contains(pattern) {
                    matches.push(format!("{name}:{}: {line}", i + 1));
                    if matches.len() >= GREP_MAX_MATCHES {
                        break;
                    }
                }
            }
        }

        if matches.is_empty() {
            return Ok(ToolOutcome::ok(format!("（'{pattern}' に一致なし）")));
        }
        Ok(ToolOutcome::ok(truncate(&matches.join("\n"))))
    }
}

/// 入力から必須の `name` を取り出す。
fn required_name(input: &serde_json::Value) -> Result<String, ToolError> {
    input
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ToolError::Invalid("missing 'name'".into()))
}

/// 書込系ツール（`fs_write`/`fs_edit`/`fs_delete`）と共有する `name` 取り出し。
pub(super) fn parse_name(input: &serde_json::Value) -> Result<String, ToolError> {
    required_name(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{WorkspaceEntry, WorkspaceWrite};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// インメモリのフェイク WorkspaceStore（名前→バイト）。
    #[derive(Default)]
    pub(super) struct FakeWorkspace {
        files: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl FakeWorkspace {
        fn seeded(items: &[(&str, &str)]) -> Self {
            let ws = FakeWorkspace::default();
            for (n, c) in items {
                ws.files
                    .lock()
                    .unwrap()
                    .insert((*n).to_string(), c.as_bytes().to_vec());
            }
            ws
        }
    }

    #[async_trait::async_trait]
    impl WorkspaceStore for FakeWorkspace {
        async fn list(
            &self,
            _ctx: &AuthContext,
            _trace_id: Option<&str>,
        ) -> Result<Vec<WorkspaceEntry>, ToolError> {
            let mut v: Vec<WorkspaceEntry> = self
                .files
                .lock()
                .unwrap()
                .iter()
                .map(|(k, b)| WorkspaceEntry {
                    name: k.clone(),
                    size: b.len() as u64,
                })
                .collect();
            v.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(v)
        }
        async fn read(
            &self,
            _ctx: &AuthContext,
            name: &str,
            _trace_id: Option<&str>,
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
            _content_type: &str,
            _trace_id: Option<&str>,
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
                version: 1,
                created,
            })
        }
        async fn delete(
            &self,
            _ctx: &AuthContext,
            name: &str,
            _trace_id: Option<&str>,
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
    async fn list_and_read() {
        let ws = Arc::new(FakeWorkspace::seeded(&[
            ("a.txt", "hello"),
            ("b.md", "# x"),
        ]));
        let list = FsListTool::new(ws.clone());
        let out = list
            .call(&ctx(), serde_json::json!({}), None)
            .await
            .unwrap();
        assert!(out.content.contains("a.txt"));
        assert!(out.content.contains("b.md"));

        let read = FsReadTool::new(ws);
        let out = read
            .call(&ctx(), serde_json::json!({"name": "a.txt"}), None)
            .await
            .unwrap();
        assert!(out.content.contains("hello"));
    }

    #[tokio::test]
    async fn read_missing_is_invalid() {
        let ws = Arc::new(FakeWorkspace::default());
        let read = FsReadTool::new(ws);
        let err = read
            .call(&ctx(), serde_json::json!({"name": "nope"}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }

    #[tokio::test]
    async fn grep_finds_lines_across_files() {
        let ws = Arc::new(FakeWorkspace::seeded(&[
            ("a.txt", "foo\nbar\nbaz"),
            ("b.txt", "nothing here"),
            ("c.txt", "another bar line"),
        ]));
        let grep = GrepTool::new(ws);
        let out = grep
            .call(&ctx(), serde_json::json!({"pattern": "bar"}), None)
            .await
            .unwrap();
        assert!(out.content.contains("a.txt:2: bar"));
        assert!(out.content.contains("c.txt:1: another bar line"));
        assert!(!out.content.contains("b.txt"));
    }

    #[tokio::test]
    async fn grep_no_match() {
        let ws = Arc::new(FakeWorkspace::seeded(&[("a.txt", "foo")]));
        let grep = GrepTool::new(ws);
        let out = grep
            .call(&ctx(), serde_json::json!({"pattern": "zzz"}), None)
            .await
            .unwrap();
        assert!(out.content.contains("一致なし"));
    }
}
