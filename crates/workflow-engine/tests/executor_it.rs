//! CapabilityNodeExecutor の単体テスト（fake ポート・DB 不要）。
//!
//! 検証: 制御ノードの taken_ports・scope ceiling ゲート・能力ノードの dispatch・http 宛先束縛
//! adversarial・監査/出力の redact。副作用の in-TX 冪等（storage.write）と workflow.start の
//! cross-TX journal は DB を要するため W3/e2e で検証する（本ファイルは純ロジック）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::cognitive_complexity
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq, NodePorts, PortError,
    ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::{
    CapabilityAudit, CapabilityNodeExecutor, EffectJournal, NodeContext, NodeExecutor,
};

/// 監査記録を捕捉する（redact 検証用）。
#[derive(Default)]
struct CapturingAudit {
    records: Mutex<Vec<(String, bool, Value)>>,
}
impl CapabilityAudit for CapturingAudit {
    fn record(&self, _tenant_id: &str, api: &str, allowed: bool, meta: &Value) {
        self.records
            .lock()
            .unwrap()
            .push((api.to_string(), allowed, meta.clone()));
    }
}

/// テスト用ポート。呼び出しを記録し、あらかじめ仕込んだ応答を返す。
#[derive(Default)]
struct FakePorts {
    last_write: Mutex<Option<StorageWriteReq>>,
    last_http: Mutex<Option<HttpSendReq>>,
    http_status: Mutex<u16>,
    secret_hosts: Mutex<Vec<String>>,
}
impl FakePorts {
    fn with_http_status(status: u16) -> Self {
        let p = FakePorts::default();
        *p.http_status.lock().unwrap() = status;
        p
    }
}

#[async_trait]
impl NodePorts for FakePorts {
    async fn storage_write(
        &self,
        _ctx: &ExecCtx,
        req: StorageWriteReq,
    ) -> Result<Value, PortError> {
        let out = json!({ "written": req.name, "bytes": req.bytes.len() });
        *self.last_write.lock().unwrap() = Some(req);
        Ok(out)
    }
    async fn storage_read(&self, _ctx: &ExecCtx, file_id: Uuid) -> Result<Value, PortError> {
        Ok(json!({ "id": file_id.to_string(), "text": "hello" }))
    }
    async fn storage_list(
        &self,
        _ctx: &ExecCtx,
        _parent: Option<Uuid>,
    ) -> Result<Value, PortError> {
        Ok(json!({ "items": [] }))
    }
    async fn rag_search(
        &self,
        _ctx: &ExecCtx,
        _query: &str,
        _top_k: Option<u32>,
    ) -> Result<Value, PortError> {
        Ok(json!({ "results": [ { "file_name": "a" } ] }))
    }
    async fn llm_invoke(&self, _ctx: &ExecCtx, req: LlmInvokeReq) -> Result<Value, PortError> {
        Ok(json!({ "text": format!("echo:{}", req.prompt) }))
    }
    async fn agent_invoke(&self, _ctx: &ExecCtx, _req: AgentInvokeReq) -> Result<Value, PortError> {
        Ok(json!({ "stdout": "ok" }))
    }
    async fn http_send(&self, _ctx: &ExecCtx, req: HttpSendReq) -> Result<HttpSendResp, PortError> {
        let status = *self.http_status.lock().unwrap();
        *self.last_http.lock().unwrap() = Some(req);
        Ok(HttpSendResp {
            status,
            body: br#"{"ok":true}"#.to_vec(),
        })
    }
    async fn resolve_secret(
        &self,
        _ctx: &ExecCtx,
        _name: &str,
    ) -> Result<ResolvedSecretView, PortError> {
        Ok(ResolvedSecretView {
            plaintext: b"super-secret-token".to_vec(),
            allowed_hosts: self.secret_hosts.lock().unwrap().clone(),
        })
    }
    async fn workflow_start(
        &self,
        _ctx: &ExecCtx,
        _name: &str,
        _input: &Value,
    ) -> Result<Value, PortError> {
        Ok(json!({ "run_id": Uuid::nil().to_string() }))
    }
}

/// journal 用の非接続 lazy pool（純ロジックテストは journal を叩かない）。
fn lazy_journal() -> EffectJournal {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap();
    EffectJournal::new(pool)
}

