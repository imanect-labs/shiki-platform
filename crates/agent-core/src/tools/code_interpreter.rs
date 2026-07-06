//! `code_interpreter` ツール（Task 4.10）。サンドボックスで Python を実行し結果を会話へ返す。
//!
//! 実体はサンドボックス基盤（wasm ティア）の制約インスタンス: **Python 限定・ネット遮断（egress 空）・
//! 短命（実行して破棄）・厳しめリソース上限**。呼び出しユーザーの `AuthContext` を素通しし、成果物保存は
//! そのユーザー権限で行う（confused-deputy 回避）。破壊対象が無く確認不要（`requires_confirmation=false`）。

use std::sync::Arc;

use authz::AuthContext;
use futures::StreamExt;
use sandbox_client::{ExecEvent, ExecRequest, Sandbox, SandboxSpec};

use super::artifacts::collect_artifacts;
use crate::tool::{ArtifactStore, Tool, ToolError, ToolOutcome};

/// stdout/stderr の会話返却上限（サンドボックス側の 1MiB 上限とは別の、モデル向け整形上限）。
const MODEL_OUTPUT_CAP: usize = 16 * 1024;

/// `code_interpreter` ツール。サンドボックスに `Sandbox` トレイト裏でアクセスする。
pub struct CodeInterpreterTool {
    sandbox: Arc<dyn Sandbox>,
    /// 成果物の保存先（発話ユーザー権限で保存）。未配線なら成果物は回収しない。
    artifacts: Option<Arc<dyn ArtifactStore>>,
}

impl CodeInterpreterTool {
    pub fn new(sandbox: Arc<dyn Sandbox>, artifacts: Option<Arc<dyn ArtifactStore>>) -> Self {
        CodeInterpreterTool { sandbox, artifacts }
    }
}

/// exec ストリームを stdout/stderr/exit に畳み込む。
async fn collect_output(
    mut stream: futures::stream::BoxStream<
        'static,
        Result<ExecEvent, sandbox_client::SandboxError>,
    >,
) -> Result<(String, String, Option<i32>, Option<String>), ToolError> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit = None;
    let mut limit = None;
    while let Some(ev) = stream.next().await {
        match ev.map_err(|e| ToolError::Unavailable(format!("sandbox exec: {e}")))? {
            ExecEvent::Stdout(b) => stdout.extend_from_slice(&b),
            ExecEvent::Stderr(b) => stderr.extend_from_slice(&b),
            ExecEvent::Exited { code } => exit = Some(code),
            ExecEvent::LimitExceeded { kind, detail } => {
                limit = Some(format!("リソース超過（{kind:?}）: {detail}"));
            }
        }
    }
    Ok((
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
        exit,
        limit,
    ))
}

fn truncate(s: &str) -> String {
    if s.len() <= MODEL_OUTPUT_CAP {
        return s.to_string();
    }
    // バイト境界がマルチバイト文字の途中に来るとスライスがパニックするため、
    // 直近の char 境界まで戻す（Python 出力は日本語等を含みうる）。
    let mut end = MODEL_OUTPUT_CAP;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…（出力を{end}バイトで打ち切り）", &s[..end])
}

