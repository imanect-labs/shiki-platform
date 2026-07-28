//! [`super`]（ステップ内ツール実行フェーズ・#349）の単体テスト。
//!
//! 実ツールを使わず、遅延と同時実行数を観測する `ProbeTool` で
//! 「並列になっているか」「観測順が呼び出し順か」を直接確かめる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::approval::{ApprovalDecision, ApprovalPolicy};
use crate::tool::ToolError;

use super::*;

/// 実行の重なりを観測するテストツール。`delay` 待つ間の同時実行数を記録する。
struct ProbeTool {
    name: &'static str,
    read_only: bool,
    confirm: bool,
    delay: Duration,
    /// 同時実行数の最大値（並列化の証拠）。
    peak: Arc<AtomicUsize>,
    live: Arc<AtomicUsize>,
    /// 開始順（呼び出し順とは限らない）。
    started: Arc<Mutex<Vec<String>>>,
}

impl ProbeTool {
    fn new(name: &'static str, read_only: bool) -> Self {
        ProbeTool {
            name,
            read_only,
            confirm: false,
            delay: Duration::from_millis(120),
            peak: Arc::new(AtomicUsize::new(0)),
            live: Arc::new(AtomicUsize::new(0)),
            started: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn shared(mut self, peak: &Arc<AtomicUsize>, live: &Arc<AtomicUsize>) -> Self {
        self.peak = peak.clone();
        self.live = live.clone();
        self
    }
}

#[async_trait::async_trait]
impl Tool for ProbeTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "probe"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    fn requires_confirmation(&self) -> bool {
        self.confirm
    }
    fn is_read_only(&self) -> bool {
        self.read_only
    }
    async fn call(
        &self,
        _ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let tag = input
            .get("tag")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("-")
            .to_string();
        self.started.lock().unwrap().push(tag.clone());
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.live.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutcome::ok(format!("done:{tag}")))
    }
}

struct NullSink {
    events: Vec<AgentEvent>,
}

#[async_trait::async_trait]
impl EventSink for NullSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), AgentError> {
        self.events.push(event);
        Ok(())
    }
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// 常に承認する Approver（承認待ちが read の並列を止めないことの確認に使う）。
struct YesApprover;

#[async_trait::async_trait]
impl Approver for YesApprover {
    async fn decide(&self, _id: &str, _name: &str, _input: &serde_json::Value) -> ApprovalDecision {
        tokio::time::sleep(Duration::from_millis(120)).await;
        ApprovalDecision::Approved
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

fn call(id: &str, name: &str, tag: &str) -> PendingCall {
    PendingCall {
        id: id.into(),
        name: name.into(),
        input: serde_json::json!({ "tag": tag }),
    }
}

fn contents(blocks: &[Block]) -> Vec<String> {
    blocks
        .iter()
        .map(|b| match b {
            Block::ToolResult { content, .. } => content.clone(),
            _ => String::new(),
        })
        .collect()
}

/// 冪等 read は並列に走り（壁時計が直列和にならない）、観測順は呼び出し順のまま。
#[tokio::test]
async fn reads_run_in_parallel_and_keep_call_order() {
    let peak = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ProbeTool::new("doc_search", true).shared(&peak, &live)),
        Arc::new(ProbeTool::new("web_search", true).shared(&peak, &live)),
        Arc::new(ProbeTool::new("code_interpreter", true).shared(&peak, &live)),
    ];
    let map: HashMap<&str, &Arc<dyn Tool>> = tools.iter().map(|t| (t.name(), t)).collect();
    let opts = AgentOptions::chat(8);
    let phase = ToolPhase {
        tool_map: &map,
        ctx: &ctx(),
        trace_id: None,
        opts: &opts,
        approver: None,
    };
    let calls = vec![
        call("1", "doc_search", "a"),
        call("2", "web_search", "b"),
        call("3", "code_interpreter", "c"),
    ];
    let mut sink = NullSink { events: Vec::new() };
    let mut plan = Plan::default();
    let mut detector = LoopDetector::default();
    let started = Instant::now();
    let out = run_tool_calls(&phase, calls, &mut plan, &mut sink, &mut detector)
        .await
        .unwrap();
    let elapsed = started.elapsed();

    let ToolPhaseOutcome::Executed { blocks, .. } = out else {
        panic!("cancelled")
    };
    // 3 件 ×120ms を直列にすると 360ms。並列なら最遅 1 件（≒120ms）に近づく。
    assert!(
        elapsed < Duration::from_millis(300),
        "直列になっている: {elapsed:?}"
    );
    assert_eq!(peak.load(Ordering::SeqCst), 3, "3 件が同時に走っていない");
    // 観測は**呼び出し順**（完了順ではない）。
    assert_eq!(contents(&blocks), vec!["done:a", "done:b", "done:c"]);
}

