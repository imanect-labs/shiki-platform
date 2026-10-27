//! 本番 `NodeExecutor`（能力ゲートウェイ → チョークポイント・Task 10.6a/10.8/10.10）。
//!
//! 各 node_type を **能力ゲートウェイ**（scope ceiling ∩ → effect_journal → rate limit → 監査）を
//! 通してから [`NodePorts`] 越しに既存チョークポイントへ dispatch する。認可（OpenFGA）はチョーク
//! ポイント側、scope ceiling は本 executor 側＝二重ゲート（個別ノードに認可検査を散らさない・INV-2）。
//!
//! - 制御ノード branch/switch は純関数（[`control`](crate::control)）で `taken_ports` を決める。
//!   join は pass-through（待ち合わせは readiness が担保）。wait/map は terminal 化せず durable に中断する
//!   （[`NodeResult::wait`]/[`NodeResult::map_fanout`] を返し、checkpoint 側が `waiting_*`/動的 fan-out を結線）。
//! - 能力呼び出しの本体は [`capability`](super::capability)（node 経路と script の `Shiki.*` 経路で共用）。

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use script_runtime::engine::{EngineHandle, Limits};
use serde_json::{json, Value};

use crate::capability::{check_scope_ceiling, CapabilityAudit, EffectJournal, ScopeCeiling};
use crate::control::eval::{resolve_deep, EvalError};
use crate::control::{bind_wait_filter, branch_port, switch_port};
use crate::ir::expr::ValueExpr;
use crate::ir::params::{
    self, BranchParams, MapItemError, MapParams, SwitchParams, WaitKind, WaitParams, WaitTimeout,
};
use crate::ratelimit::{BucketConfig, TokenBucket};
use crate::run::{
    MapFanout, NodeContext, NodeExecutor, NodeResult, OnItemError, OnTimeout, Suspend,
};
use crate::vocab::{EventSource, NodeType};

use super::capability_agent::AgentLimits;
use super::ports::{ExecCtx, NodePorts};
use super::resolver::ParamResolver;

/// 能力ノードの本番 executor（server 側でポート・journal・audit を注入）。
pub struct CapabilityNodeExecutor {
    pub(super) ports: Arc<dyn NodePorts>,
    pub(super) journal: EffectJournal,
    pub(super) audit: Arc<dyn CapabilityAudit>,
    /// 外部 API（llm/agent/http）のレート制御（未設定なら制限しない）。
    pub(super) ratelimit: Option<TokenBucket>,
    pub(super) ratelimit_cfg: BucketConfig,
    /// script.run 用エンジン（未設定なら script ノードは permanent 失敗）。
    pub(super) script_engine: Option<EngineHandle>,
    pub(super) script_limits: Limits,
    /// http.request の egress allowlist（secret 宛先束縛と AND・空なら secret 必須）。
    pub(super) http_allowlist: Vec<String>,
    pub(super) http_timeout_ms: u64,
    /// agent.invoke の上限と天井（デプロイ設定・BUG-6）。
    pub(super) agent_limits: AgentLimits,
    /// agent.invoke の同時実行の枠（プロセス内・`agent_limits.max_concurrent`）。
    pub(super) agent_slots: Arc<tokio::sync::Semaphore>,
}

impl CapabilityNodeExecutor {
    /// 最小構成（ratelimit/script/allowlist 無効）。server 側でビルダーで肉付けする。
    #[must_use]
    pub fn new(
        ports: Arc<dyn NodePorts>,
        journal: EffectJournal,
        audit: Arc<dyn CapabilityAudit>,
    ) -> Self {
        CapabilityNodeExecutor {
            ports,
            journal,
            audit,
            ratelimit: None,
            ratelimit_cfg: BucketConfig {
                capacity: 60,
                refill_per_sec: 1.0,
            },
            script_engine: None,
            script_limits: Limits::default(),
            http_allowlist: Vec::new(),
            http_timeout_ms: 30_000,
            agent_slots: Arc::new(tokio::sync::Semaphore::new(
                AgentLimits::default().max_concurrent,
            )),
            agent_limits: AgentLimits::default(),
        }
    }

    /// agent.invoke の上限・web の天井・同時実行の枠を設定する（BUG-6・D-29）。
    #[must_use]
    pub fn with_agent_limits(mut self, limits: AgentLimits) -> Self {
        self.agent_slots = Arc::new(tokio::sync::Semaphore::new(limits.max_concurrent.max(1)));
        self.agent_limits = limits;
        self
    }

