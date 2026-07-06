//! `code_interpreter` ツール（Task 4.10）。サンドボックスで Python を実行し結果を会話へ返す。
//!
//! 実体はサンドボックス基盤（wasm ティア）の制約インスタンス: **Python 限定・ネット遮断（egress 空）・
//! 短命（実行して破棄）・厳しめリソース上限**。呼び出しユーザーの `AuthContext` を素通しし、成果物保存は
//! そのユーザー権限で行う（confused-deputy 回避）。破壊対象が無く確認不要（`requires_confirmation=false`）。

use std::sync::Arc;

use authz::AuthContext;
use futures::StreamExt;
use sandbox_client::{ExecEvent, ExecRequest, Sandbox, SandboxSpec};

use crate::tool::{Tool, ToolError, ToolOutcome};

/// stdout/stderr の会話返却上限（サンドボックス側の 1MiB 上限とは別の、モデル向け整形上限）。
const MODEL_OUTPUT_CAP: usize = 16 * 1024;

/// `code_interpreter` ツール。サンドボックスに `Sandbox` トレイト裏でアクセスする。
pub struct CodeInterpreterTool {
    sandbox: Arc<dyn Sandbox>,
}

impl CodeInterpreterTool {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        CodeInterpreterTool { sandbox }
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
        s.to_string()
    } else {
        format!(
            "{}\n…（出力を{}文字で打ち切り）",
            &s[..MODEL_OUTPUT_CAP],
            MODEL_OUTPUT_CAP
        )
    }
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
         計算・データ処理・整形に使う（ネットワークは遮断）。グラフ描画は行わず、結果の数値/表を返すこと。"
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
        _trace_id: Option<&str>,
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
        let outcome = match exec_result {
            Ok(stream) => {
                let (stdout, stderr, exit, limit) = collect_output(stream).await?;
                Ok(render_outcome(&stdout, &stderr, exit, limit.as_deref()))
            }
            Err(e) => Err(ToolError::Unavailable(format!("sandbox exec: {e}"))),
        };
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
    use sandbox_client::{FakeExecResult, FakeSandbox};

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

    #[tokio::test]
    async fn runs_and_returns_stdout() {
        let sandbox = Arc::new(FakeSandbox::new().with_exec(FakeExecResult {
            stdout: b"42\n".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
            artifacts: Vec::new(),
        }));
        let tool = CodeInterpreterTool::new(sandbox.clone());
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
        let tool = CodeInterpreterTool::new(sandbox);
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
    async fn missing_code_is_invalid() {
        let sandbox = Arc::new(FakeSandbox::new());
        let tool = CodeInterpreterTool::new(sandbox);
        let err = tool
            .call(&ctx(), serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }
}