/// 承認要ツールが混ざっても read は先行して並列に走る（承認待ちにブロックされない）。
#[tokio::test]
async fn reads_do_not_block_on_approval() {
    let peak = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let mut writer = ProbeTool::new("fs_write", false);
    writer.confirm = true;
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(writer),
        Arc::new(ProbeTool::new("doc_search", true).shared(&peak, &live)),
        Arc::new(ProbeTool::new("web_search", true).shared(&peak, &live)),
    ];
    let map: HashMap<&str, &Arc<dyn Tool>> = tools.iter().map(|t| (t.name(), t)).collect();
    let opts = AgentOptions::chat(8);
    let approver = YesApprover;
    let phase = ToolPhase {
        tool_map: &map,
        ctx: &ctx(),
        trace_id: None,
        opts: &opts,
        approver: Some(&approver),
    };
    // 破壊系が**先頭**でも、後続の read はその承認待ちと並行して進む。
    let calls = vec![
        call("1", "fs_write", "w"),
        call("2", "doc_search", "a"),
        call("3", "web_search", "b"),
    ];
    let mut sink = NullSink { events: Vec::new() };
    let mut plan = Plan::default();
    let mut detector = LoopDetector::default();
    let started = Instant::now();
    let out = run_tool_calls(&phase, calls, &mut plan, &mut sink, &mut detector)
        .await
        .unwrap();
    let elapsed = started.elapsed();

    let ToolPhaseOutcome::Executed { blocks, .. } = out else {
        panic!("cancelled")
    };
    // 承認 120ms → 書込 120ms の逐次（240ms）と read 120ms が**重なる**。
    assert!(
        elapsed < Duration::from_millis(360),
        "read が承認待ちで止まっている: {elapsed:?}"
    );
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(contents(&blocks), vec!["done:w", "done:a", "done:b"]);
    // 承認イベントは出ている（ゲートは素通しされていない）。
    assert!(sink
        .events
        .iter()
        .any(|e| matches!(e, AgentEvent::ApprovalRequested { .. })));
}

/// 承認者が居なければ破壊系は実行されず、観測だけが呼び出し順で残る。
#[tokio::test]
async fn unapproved_write_is_rejected_in_place() {
    let mut writer = ProbeTool::new("fs_write", false);
    writer.confirm = true;
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(writer),
        Arc::new(ProbeTool::new("doc_search", true)),
    ];
    let map: HashMap<&str, &Arc<dyn Tool>> = tools.iter().map(|t| (t.name(), t)).collect();
    let opts = AgentOptions::chat(8);
    let phase = ToolPhase {
        tool_map: &map,
        ctx: &ctx(),
        trace_id: None,
        opts: &opts,
        approver: None,
    };
    let calls = vec![call("1", "doc_search", "a"), call("2", "fs_write", "w")];
    let mut sink = NullSink { events: Vec::new() };
    let mut plan = Plan::default();
    let mut detector = LoopDetector::default();
    let ToolPhaseOutcome::Executed { blocks, .. } =
        run_tool_calls(&phase, calls, &mut plan, &mut sink, &mut detector)
            .await
            .unwrap()
    else {
        panic!("cancelled")
    };
    assert_eq!(blocks.len(), 2);
    let Block::ToolResult { is_error, .. } = &blocks[1] else {
        panic!("tool result ではない")
    };
    assert!(is_error, "未承認の破壊系が実行されている");
}

/// 同一ホストへの web_fetch は並列にしない（1 ステップから同じサイトを叩き続けない）。
#[tokio::test]
async fn same_host_web_fetch_is_serialized() {
    let peak = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(
        ProbeTool::new("web_fetch", true).shared(&peak, &live),
    )];
    let map: HashMap<&str, &Arc<dyn Tool>> = tools.iter().map(|t| (t.name(), t)).collect();
    let opts = AgentOptions::chat(8);
    let phase = ToolPhase {
        tool_map: &map,
        ctx: &ctx(),
        trace_id: None,
        opts: &opts,
        approver: None,
    };
    let url_call = |id: &str, url: &str| PendingCall {
        id: id.into(),
        name: "web_fetch".into(),
        input: serde_json::json!({ "url": url, "tag": url }),
    };
    let calls = vec![
        url_call("1", "https://example.com/a"),
        url_call("2", "https://example.com/b"),
        url_call("3", "https://other.example.org/c"),
    ];
    let mut sink = NullSink { events: Vec::new() };
    let mut plan = Plan::default();
    let mut detector = LoopDetector::default();
    let ToolPhaseOutcome::Executed { blocks, .. } =
        run_tool_calls(&phase, calls, &mut plan, &mut sink, &mut detector)
            .await
            .unwrap()
    else {
        panic!("cancelled")
    };
    // 同一ホストは直列なので、同時実行は「別ホストとの 2 本」が上限。
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        contents(&blocks)[0],
        "done:https://example.com/a",
        "観測順が呼び出し順でない"
    );
}