    /// 外部 API のレート制御を有効化する。
    #[must_use]
    pub fn with_ratelimit(mut self, bucket: TokenBucket, cfg: BucketConfig) -> Self {
        self.ratelimit = Some(bucket);
        self.ratelimit_cfg = cfg;
        self
    }

    /// script.run 用エンジンを設定する（[`EngineHandle::shared`] なら、コンパイルの完了は script を実行する
    /// ノードだけが待つ）。
    #[must_use]
    pub fn with_script_engine(mut self, engine: impl Into<EngineHandle>, limits: Limits) -> Self {
        self.script_engine = Some(engine.into());
        self.script_limits = limits;
        self
    }

    /// http.request の egress allowlist（グローバル）を設定する。
    #[must_use]
    pub fn with_http_allowlist(mut self, hosts: Vec<String>, timeout_ms: u64) -> Self {
        self.http_allowlist = hosts;
        self.http_timeout_ms = timeout_ms;
        self
    }

    /// `ExecCtx`（ポート実装が AuthContext を組む素材）を NodeContext から作る。
    pub(super) fn exec_ctx(ctx: &NodeContext) -> ExecCtx {
        ExecCtx {
            tenant_id: ctx.tenant_id.clone(),
            org: ctx.org.clone(),
            principal: ctx.principal.clone(),
            principal_kind: ctx.principal_kind.clone(),
            trace_id: ctx.trace_id.clone(),
            run_id: Some(ctx.run_id),
            step_path: Some(ctx.step_path.clone()),
        }
    }

    /// scope ceiling ゲート: 操作の要求スコープ ∈ scope_ceiling を検証する。
    pub(super) fn check_ceiling(api: &str, ctx: &NodeContext) -> ScopeCeiling {
        let effective: BTreeSet<String> = ctx.scope_ceiling.iter().cloned().collect();
        check_scope_ceiling(api, &effective)
    }

    /// control.branch: 条件を評価して `true`/`false` ポートを確定する（型不一致などは step の失敗）。
    fn eval_branch(params: &Value, ctx: &NodeContext) -> NodeResult {
        let p: BranchParams = match params::parse(params) {
            Ok(p) => p,
            Err(e) => {
                return NodeResult::fail("bad_params", format!("branch params が不正: {e}"), false)
            }
        };
        let r = ParamResolver::new(ctx);
        match branch_port(&p.condition, &r) {
            Ok(port) => NodeResult::ok_port(ctx.input.clone(), port),
            Err(e) => eval_failed("branch の条件", &e),
        }
    }

    /// control.switch: value を各 case とリテラル一致で照合し、一致 port（無ければ default）を確定する。
    fn eval_switch(params: &Value, ctx: &NodeContext) -> NodeResult {
        let p: SwitchParams = match params::parse(params) {
            Ok(p) => p,
            Err(e) => {
                return NodeResult::fail("bad_params", format!("switch params が不正: {e}"), false)
            }
        };
        let cases: Vec<(String, Value)> = p.cases.into_iter().map(|c| (c.port, c.equals)).collect();
        let r = ParamResolver::new(ctx);
        match switch_port(&p.value, &cases, &r) {
            Ok(port) => NodeResult::ok_port(ctx.input.clone(), &port),
            Err(e) => eval_failed("switch の value", &e),
        }
    }

