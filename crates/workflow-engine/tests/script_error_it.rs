//! 「script を実行」の失敗の code の単体テスト（実 ScriptEngine・fake ポート・DB 不要・D-121・BUG-136・BUG-137）。
//!
//! - 捕まえていない `Shiki.*` の誤りは、ホストが返した code・retryable のまま step の失敗になる（internal にしない）。
//!   使ってよい操作の天井の外（out_of_scope）は、再開しても同じ結果になる失敗として分類される。
//! - 作成者が投げた誤り（throw・Shiki.fail・書き誤り）は script_error（設定の誤り・やり直さない）になる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::run::failure::{resume_hint, run_failure};
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, FailureCategory, NodeContext,
    NodeExecutor, NodeResult, ResumeAdvice,
};

/// storage.read だけ、決まった誤りを返すポート（ほかの能力は使わない）。
struct FailingReadPorts {
    error: PortError,
}

fn unused() -> PortError {
    PortError::unavailable("このテストでは使わない")
}

#[async_trait]
impl NodePorts for FailingReadPorts {
    async fn storage_write(&self, _: &ExecCtx, _: StorageWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn storage_read(&self, _: &ExecCtx, _: Uuid) -> Result<Value, PortError> {
        Err(self.error.clone())
    }
    async fn storage_list(
        &self,
        _: &ExecCtx,
        _: workflow_engine::StorageListReq,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn rag_search(&self, _: &ExecCtx, _: &str, _: Option<u32>) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn llm_invoke(&self, _: &ExecCtx, _: LlmInvokeReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn agent_invoke(
        &self,
        _: &ExecCtx,
        _: AgentInvokeReq,
        _: Arc<dyn workflow_engine::AgentToolGateway>,
    ) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn http_send(&self, _: &ExecCtx, _: HttpSendReq) -> Result<HttpSendResp, PortError> {
        Err(unused())
    }
    async fn resolve_secret(&self, _: &ExecCtx, _: &str) -> Result<ResolvedSecretView, PortError> {
        Err(unused())
    }
    async fn workflow_start(&self, _: &ExecCtx, _: &str, _: &Value) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn skill_resolve(
        &self,
        _: &ExecCtx,
        _: &str,
        _: &str,
    ) -> Result<workflow_engine::nodes::ResolvedSkillView, PortError> {
        Err(unused())
    }
    async fn csv_query(&self, _: &ExecCtx, _: Uuid, _: &str) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_patch(&self, _: &ExecCtx, _: CsvPatchReq) -> Result<Value, PortError> {
        Err(unused())
    }
    async fn csv_write(&self, _: &ExecCtx, _: CsvWriteReq) -> Result<Value, PortError> {
        Err(unused())
    }
}

struct NoAudit;
impl CapabilityAudit for NoAudit {
    fn record(&self, _: &str, _: &str, _: bool, _: &Value) {}
}

fn ctx(scopes: &[&str]) -> NodeContext {
    let input = json!({ "id": Uuid::nil().to_string() });
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        run_started_at: chrono::Utc::now(),
        step_path: "n1".into(),
        idempotency_key: "wf:t1:0:n1".into(),
        attempt: 1,
        principal: "wf".into(),
        principal_kind: "workflow".into(),
        input: input.clone(),
        trigger: input,
        node_outputs: Value::Null,
        each: None,
        trace_id: None,
        scope_ceiling: scopes.iter().map(ToString::to_string).collect(),
        cancel: workflow_engine::run::CancelSignal::default(),
        workflow_id: Uuid::nil(),
        cache: None,
    }
}

/// 1 つの script を、storage.read が `error` を返すポートで 1 回実行する。
async fn run(source: &str, error: PortError, scopes: &[&str]) -> NodeResult {
    let journal = EffectJournal::new(
        PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
            .unwrap(),
    );
    let exec = CapabilityNodeExecutor::new(
        Arc::new(FailingReadPorts { error }) as Arc<dyn NodePorts>,
        journal,
        Arc::new(NoAudit),
    )
    .with_script_engine(
        script_runtime::EngineHandle::shared(),
        script_runtime::engine::Limits::default(),
    );
    exec.execute(
        "script.run",
        &json!({ "source": { "inline": source } }),
        &ctx(scopes),
    )
    .await
}

const READ: &str = "function main(input) { return Shiki.storage.read(input.id); }";

/// 使ってよい操作の天井の外の `Shiki.storage.read` を捕まえずに投げ返すと、step は out_of_scope で失敗し、再開しても
/// 同じ結果になると分類される（BUG-136。以前は internal で「そのまま再開できる」と勧めていた）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncaught_out_of_scope_keeps_its_code() {
    let res = run(READ, PortError::unavailable("呼ばれない"), &[]).await;
    assert!(!res.ok);
    let e = res.error.unwrap();
    assert_eq!((e.code.as_str(), e.retryable), ("out_of_scope", false));
    assert!(e.message.contains("storage.read"), "{}", e.message);
    assert_eq!(
        run_failure(Uuid::nil(), Some(&e.code), None).category,
        FailureCategory::Permission
    );
    let hint = resume_hint(Some(&e.code), None);
    assert_eq!(hint.advice, ResumeAdvice::SameResult);
    assert!(!hint.offer);
}

/// ポートの誤りもそのままの code・retryable・文で返る（やり直せる誤りはやり直せる）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncaught_port_errors_keep_code_and_retryable() {
    for (code, retryable) in [("unavailable", true), ("not_found", false)] {
        let res = run(
            READ,
            PortError::new(code, format!("{code} の誤り"), retryable),
            &["storage.read"],
        )
        .await;
        let e = res.error.unwrap();
        assert_eq!(
            (e.code.as_str(), e.retryable, e.message.as_str()),
            (code, retryable, format!("{code} の誤り").as_str())
        );
    }
}

/// 作成者が投げた誤りは script_error（設定の誤り・やり直さない）。利用者向けの文は「作成者に連絡」で、再開しても
/// 同じ結果になると分類される（BUG-137。以前は internal で「問題が続くときは作成者に連絡」だった）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn author_errors_are_script_errors() {
    for source in [
        "function main(input) { throw new Error('金額が負です'); }",
        "function main(input) { Shiki.fail('止めます', { permanent: true }); }",
        "function main(input) { var a = null; return a.b; }",
        // ホストの誤りを捕まえて、文を変えて投げ直したもの。
        "function main(input) { try { Shiki.storage.read(input.id); } catch (e) { throw new Error('読めません: ' + e.message); } }",
    ] {
        let res = run(
            source,
            PortError::new("not_found", "見つかりません", false),
            &["storage.read"],
        )
        .await;
        let e = res.error.unwrap();
        assert_eq!(e.code, script_runtime::SCRIPT_ERROR, "{source}: {e:?}");
        assert!(!e.retryable, "{source}");
        let f = run_failure(Uuid::nil(), Some(&e.code), None);
        assert_eq!(f.category, FailureCategory::Configuration);
        assert!(f.message.contains("作成者に連絡"), "{}", f.message);
        assert_eq!(
            resume_hint(Some(&e.code), None).advice,
            ResumeAdvice::SameResult
        );
    }
}
