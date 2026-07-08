//! `shell` ツール（Task 5.4）: 任意コマンドをサンドボックス内で実行する。
//!
//! **Durable Workspace モデル**: 実行前にワークスペース（StorageService）を ephemeral サンドボックスへ
//! `put_file` で seed し、実行後に**変更/新規ファイルを `workspace.write` でワークスペースへ sync-back** する
//! （書込→再索引に自動で乗る）。永続 mount は post-alpha のため round-trip で吸収する。egress は既定遮断。
//! 任意コマンド＝破壊的なので `requires_confirmation=true`（承認ゲート/事前許可対象・5.6）。
//!
//! sync-back は 2 系統で漏れなく拾う: ①`list_dir` に現れる**新規**ファイル ②seed 済み名の `get_file` を
//! 取り直しハッシュ比較した**変更**ファイル（実 sidecar は put_file 済みを readdir に出さない quirk があるため）。

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use authz::AuthContext;
use sandbox_client::{
    ExecRequest, Sandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxSpec,
};

use super::mime::content_type_for;
use super::sandbox_exec::{collect_output, truncate};
use crate::tool::{ArtifactRef, Tool, ToolError, ToolOutcome};
use crate::workspace::WorkspaceStore;

/// ワークスペースの guest 上のマウント位置（cwd）。
const WORKSPACE_DIR: &str = "/workspace";
/// seed/sync するファイル数の上限（round-trip の暴走防止）。
const MAX_FILES: usize = 100;
/// seed/sync する 1 ファイルのサイズ上限（orchestrator の 8MiB と同値・二重防御）。
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// `shell` ツール。任意コマンドを実行し、ワークスペースを round-trip する。
pub struct ShellTool {
    sandbox: Arc<dyn Sandbox>,
    workspace: Arc<dyn WorkspaceStore>,
    /// 有効化するゲストコマンドパッケージ（coreutils 等）。
    software: Vec<String>,
    /// 隔離ティア（admin ポリシー・design §4.6）。フル Linux コマンドが動く gVisor 等を選べる。既定は wasm。
    backend: SandboxBackend,
}

impl ShellTool {
    pub fn new(
        sandbox: Arc<dyn Sandbox>,
        workspace: Arc<dyn WorkspaceStore>,
        software: Vec<String>,
        backend: SandboxBackend,
    ) -> Self {
        ShellTool {
            sandbox,
            workspace,
            software,
            backend,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ShellTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "shell"
    }
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "隔離サンドボックスで単一のシェルコマンドを実行する（cwd=/workspace）。作業ディレクトリの\
         ファイルは実行前に読み込まれ、変更/新規ファイルは実行後に自動保存される（再索引される）。\
         ネットワークは遮断。パイプや `&&` は使えない（1 コマンドずつ実行する）。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "cmd": { "type": "string", "description": "実行する単一コマンド（例 `ls -la`）" } },
            "required": ["cmd"],
            "additionalProperties": false
        })
    }
    // 任意コマンド＝破壊的。確認が要る（Task 3.9/5.6）。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let cmd = input
            .get("cmd")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ToolError::Invalid("missing 'cmd'".into()))?;

        let spec = SandboxSpec::agent_shell(
            self.backend,
            ctx.tenant_id.clone(),
            ctx.org.clone(),
            ctx.principal.id.clone(),
            self.software.clone(),
        );
        let handle = self
            .sandbox
            .create(spec)
            .await
            .map_err(|e| ToolError::Unavailable(format!("sandbox create: {e}")))?;

        // 早期 return で destroy を飛ばさないよう、実行本体を包んで最後に必ず destroy する。
        let result = self.run_with_workspace(ctx, &handle, cmd, trace_id).await;
        let _ = self.sandbox.destroy(&handle).await;
        result
    }
}

impl ShellTool {
    /// seed → exec → sync-back を実行する（destroy は呼び出し側が保証）。
    async fn run_with_workspace(
        &self,
        ctx: &AuthContext,
        handle: &SandboxHandle,
        cmd: &str,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let seeded = self.seed_workspace(ctx, handle, trace_id).await?;

        let stream = self
            .sandbox
            .exec(
                handle,
                ExecRequest::Shell {
                    cmd: cmd.to_string(),
                    timeout_ms: None,
                },
            )
            .await
            .map_err(|e| ToolError::Unavailable(format!("sandbox exec: {e}")))?;
        let (stdout, stderr, exit, limit) = collect_output(stream).await?;

        // 変更/新規ファイルをワークスペースへ書き戻す（コマンド失敗時も書かれたものは拾う）。
        let (synced, notes) = self.sync_back(ctx, handle, &seeded, trace_id).await;

        Ok(render(
            &stdout,
            &stderr,
            exit,
            limit.as_deref(),
            synced,
            &notes,
        ))
    }