    /// control.wait: params から中断指示（timer/event）を組む（durable 化は checkpoint 側・engine.md §9）。
    fn eval_wait(params: &Value, ctx: &NodeContext) -> NodeResult {
        let p: WaitParams = match params::parse(params) {
            Ok(p) => p,
            Err(e) => {
                return NodeResult::fail("bad_params", format!("wait params が不正: {e}"), false)
            }
        };
        let r = ParamResolver::new(ctx);
        let now = Utc::now();
        // 書いてある値式が解決できなければ「無い」扱い（無期限待ちなど）にせず失敗させる。
        let resolve = |e: Option<&ValueExpr>| e.map(|e| resolve_deep(e, &r)).transpose();
        match p.kind {
            WaitKind::Duration => {
                let secs = match resolve(p.duration_sec.as_ref()) {
                    Ok(v) => v,
                    Err(e) => return eval_failed("wait の duration_sec", &e),
                };
                let Some(secs) = secs.as_ref().and_then(Value::as_i64) else {
                    return NodeResult::fail(
                        "bad_params",
                        "wait(duration) には整数の duration_sec が必要です",
                        false,
                    );
                };
                if secs < 0 {
                    return NodeResult::fail("bad_params", "duration_sec は非負", false);
                }
                // 過大値は checked 演算で拒否する（panic で worker task を殺さない）。
                let wake_at = Duration::try_seconds(secs).and_then(|d| now.checked_add_signed(d));
                let Some(wake_at) = wake_at else {
                    return NodeResult::fail("bad_params", "duration_sec が大きすぎます", false);
                };
                NodeResult::wait(Suspend::Timer { wake_at })
            }
            WaitKind::Until => {
                let until = match resolve(p.until.as_ref()) {
                    Ok(v) => v,
                    Err(e) => return eval_failed("wait の until", &e),
                };
                let Some(ts) = until.as_ref().and_then(Value::as_str).map(String::from) else {
                    return NodeResult::fail(
                        "bad_params",
                        "wait(until) には日時の文字列（RFC3339）の until が必要です",
                        false,
                    );
                };
                match DateTime::parse_from_rfc3339(&ts) {
                    Ok(dt) => NodeResult::wait(Suspend::Timer {
                        wake_at: dt.with_timezone(&Utc),
                    }),
                    Err(_) => NodeResult::fail(
                        "bad_params",
                        format!("until が RFC3339 でない: {ts}"),
                        false,
                    ),
                }
            }
            WaitKind::Event => {
                let Some(source) = p.source else {
                    return NodeResult::fail(
                        "bad_params",
                        "wait(event) に source がありません",
                        false,
                    );
                };
                let on_timeout = match p.on_timeout.unwrap_or_default() {
                    WaitTimeout::Continue => OnTimeout::Continue,
                    WaitTimeout::Fail => OnTimeout::Fail,
                };
                // timeout_sec 未指定は無期限待ち（timeout_at=None）。負値・過大値は不正として拒否する
                // （checked 演算で panic させず bad_params の permanent 失敗として checkpoint する）。
                let timeout = match resolve(p.timeout_sec.as_ref()) {
                    Ok(v) => v,
                    Err(e) => return eval_failed("wait の timeout_sec", &e),
                };
                let timeout_sec = match timeout.as_ref().map(Value::as_i64) {
                    None => None,
                    Some(Some(s)) => Some(s),
                    Some(None) => {
                        return NodeResult::fail(
                            "bad_params",
                            "wait(event) の timeout_sec は整数で指定してください",
                            false,
                        )
                    }
                };
                let timeout_at = match timeout_sec {
                    Some(s) if s < 0 => {
                        return NodeResult::fail("bad_params", "timeout_sec は非負", false)
                    }
                    Some(s) => {
                        let Some(at) =
                            Duration::try_seconds(s).and_then(|d| now.checked_add_signed(d))
                        else {
                            return NodeResult::fail(
                                "bad_params",
                                "timeout_sec が大きすぎます",
                                false,
                            );
                        };
                        Some(at)
                    }
                    None => None,
                };
                // イベント以外の参照（input・nodes.*・run・each）は停止時にリテラルへ束縛する（起床の判定は
                // 届いたイベントしか解決しない・GAP-07 の T3）。
                let filter = match p
                    .filter
                    .as_ref()
                    .map(|c| bind_wait_filter(c, &r))
                    .transpose()
                {
                    Ok(f) => f.and_then(|c| serde_json::to_value(c).ok()),
                    Err(e) => return eval_failed("wait の filter", &e),
                };
                NodeResult::wait(Suspend::Event {
                    source,
                    scope: p.scope.unwrap_or_else(|| json!({})),
                    filter,
                    timeout_at,
                    on_timeout,
                    suspend_rev: None,
                })
            }
        }
    }