fn executor(ports: Arc<FakePorts>, audit: Arc<CapturingAudit>) -> CapabilityNodeExecutor {
    CapabilityNodeExecutor::new(ports, lazy_journal(), audit)
        .with_http_allowlist(vec!["api.example.com".into()], 5_000)
}

fn ctx(input: Value, scopes: Vec<String>) -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        step_path: "n1".into(),
        idempotency_key: "wf:t1:0:n1".into(),
        attempt: 1,
        principal: "wf".into(),
        principal_kind: "workflow".into(),
        input: input.clone(),
        trigger: input,
        node_outputs: Value::Null,
        trace_id: Some("trace-1".into()),
        scope_ceiling: scopes,
    }
}

#[tokio::test]
async fn branch_selects_true_false_ports() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    let params = json!({
        "condition": { "cmp": { "left": { "$from": "input", "path": "/n" }, "op": "gt", "right": 3 } }
    });
    let res = exec
        .execute("control.branch", &params, &ctx(json!({ "n": 5 }), vec![]))
        .await;
    assert!(res.ok);
    assert_eq!(res.taken_ports, vec!["true".to_string()]);

    let res2 = exec
        .execute("control.branch", &params, &ctx(json!({ "n": 1 }), vec![]))
        .await;
    assert_eq!(res2.taken_ports, vec!["false".to_string()]);
}

#[tokio::test]
async fn switch_matches_case_or_default() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    let params = json!({
        "value": { "$from": "input", "path": "/kind" },
        "cases": [ { "port": "img", "equals": "png" }, { "port": "doc", "equals": "pdf" } ]
    });
    let r = exec
        .execute(
            "control.switch",
            &params,
            &ctx(json!({ "kind": "pdf" }), vec![]),
        )
        .await;
    assert_eq!(r.taken_ports, vec!["doc".to_string()]);
    let r2 = exec
        .execute(
            "control.switch",
            &params,
            &ctx(json!({ "kind": "zip" }), vec![]),
        )
        .await;
    assert_eq!(r2.taken_ports, vec!["default".to_string()]);
}

#[tokio::test]
async fn scope_ceiling_denies_out_of_scope() {
    let audit = Arc::new(CapturingAudit::default());
    let exec = executor(Arc::new(FakePorts::default()), Arc::clone(&audit));
    // scope_ceiling に storage.read が無い → 拒否（permanent）。
    let res = exec
        .execute(
            "storage.read",
            &json!({ "id": Uuid::nil().to_string() }),
            &ctx(json!({}), vec![]),
        )
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.unwrap().code, "out_of_scope");
    // 拒否も監査に残る。
    let recs = audit.records.lock().unwrap();
    assert!(recs
        .iter()
        .any(|(api, allowed, _)| api == "storage.read" && !allowed));
}

#[tokio::test]
async fn storage_read_dispatches_with_scope() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    let res = exec
        .execute(
            "storage.read",
            &json!({ "id": Uuid::nil().to_string() }),
            &ctx(json!({}), vec!["storage.read".into()]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["text"], json!("hello"));
}