    /// ワークスペースを guest `/workspace` へ seed し、seed した (name → 内容ハッシュ) を返す。
    async fn seed_workspace(
        &self,
        ctx: &AuthContext,
        handle: &SandboxHandle,
        trace_id: Option<&str>,
    ) -> Result<HashMap<String, u64>, ToolError> {
        let mut seeded = HashMap::new();
        let entries = self.workspace.list(ctx, trace_id).await?;
        for entry in entries.into_iter().take(MAX_FILES) {
            if entry.size > MAX_FILE_BYTES {
                continue; // 大きすぎるファイルは seed しない（round-trip コスト）。
            }
            // 直前に消えた等はスキップ。
            let Ok(bytes) = self.workspace.read(ctx, &entry.name, trace_id).await else {
                continue;
            };
            let path = guest_path(&entry.name);
            if self
                .sandbox
                .put_file(handle, &path, bytes.clone())
                .await
                .is_ok()
            {
                seeded.insert(entry.name, hash_bytes(&bytes));
            }
        }
        Ok(seeded)
    }

    /// guest `/workspace` の**新規/変更/削除**をワークスペースへ反映し、(保存した参照, 注記) を返す。
    async fn sync_back(
        &self,
        ctx: &AuthContext,
        handle: &SandboxHandle,
        seeded: &HashMap<String, u64>,
        trace_id: Option<&str>,
    ) -> (Vec<ArtifactRef>, Vec<String>) {
        let mut saved = Vec::new();
        let mut notes = Vec::new();
        let Ok(entries) = self.sandbox.list_dir(handle, WORKSPACE_DIR).await else {
            return (saved, notes);
        };
        // 実 sidecar は seed 済みを readdir に出さないため、seed 名も明示的に取り直して変更検出する。
        let mut candidates: Vec<String> = entries
            .into_iter()
            .filter(|e| !e.is_dir && !e.name.contains('/') && e.size <= MAX_FILE_BYTES)
            .map(|e| e.name)
            .collect();
        for name in seeded.keys() {
            if !candidates.contains(name) {
                candidates.push(name.clone());
            }
        }

        for name in candidates.into_iter().take(MAX_FILES) {
            match self.sandbox.get_file(handle, &guest_path(&name)).await {
                Ok(bytes) => {
                    // seed 済みで内容が不変ならスキップ（無駄な新版・再索引を避ける）。
                    if seeded.get(&name) == Some(&hash_bytes(&bytes)) {
                        continue;
                    }
                    match self
                        .workspace
                        .write(ctx, &name, bytes, content_type_for(&name), trace_id)
                        .await
                    {
                        Ok(w) => saved.push(ArtifactRef {
                            node_id: w.node_id,
                            name: w.name,
                        }),
                        // 保存失敗は握り潰さずモデルへ観測させる（名前不正は storage が最終検証・PIT-23）。
                        Err(e) => notes.push(format!("{name} の保存に失敗: {e}")),
                    }
                }
                // seed 済みが消えた（`rm` 等）→ ワークスペースからも削除して反映する（削除の同期）。
                Err(SandboxError::NotFound(_)) if seeded.contains_key(&name) => {
                    if self.workspace.delete(ctx, &name, trace_id).await.is_ok() {
                        notes.push(format!("{name} を削除しました。"));
                    }
                }
                // 一時的な取得失敗はスキップ（次回に拾う）。
                Err(_) => {}
            }
        }
        (saved, notes)
    }
}

