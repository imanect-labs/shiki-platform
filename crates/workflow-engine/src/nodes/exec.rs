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
use script_runtime::engine::{Limits, ScriptEngine};
use serde_json::{json, Value};

use crate::capability::{check_scope_ceiling, CapabilityAudit, EffectJournal, ScopeCeiling};
use crate::control::{branch_port, switch_port};
use crate::ir::expr::{Condition, ValueExpr};
use crate::ratelimit::{BucketConfig, TokenBucket};
use crate::run::{
    MapFanout, NodeContext, NodeExecutor, NodeResult, OnItemError, OnTimeout, Suspend,
};
use crate::vocab::NodeType;

use super::ports::{ExecCtx, NodePorts};
use super::resolver::{resolve_field, ParamResolver};

/// 能力ノードの本番 executor（server 側でポート・journal・audit を注入）。
pub struct CapabilityNodeExecutor {
    pub(super) ports: Arc<dyn NodePorts>,
    pub(super) journal: EffectJournal,
    pub(super) audit: Arc<dyn CapabilityAudit>,
    /// 外部 API（llm/agent/http）のレート制御（未設定なら制限しない）。
    pub(super) ratelimit: Option<TokenBucket>,
    pub(super) ratelimit_cfg: BucketConfig,
    /// script.run 用エンジン（未設定なら script ノードは permanent 失敗）。
    pub(super) script_engine: Option<Arc<ScriptEngine>>,
    pub(super) script_limits: Limits,
    /// http.request の egress allowlist（secret 宛先束縛と AND・空なら secret 必須）。
    pub(super) http_allowlist: Vec<String>,
    pub(super) http_timeout_ms: u64,
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
        }
    }

    /// 外部 API のレート制御を有効化する。
    #[must_use]
    pub fn with_ratelimit(mut self, bucket: TokenBucket, cfg: BucketConfig) -> Self {
        self.ratelimit = Some(bucket);
        self.ratelimit_cfg = cfg;
        self
    }

    /// script.run 用エンジンを設定する。
    #[must_use]
    pub fn with_script_engine(mut self, engine: Arc<ScriptEngine>, limits: Limits) -> Self {
        self.script_engine = Some(engine);
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
        }
    }

    /// scope ceiling ゲート: 操作の要求スコープ ∈ scope_ceiling を検証する。
    pub(super) fn check_ceiling(api: &str, ctx: &NodeContext) -> ScopeCeiling {
        let effective: BTreeSet<String> = ctx.scope_ceiling.iter().cloned().collect();
        check_scope_ceiling(api, &effective)
    }

    /// control.branch: 条件を評価して `true`/`false` ポートを確定する。
    fn eval_branch(params: &Value, ctx: &NodeContext) -> NodeResult {
        let Some(cond) = params.get("condition") else {
            return NodeResult::fail("bad_params", "branch に condition がありません", false);
        };
        let cond: Condition = match serde_json::from_value(cond.clone()) {
            Ok(c) => c,
            Err(e) => {
                return NodeResult::fail("bad_params", format!("condition が不正: {e}"), false)
            }
        };
        let r = ParamResolver::new(ctx);
        NodeResult::ok_port(ctx.input.clone(), branch_port(&cond, &r))
    }

    /// control.switch: value を各 case とリテラル一致で照合し、一致 port（無ければ default）を確定する。
    fn eval_switch(params: &Value, ctx: &NodeContext) -> NodeResult {
        let Some(value_raw) = params.get("value") else {
            return NodeResult::fail("bad_params", "switch に value がありません", false);
        };
        let value_expr: ValueExpr = match serde_json::from_value(value_raw.clone()) {
            Ok(v) => v,
            Err(e) => return NodeResult::fail("bad_params", format!("value が不正: {e}"), false),
        };
        // cases: [{ "port": "...", "equals": <literal> }, ...]
        let cases: Vec<(String, Value)> = params
            .get("cases")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        let port = c.get("port")?.as_str()?.to_string();
                        let eq = c.get("equals")?.clone();
                        Some((port, eq))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let r = ParamResolver::new(ctx);
        let port = switch_port(&value_expr, &cases, &r);
        NodeResult::ok_port(ctx.input.clone(), &port)
    }

    /// control.wait: params から中断指示（timer/event）を組む（durable 化は checkpoint 側・engine.md §9）。
    fn eval_wait(params: &Value, ctx: &NodeContext) -> NodeResult {
        let r = ParamResolver::new(ctx);
        let now = Utc::now();
        match params.get("kind").and_then(Value::as_str).unwrap_or("") {
            "duration" => {
                let Some(secs) = resolve_field(params, "duration_sec", &r).and_then(|v| v.as_i64())
                else {
                    return NodeResult::fail(
                        "bad_params",
                        "wait(duration) に duration_sec がありません",
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
            "until" => {
                let Some(ts) =
                    resolve_field(params, "until", &r).and_then(|v| v.as_str().map(String::from))
                else {
                    return NodeResult::fail(
                        "bad_params",
                        "wait(until) に until がありません",
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
            "event" => {
                let Some(source) = params.get("source").and_then(Value::as_str) else {
                    return NodeResult::fail(
                        "bad_params",
                        "wait(event) に source がありません",
                        false,
                    );
                };
                let on_timeout = match params.get("on_timeout").and_then(Value::as_str) {
                    Some("continue") => OnTimeout::Continue,
                    _ => OnTimeout::Fail,
                };
                // timeout_sec 未指定は無期限待ち（timeout_at=None）。負値・過大値は不正として拒否する。
                let timeout_at =
                    match resolve_field(params, "timeout_sec", &r).and_then(|v| v.as_i64()) {
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
                NodeResult::wait(Suspend::Event {
                    source: source.to_string(),
                    scope: params.get("scope").cloned().unwrap_or_else(|| json!({})),
                    filter: params.get("filter").cloned(),
                    timeout_at,
                    on_timeout,
                })
            }
            other => NodeResult::fail("bad_params", format!("未知の wait kind: {other}"), false),
        }
    }

    /// control.map: items を解決し動的 fan-out 指示を組む（要素挿入は checkpoint 側・engine.md §4.5）。
    fn eval_map(params: &Value, ctx: &NodeContext) -> NodeResult {
        let r = ParamResolver::new(ctx);
        let Some(items_val) = resolve_field(params, "items", &r) else {
            return NodeResult::fail("expr_resolve_error", "map の items が解決できません", false);
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
        let max_concurrency = resolve_field(params, "max_concurrency", &r)
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(10);
        let on_item_error = match params.get("on_item_error").and_then(Value::as_str) {
            Some("collect") => OnItemError::Collect,
            _ => OnItemError::FailMap,
        };
        NodeResult::map_fanout(MapFanout {
            items: items.clone(),
            max_concurrency,
            on_item_error,
        })
    }
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
            NodeType::ControlWait => return Self::eval_wait(params, ctx),
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
            self.audit.record(
                &ctx.tenant_id,
                node_type,
                false,
                &json!({ "reason": "out_of_scope", "step": ctx.step_path }),
            );
            return NodeResult::fail(
                "out_of_scope",
                format!("scope_ceiling 外の操作: {node_type}"),
                false,
            );
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
            NodeType::WorkflowStart => self.node_workflow_start(params, ctx, &ec, &r).await,
            // 制御ノードは上で return 済み・予約語彙は available_stage_a ゲートで返却済み。
            _ => unreachable!("制御ノード/予約語彙は上で処理済み"),
        };

        match out {
            Ok(v) => NodeResult::ok(v),
            Err(e) => NodeResult::fail(&e.code, e.message, e.retryable),
        }
    }
}