    /// control.wait の本体: [`Self::eval_wait`] の中断指示に、データ変更の wait の束縛を足す（GAP-07 の T3）。
    ///
    /// データ変更の wait は `scope.record`（値式）を停止時に解決し、実行主体の権限でその行を読んで rev を
    /// `suspend_rev` に残す（これ以下の rev の変更＝停止より前の自分の書込では起きない）。行を読むのは
    /// data.get の能力として scope 天井を通す（要求スコープ data.read）。
    async fn node_wait(&self, params: &Value, ctx: &NodeContext) -> NodeResult {
        let mut out = Self::eval_wait(params, ctx);
        let Some(Suspend::Event {
            source,
            scope,
            suspend_rev,
            ..
        }) = out.suspend.as_mut()
        else {
            return out;
        };
        if !EventSource::parse(source).is_some_and(EventSource::is_data) {
            return out;
        }
        let table = scope
            .get("table")
            .and_then(Value::as_str)
            .and_then(|s| uuid::Uuid::parse_str(s).ok());
        let Some(table) = table else {
            return NodeResult::fail(
                "bad_params",
                "wait(event) の scope.table にテーブルの ID がありません",
                false,
            );
        };
        let record = match scope.get("record").cloned() {
            None | Some(Value::Null) => None,
            Some(raw) => {
                let expr: ValueExpr =
                    serde_json::from_value(raw).unwrap_or(ValueExpr::Literal(Value::Null));
                let r = ParamResolver::new(ctx);
                let id = match resolve_deep(&expr, &r) {
                    Ok(v) => v,
                    Err(e) => return eval_failed("wait の scope.record", &e),
                };
                match id.as_str().and_then(|s| uuid::Uuid::parse_str(s).ok()) {
                    Some(id) => Some(id),
                    None => {
                        return NodeResult::fail(
                            "bad_params",
                            "wait(event) の scope.record が行の ID になりません",
                            false,
                        )
                    }
                }
            }
        };
        let Some(record) = record else {
            // テーブル単位の待ち: 停止より前のイベントは checkpoint の Tx で outbox の位置を残して無視する。
            *scope = json!({ "table": table.to_string() });
            return out;
        };
        if let ScopeCeiling::Denied(_) = Self::check_ceiling("data.get", ctx) {
            self.audit.record_out_of_scope(
                &Self::exec_ctx(ctx),
                "data.get",
                &json!({ "reason": "out_of_scope", "step": ctx.step_path }),
            );
            return NodeResult::fail("out_of_scope", "scope_ceiling 外の操作: data.get", false);
        }
        match self
            .ports
            .data_get(&Self::exec_ctx(ctx), table, record)
            .await
        {
            Ok(rec) => {
                *suspend_rev = rec.get("rev").and_then(Value::as_i64);
                *scope = json!({ "table": table.to_string(), "record": record.to_string() });
                out
            }
            Err(e) => NodeResult::fail(&e.code, e.message, e.retryable),
        }
    }

    /// control.map: items を解決し動的 fan-out 指示を組む（要素挿入は checkpoint 側・engine.md §4.5）。
    fn eval_map(params: &Value, ctx: &NodeContext) -> NodeResult {
        let p: MapParams = match params::parse(params) {
            Ok(p) => p,
            Err(e) => {
                return NodeResult::fail("bad_params", format!("map params が不正: {e}"), false)
            }
        };
        let r = ParamResolver::new(ctx);
        // 配列リテラルの要素に置いた $from / $template も展開する（ir.md §3.1「任意の位置」）。
        let items_val = match resolve_deep(&p.items, &r) {
            Ok(v) => v,
            Err(e) => return eval_failed("map の items", &e),
        };
        let Some(items) = items_val.as_array() else {
            return NodeResult::fail("bad_params", "map の items が配列ではありません", false);
        };
        if items.len() > 1000 {
            return NodeResult::fail(
                "fanout_limit_exceeded",
                format!("map の要素数 {} が上限 1000 を超過", items.len()),
                false,
            );
        }
        let on_item_error = match p.on_item_error {
            MapItemError::Collect => OnItemError::Collect,
            MapItemError::FailMap => OnItemError::FailMap,
        };
        NodeResult::map_fanout(MapFanout {
            items: items.clone(),
            max_concurrency: p.max_concurrency.unwrap_or(10),
            on_item_error,
        })
    }
}

/// 制御ノードの値式・条件の評価エラーを permanent な step の失敗にする（同じ入力では直らない）。
fn eval_failed(what: &str, e: &EvalError) -> NodeResult {
    NodeResult::fail(e.code, format!("{what}: {}", e.message), false)
}