/// ワークスペースのファイル名を guest の絶対パスへ。
fn guest_path(name: &str) -> String {
    format!("{WORKSPACE_DIR}/{name}")
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// stdout/stderr/exit ＋ 保存ファイルを tool_result へ整形する。
fn render(
    stdout: &str,
    stderr: &str,
    exit: Option<i32>,
    limit: Option<&str>,
    synced: Vec<ArtifactRef>,
    notes: &[String],
) -> ToolOutcome {
    use std::fmt::Write as _;
    let mut body = String::new();
    if !stdout.is_empty() {
        body.push_str("stdout:\n");
        body.push_str(&truncate(stdout));
        body.push('\n');
    }
    if !stderr.is_empty() {
        body.push_str("stderr:\n");
        body.push_str(&truncate(stderr));
        body.push('\n');
    }
    if !synced.is_empty() {
        body.push_str("保存したファイル:\n");
        for a in &synced {
            let _ = writeln!(body, "- {}", a.name);
        }
    }
    // sync-back の削除反映・保存失敗などの注記をモデルへ観測させる。
    for note in notes {
        let _ = writeln!(body, "（{note}）");
    }

    let mut out = if let Some(l) = limit {
        let _ = writeln!(body, "{l}");
        ToolOutcome::error(body)
    } else {
        match exit {
            Some(0) => {
                if body.is_empty() {
                    body.push_str("（出力なし・終了コード 0）");
                }
                ToolOutcome::ok(body)
            }
            Some(code) => {
                let _ = write!(body, "（終了コード {code}）");
                ToolOutcome::error(body)
            }
            None => {
                body.push_str("（実行が完了しませんでした）");
                ToolOutcome::error(body)
            }
        }
    };
    out.artifacts = synced;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{WorkspaceEntry, WorkspaceStore, WorkspaceWrite};
    use sandbox_client::{FakeExecResult, FakeSandbox};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeWorkspace {
        files: Mutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl WorkspaceStore for FakeWorkspace {
        async fn list(
            &self,
            _c: &AuthContext,
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
            _c: &AuthContext,
            name: &str,
            _t: Option<&str>,
        ) -> Result<Vec<u8>, ToolError> {
            self.files
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| ToolError::Invalid("nf".into()))
        }
        async fn write(
            &self,
            _c: &AuthContext,
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
                version: 1,
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
    async fn runs_command_and_syncs_new_file_back() {
        let ws = Arc::new(FakeWorkspace::default());
        // 既存ファイルを 1 つ seed。
        ws.write(&ctx(), "input.txt", b"hi".to_vec(), "text/plain", None)
            .await
            .unwrap();
        // exec は新規ファイル /workspace/out.txt を作る。
        let sandbox = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
            stdout: b"done\n".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
            artifacts: vec![("/workspace/out.txt".into(), b"generated".to_vec())],
        }));
        let tool = ShellTool::new(
            sandbox.clone(),
            ws.clone(),
            vec!["coreutils".into()],
            SandboxBackend::Gvisor,
        );
        assert!(tool.requires_confirmation());
        let out = tool
            .call(&ctx(), serde_json::json!({"cmd": "make out.txt"}), None)
            .await
            .unwrap();
        assert!(!out.is_error);
        // 新規ファイルがワークスペースへ書き戻り、成果物として外部化される。
        assert!(out.content.contains("out.txt"));
        assert!(out.artifacts.iter().any(|a| a.name == "out.txt"));
        let synced = ws.read(&ctx(), "out.txt", None).await.unwrap();
        assert_eq!(synced, b"generated");
        // 変更のない seed 済み input.txt は再書込しない。
        assert!(!out.artifacts.iter().any(|a| a.name == "input.txt"));
        // spec は agent_shell（egress 遮断・software 同梱・admin 選択の backend）。
        let spec = &sandbox.created_specs()[0];
        assert!(spec.egress.static_allow.is_empty());
        assert_eq!(spec.software, vec!["coreutils".to_string()]);
        assert_eq!(spec.backend, SandboxBackend::Gvisor);
        // 実行後に必ず破棄。
        assert_eq!(sandbox.destroyed().len(), 1);
    }

    #[tokio::test]
    async fn nonzero_exit_is_error_but_still_syncs() {
        let ws = Arc::new(FakeWorkspace::default());
        let sandbox = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
            stdout: Vec::new(),
            stderr: b"boom\n".to_vec(),
            exit_code: 2,
            artifacts: vec![("/workspace/partial.txt".into(), b"x".to_vec())],
        }));
        let tool = ShellTool::new(sandbox, ws.clone(), vec![], SandboxBackend::Wasm);
        let out = tool
            .call(&ctx(), serde_json::json!({"cmd": "fail"}), None)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("終了コード 2"));
        // 失敗しても書かれたファイルは拾う。
        assert!(ws.read(&ctx(), "partial.txt", None).await.is_ok());
    }

    #[tokio::test]
    async fn empty_cmd_is_invalid() {
        let ws = Arc::new(FakeWorkspace::default());
        let sandbox = Arc::new(FakeSandbox::new());
        let tool = ShellTool::new(sandbox, ws, vec![], SandboxBackend::Wasm);
        let err = tool
            .call(&ctx(), serde_json::json!({"cmd": "  "}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }
}