#[async_trait::async_trait]
impl Tool for CodeInterpreterTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "code_interpreter"
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "隔離サンドボックスで Python コードを実行し、標準出力/エラーを返す。numpy・pandas が使える。\
         計算・データ処理・整形に使う（ネットワークは遮断）。/workspace に書いたファイルは実行後に\
         自動保存され会話に添付される。グラフ描画は行わず、結果の数値/表を返すこと。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "description": "実行する Python コード" }
            },
            "required": ["code"],
            "additionalProperties": false
        })
    }

    // ネット遮断・まっさら・短命で破壊対象が無いため確認不要（Task 3.9 ポリシ）。
    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let code = input
            .get("code")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::Invalid("missing 'code'".into()))?;
        if code.trim().is_empty() {
            return Err(ToolError::Invalid("code is empty".into()));
        }

        let spec = SandboxSpec::code_interpreter(
            ctx.tenant_id.clone(),
            ctx.org.clone(),
            ctx.principal.id.clone(),
        );
        let handle = self
            .sandbox
            .create(spec)
            .await
            .map_err(|e| ToolError::Unavailable(format!("sandbox create: {e}")))?;

        // 実行後は必ず破棄する（短命・まっさら）。
        let exec_result = self
            .sandbox
            .exec(
                &handle,
                ExecRequest::Python {
                    code: code.to_string(),
                    timeout_ms: None,
                },
            )
            .await;
        // `?` で早期 return すると下の destroy がスキップされるため、必ず destroy を通す形にする。
        let outcome = match exec_result {
            Ok(stream) => match collect_output(stream).await {
                Ok((stdout, stderr, exit, limit)) => {
                    let mut out = render_outcome(&stdout, &stderr, exit, limit.as_deref());
                    // 成果物の回収は実行成功時のみ（失敗実行の中途ファイルは保存しない）。
                    if !out.is_error {
                        if let Some(store) = &self.artifacts {
                            collect_artifacts(
                                self.sandbox.as_ref(),
                                ctx,
                                &handle,
                                store.as_ref(),
                                &mut out,
                                trace_id,
                            )
                            .await;
                        }
                    }
                    Ok(out)
                }
                Err(e) => Err(e),
            },
            Err(e) => Err(ToolError::Unavailable(format!("sandbox exec: {e}"))),
        };
        // 実行後は必ず破棄する（短命・まっさら）。collect のエラー時も破棄を保証する。
        let _ = self.sandbox.destroy(&handle).await;
        outcome
    }
}