#[async_trait]
impl NodeExecutor for CapabilityNodeExecutor {
    async fn execute(&self, node_type: &str, params: &Value, ctx: &NodeContext) -> NodeResult {
        let Some(nt) = NodeType::parse(node_type) else {
            return NodeResult::fail(
                "unknown_node",
                format!("未知のノード種別: {node_type}"),
                false,
            );
        };

        // 制御ノード: taken_ports を純関数で決める（能力を呼ばない）。
        match nt {
            NodeType::ControlBranch => return Self::eval_branch(params, ctx),
            NodeType::ControlSwitch => return Self::eval_switch(params, ctx),
            NodeType::ControlJoin => return NodeResult::ok(ctx.input.clone()),
            // wait/map は terminal 化せず durable に中断する（checkpoint 側で waiting_*/fan-out へ）。
            NodeType::ControlWait => return self.node_wait(params, ctx).await,
            NodeType::ControlMap => return Self::eval_map(params, ctx),
            _ => {}
        }

        // 予約語彙（将来ノード）は保存時 V3 で弾かれるが、DB 直書き等で到達しても
        // fail-closed で拒否する（防御の多層化）。
        if !nt.available_stage_a() {
            return NodeResult::fail(
                "unsupported_stage_a",
                format!("{node_type} は Stage A 未実装（予約語彙）"),
                false,
            );
        }

        // scope ceiling ゲート（二重ゲートの一段目）。
        if let ScopeCeiling::Denied(_) = Self::check_ceiling(node_type, ctx) {
            self.audit.record_out_of_scope(
                &Self::exec_ctx(ctx),
                node_type,
                &json!({ "reason": "out_of_scope", "step": ctx.step_path }),
            );
            return NodeResult::fail(
                "out_of_scope",
                format!("scope_ceiling 外の操作: {node_type}"),
                false,
            );
        }

        // 承認は terminal 化せず durable に中断する（判断は承認者本人の FSM 遷移・GAP-08）。
        if nt == NodeType::HumanApproval {
            return self.node_human_approval(params, ctx).await;
        }
        // 結果を画面に返す（値を整形するだけの pure ノード・GAP-09）。
        if nt == NodeType::WorkflowReply {
            return super::reply::execute(params, ctx);
        }
        // 判定は答えで出口を選ぶ（NodeResult を直接返す・FEAT-1）。
        if nt == NodeType::AiDecide {
            return self.node_ai_decide(params, ctx).await;
        }

        let ec = Self::exec_ctx(ctx);
        let r = ParamResolver::new(ctx);

        let out = match nt {
            NodeType::StorageRead => self.node_storage_read(params, ctx, &ec, &r).await,
            NodeType::StorageWrite => self.node_storage_write(params, ctx, &ec, &r).await,
            NodeType::StorageList => self.node_storage_list(params, ctx, &ec, &r).await,
            NodeType::RagSearch => self.node_rag_search(params, ctx, &ec, &r).await,
            NodeType::LlmInvoke => self.node_llm_invoke(params, ctx, &ec, &r).await,
            NodeType::AgentInvoke => self.node_agent_invoke(params, ctx, &ec, &r).await,
            NodeType::HttpRequest => self.node_http_request(params, ctx, &ec, &r).await,
            NodeType::ScriptRun => self.node_script_run(params, ctx, &ec).await,
            NodeType::SkillInvoke => self.node_skill_invoke(params, ctx, &ec, &r).await,
            NodeType::WorkflowStart => self.node_workflow_start(params, ctx, &ec, &r).await,
            NodeType::CsvQuery => self.node_csv_query(params, ctx, &ec, &r).await,
            NodeType::CsvPatch => self.node_csv_patch(params, ctx, &ec, &r).await,
            NodeType::CsvWrite => self.node_csv_write(params, ctx, &ec, &r).await,
            NodeType::DataQuery => self.node_data_query(params, &ec, &r).await,
            NodeType::DataRecordCreate
            | NodeType::DataRecordUpdate
            | NodeType::DataRecordDelete
            | NodeType::DataTransition
            | NodeType::DataBulkUpsert => {
                self.node_data_write(node_type, params, ctx, &ec, &r).await
            }
            // 制御ノードは上で return 済み・予約語彙は available_stage_a ゲートで返却済み。
            _ => unreachable!("制御ノード/予約語彙は上で処理済み"),
        };

        match out {
            Ok(v) => NodeResult::ok(v),
            Err(e) => NodeResult::fail(&e.code, e.message, e.retryable),
        }
    }
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