#[tokio::test]
async fn storage_write_passes_step_idempotency_key() {
    let ports = Arc::new(FakePorts::default());
    let exec = executor(Arc::clone(&ports), Arc::new(CapturingAudit::default()));
    let params = json!({ "name": "out.txt", "content": "body-bytes" });
    let res = exec
        .execute(
            "storage.write",
            &params,
            &ctx(json!({}), vec!["storage.write".into()]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    let w = ports.last_write.lock().unwrap().clone().unwrap();
    assert_eq!(w.idempotency_key, "wf:t1:0:n1");
    assert_eq!(w.name, "out.txt");
    assert_eq!(w.bytes, b"body-bytes");
    assert!(!w.op_digest.is_empty());
}

#[tokio::test]
async fn dataflow_resolves_prior_node_output() {
    let ports = Arc::new(FakePorts::default());
    let exec = executor(Arc::clone(&ports), Arc::new(CapturingAudit::default()));
    let mut c = ctx(json!({}), vec!["storage.write".into()]);
    c.node_outputs = json!({ "read_file": { "text": "chained" } });
    let params = json!({
        "name": "o.txt",
        "content": { "$from": "nodes.read_file.output", "path": "/text" }
    });
    let res = exec.execute("storage.write", &params, &c).await;
    assert!(res.ok, "{:?}", res.error);
    let w = ports.last_write.lock().unwrap().clone().unwrap();
    assert_eq!(w.bytes, b"chained", "先行ノード出力が content に流れる");
}

#[tokio::test]
async fn http_destination_binding_allows_and_denies() {
    let ports = Arc::new(FakePorts::with_http_status(200));
    let exec = executor(Arc::clone(&ports), Arc::new(CapturingAudit::default()));
    let scopes = vec!["http.egress".into()];

    // allowlist 内ホスト → 通過。
    let ok = exec
        .execute(
            "http.request",
            &json!({ "method": "GET", "url": "https://api.example.com/x" }),
            &ctx(json!({}), scopes.clone()),
        )
        .await;
    assert!(ok.ok, "{:?}", ok.error);
    assert_eq!(ok.output["status"], json!(200));

    // allowlist 外ホスト → forbidden。
    let denied = exec
        .execute(
            "http.request",
            &json!({ "method": "GET", "url": "https://evil.example.org/x" }),
            &ctx(json!({}), scopes.clone()),
        )
        .await;
    assert!(!denied.ok);
    assert_eq!(denied.error.unwrap().code, "forbidden");

    // 近似ホスト（サフィックス付加）→ forbidden。
    let near = exec
        .execute(
            "http.request",
            &json!({ "method": "GET", "url": "https://api.example.com.evil.org/x" }),
            &ctx(json!({}), scopes),
        )
        .await;
    assert!(!near.ok);
}

#[tokio::test]
async fn http_redirect_is_denied_by_default() {
    let ports = Arc::new(FakePorts::with_http_status(302));
    let exec = executor(Arc::clone(&ports), Arc::new(CapturingAudit::default()));
    let res = exec
        .execute(
            "http.request",
            &json!({ "method": "GET", "url": "https://api.example.com/x" }),
            &ctx(json!({}), vec!["http.egress".into()]),
        )
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.unwrap().code, "redirect_denied");
}

#[tokio::test]
async fn http_secret_never_appears_in_audit() {
    let ports = Arc::new(FakePorts::with_http_status(200));
    *ports.secret_hosts.lock().unwrap() = vec!["api.example.com".into()];
    let audit = Arc::new(CapturingAudit::default());
    let exec = executor(Arc::clone(&ports), Arc::clone(&audit));
    let params = json!({
        "method": "POST",
        "url": "https://api.example.com/post",
        "secret": { "name": "tok", "attach": { "kind": "bearer" } }
    });
    let res = exec
        .execute(
            "http.request",
            &params,
            &ctx(json!({}), vec!["http.egress".into()]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    // secret 平文はヘッダにのみ載る（監査には出ない）。
    let http = ports.last_http.lock().unwrap().clone().unwrap();
    assert!(http
        .headers
        .iter()
        .any(|(k, v)| k == "Authorization" && v == "Bearer super-secret-token"));
    assert!(http.headers.iter().any(|(k, _)| k == "Idempotency-Key"));
    let recs = audit.records.lock().unwrap();
    let dump = serde_json::to_string(&*recs).unwrap();
    assert!(
        !dump.contains("super-secret-token"),
        "監査に平文 secret が漏れない"
    );
    assert!(!dump.contains("\"body\""), "監査に本文キーが無い");
}

#[tokio::test]
async fn map_and_wait_are_unsupported_stage_a() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    for nt in ["control.map", "control.wait"] {
        let res = exec.execute(nt, &json!({}), &ctx(json!({}), vec![])).await;
        assert!(!res.ok);
        assert_eq!(res.error.unwrap().code, "unsupported_stage_a");
    }
}

#[tokio::test]
async fn llm_invoke_is_scope_free() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    // llm.invoke は SCOPE_FREE（scope_ceiling 空でも通る）。
    let res = exec
        .execute(
            "llm.invoke",
            &json!({ "prompt": "hi" }),
            &ctx(json!({}), vec![]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["text"], json!("echo:hi"));
}