/// stdout/stderr/exit を tool_result テキストへ整形する。
fn render_outcome(
    stdout: &str,
    stderr: &str,
    exit: Option<i32>,
    limit: Option<&str>,
) -> ToolOutcome {
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
    if let Some(l) = limit {
        body.push_str(l);
        body.push('\n');
        return ToolOutcome::error(body);
    }
    match exit {
        Some(0) => {
            if body.is_empty() {
                body.push_str("（出力なし・終了コード 0）");
            }
            ToolOutcome::ok(body)
        }
        Some(code) => {
            use std::fmt::Write as _;
            let _ = write!(body, "（終了コード {code}）");
            ToolOutcome::error(body)
        }
        None => {
            body.push_str("（実行が完了しませんでした）");
            ToolOutcome::error(body)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::tool::ArtifactRef;
    use sandbox_client::{FakeExecResult, FakeSandbox};
    use std::sync::Mutex;

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
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

    /// 保存 1 件の記録（name, bytes, content_type）。
    type SavedArtifact = (String, Vec<u8>, String);

    /// 保存呼び出しを記録するフェイク ArtifactStore。
    #[derive(Default)]
    struct FakeArtifactStore {
        saved: Mutex<Vec<SavedArtifact>>,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl ArtifactStore for FakeArtifactStore {
        async fn save(
            &self,
            _ctx: &AuthContext,
            name: &str,
            bytes: Vec<u8>,
            content_type: &str,
            _trace_id: Option<&str>,
        ) -> Result<ArtifactRef, ToolError> {
            if self.fail {
                return Err(ToolError::Unavailable("fake save failure".into()));
            }
            let node_id = format!("node-{}", name);
            self.saved
                .lock()
                .unwrap()
                .push((name.to_string(), bytes, content_type.to_string()));
            Ok(ArtifactRef {
                node_id,
                name: name.to_string(),
            })
        }
    }

    #[tokio::test]
    async fn runs_and_returns_stdout() {
        let sandbox = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
            stdout: b"42\n".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
            artifacts: Vec::new(),
        }));
        let tool = CodeInterpreterTool::new(sandbox.clone(), None);
        let out = tool
            .call(&ctx(), serde_json::json!({"code": "print(42)"}), None)
            .await
            .expect("ok");
        assert!(out.content.contains("42"));
        assert!(!out.is_error);
        // 実行後に破棄されている。
        assert_eq!(sandbox.destroyed().len(), 1);
    }

    #[tokio::test]
    async fn nonzero_exit_is_error() {
        let sandbox = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
            stdout: Vec::new(),
            stderr: b"Traceback...\n".to_vec(),
            exit_code: 1,
            artifacts: Vec::new(),
        }));
        let tool = CodeInterpreterTool::new(sandbox, None);
        let out = tool
            .call(
                &ctx(),
                serde_json::json!({"code": "raise Exception()"}),
                None,
            )
            .await
            .expect("ok");
        assert!(out.is_error);
        assert!(out.content.contains("Traceback"));
    }

    #[tokio::test]
    async fn collects_artifacts_after_success() {
        // 実行後 /workspace に現れたファイル（main.py 以外）を保存し、ArtifactRef を返す。
        let sandbox = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
            stdout: b"done\n".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
            artifacts: vec![
                ("/workspace/result.csv".into(), b"a,b\n1,2\n".to_vec()),
                ("/workspace/main.py".into(), b"print()".to_vec()), // 実行コードは除外
            ],
        }));
        let store = Arc::new(FakeArtifactStore::default());
        let tool = CodeInterpreterTool::new(sandbox.clone(), Some(store.clone()));
        let out = tool
            .call(&ctx(), serde_json::json!({"code": "write csv"}), None)
            .await
            .expect("ok");
        assert!(!out.is_error);
        assert_eq!(out.artifacts.len(), 1);
        assert_eq!(out.artifacts[0].name, "result.csv");
        assert!(out.content.contains("result.csv"));
        // content_type は拡張子から推定される。
        let saved = store.saved.lock().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].2, "text/csv");
        drop(saved);
        // 成果物回収後も必ず破棄される。
        assert_eq!(sandbox.destroyed().len(), 1);
    }

    #[tokio::test]
    async fn artifact_save_failure_does_not_fail_tool() {
        // 保存失敗はツール全体を失敗させず、観測テキストに注記する。
        let sandbox = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
            stdout: b"done\n".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
            artifacts: vec![("/workspace/out.txt".into(), b"x".to_vec())],
        }));
        let store = Arc::new(FakeArtifactStore {
            fail: true,
            ..Default::default()
        });
        let tool = CodeInterpreterTool::new(sandbox, Some(store));
        let out = tool
            .call(&ctx(), serde_json::json!({"code": "write"}), None)
            .await
            .expect("ok");
        assert!(!out.is_error);
        assert!(out.artifacts.is_empty());
        assert!(out.content.contains("保存に失敗"));
    }

    #[tokio::test]
    async fn failed_exec_skips_artifact_collection() {
        // 失敗実行の中途ファイルは保存しない。
        let sandbox = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
            stdout: Vec::new(),
            stderr: b"boom".to_vec(),
            exit_code: 1,
            artifacts: vec![("/workspace/partial.csv".into(), b"a".to_vec())],
        }));
        let store = Arc::new(FakeArtifactStore::default());
        let tool = CodeInterpreterTool::new(sandbox, Some(store.clone()));
        let out = tool
            .call(&ctx(), serde_json::json!({"code": "boom"}), None)
            .await
            .expect("ok");
        assert!(out.is_error);
        assert!(out.artifacts.is_empty());
        assert!(store.saved.lock().unwrap().is_empty());
    }

    #[test]
    fn truncate_respects_utf8_boundary() {
        // マルチバイト（日本語）だけの長い文字列。バイト境界が文字途中でもパニックしない。
        let s = "あ".repeat(MODEL_OUTPUT_CAP); // 3 bytes/char
        let out = truncate(&s);
        assert!(out.contains("バイトで打ち切り"));
        // 打ち切り位置は char 境界（3 の倍数）まで戻っている。
        assert!(out.starts_with('あ'));
    }

    #[tokio::test]
    async fn missing_code_is_invalid() {
        let sandbox = Arc::new(FakeSandbox::new());
        let tool = CodeInterpreterTool::new(sandbox, None);
        let err = tool
            .call(&ctx(), serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }
}