/// 並列度 1 に絞れば逐次と等価（設定が効く）。
#[tokio::test]
async fn concurrency_limit_is_honoured() {
    let peak = Arc::new(AtomicUsize::new(0));
    let live = Arc::new(AtomicUsize::new(0));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ProbeTool::new("doc_search", true).shared(&peak, &live)),
        Arc::new(ProbeTool::new("web_search", true).shared(&peak, &live)),
    ];
    let map: HashMap<&str, &Arc<dyn Tool>> = tools.iter().map(|t| (t.name(), t)).collect();
    let mut opts = AgentOptions::chat(8);
    opts.parallel_read_tools = 1;
    let phase = ToolPhase {
        tool_map: &map,
        ctx: &ctx(),
        trace_id: None,
        opts: &opts,
        approver: None,
    };
    let calls = vec![call("1", "doc_search", "a"), call("2", "web_search", "b")];
    let mut sink = NullSink { events: Vec::new() };
    let mut plan = Plan::default();
    let mut detector = LoopDetector::default();
    run_tool_calls(&phase, calls, &mut plan, &mut sink, &mut detector)
        .await
        .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 1);
}

/// 事前許可された破壊系は承認待ちに入らない（既存のポリシ挙動を壊さない）。
#[tokio::test]
async fn pre_authorized_write_runs_without_approver() {
    let mut writer = ProbeTool::new("fs_write", false);
    writer.confirm = true;
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(writer)];
    let map: HashMap<&str, &Arc<dyn Tool>> = tools.iter().map(|t| (t.name(), t)).collect();
    let mut opts = AgentOptions::chat(8);
    opts.approval = ApprovalPolicy::auto(["fs_write".to_string()]);
    let phase = ToolPhase {
        tool_map: &map,
        ctx: &ctx(),
        trace_id: None,
        opts: &opts,
        approver: None,
    };
    let mut sink = NullSink { events: Vec::new() };
    let mut plan = Plan::default();
    let mut detector = LoopDetector::default();
    let ToolPhaseOutcome::Executed { blocks, .. } = run_tool_calls(
        &phase,
        vec![call("1", "fs_write", "w")],
        &mut plan,
        &mut sink,
        &mut detector,
    )
    .await
    .unwrap() else {
        panic!("cancelled")
    };
    assert_eq!(contents(&blocks), vec!["done:w"]);
}

/// キャンセル時でも、**実際に走った read の観測は外部化**する（UI/監査に穴を空けない）。
#[tokio::test]
async fn cancellation_still_reports_completed_reads() {
    struct CancelApprover;

    #[async_trait::async_trait]
    impl Approver for CancelApprover {
        async fn decide(
            &self,
            _id: &str,
            _name: &str,
            _input: &serde_json::Value,
        ) -> ApprovalDecision {
            ApprovalDecision::Cancelled
        }
    }

    let mut writer = ProbeTool::new("fs_write", false);
    writer.confirm = true;
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ProbeTool::new("doc_search", true)),
        Arc::new(writer),
    ];
    let map: HashMap<&str, &Arc<dyn Tool>> = tools.iter().map(|t| (t.name(), t)).collect();
    let opts = AgentOptions::chat(8);
    let approver = CancelApprover;
    let phase = ToolPhase {
        tool_map: &map,
        ctx: &ctx(),
        trace_id: None,
        opts: &opts,
        approver: Some(&approver),
    };
    let calls = vec![call("1", "doc_search", "a"), call("2", "fs_write", "w")];
    let mut sink = NullSink { events: Vec::new() };
    let mut plan = Plan::default();
    let mut detector = LoopDetector::default();
    let out = run_tool_calls(&phase, calls, &mut plan, &mut sink, &mut detector)
        .await
        .unwrap();
    assert!(matches!(out, ToolPhaseOutcome::Cancelled));
    // 完了した read の結果はイベントとして出ている（実行したのに無かったことにしない）。
    assert!(
        sink.events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResult { content, .. } if content.contains("done:a")
        )),
        "完了した read の ToolResult が出ていない"
    );
}
