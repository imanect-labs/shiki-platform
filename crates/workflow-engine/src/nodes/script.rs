//! script.run ノード＋`Shiki.*` ホスト呼び出しブリッジ（Task 10.7/10.8・script.md §4-6）。
//!
//! script-runtime（swc→QuickJS/wasmtime）で 1 回実行し、ゲスト内の `Shiki.*` 同期呼び出しを
//! **同じ能力ゲートウェイ**（scope ceiling → effect_journal → 監査）へ合流させる（INV-1/INV-2）。
//! ホスト呼び出しは効果的呼び出し連番 `seq` で冪等キーを `<step冪等キー>#c<seq>` と派生し、
//! storage.write / workflow.start の高々 1 回を保つ。engine.run は同期ブロッキングのため
//! `spawn_blocking` で回し、非同期ポートは捕捉した `Handle` で `block_on` する（gRPC サーバと同型）。

use std::collections::BTreeSet;
use std::sync::Arc;

use script_runtime::engine::HostFn;
use script_runtime::host::{HostCall, HostResponse};
use serde_json::{json, Value};

use crate::capability::{
    check_scope_ceiling, op_digest, CapabilityAudit, EffectJournal, JournalDecision, ScopeCeiling,
};
use crate::run::NodeContext;

use crate::control::eval::resolve_value;
use crate::ir::params::ScriptRunParams;

use super::capability::parse_params;
use super::exec::CapabilityNodeExecutor;
use super::ports::{ExecCtx, NodePorts, PortError, StorageWriteReq};
use super::resolver::{as_bytes, ParamResolver};

impl CapabilityNodeExecutor {
    pub(super) async fn node_script_run(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
    ) -> Result<Value, PortError> {
        let engine = self
            .script_engine
            .clone()
            .ok_or_else(|| PortError::unavailable("script engine が未設定です"))?;

        // Stage A は inline のみ（`{ "artifact": "script:<name>@<ver>" }` は Stage B）。
        let p: ScriptRunParams = parse_params(params)?;
        let source = p.source.inline.clone().ok_or_else(|| {
            PortError::invalid("script.run: source.inline がありません（artifact 参照は Stage B）")
        })?;
        let r = ParamResolver::new(ctx);
        let input = p
            .input
            .as_ref()
            .and_then(|e| resolve_value(e, &r))
            .unwrap_or_else(|| ctx.input.clone());
        self.run_script_source(&source, "script.run", input, ctx, ec, engine)
            .await
    }

    /// shiki script 本文を script-runtime で 1 回実行する（script.run と skill.invoke の共用・#344）。
    ///
    /// `Shiki.*` ホスト呼び出しは同じ能力ゲートウェイ（scope ceiling → journal → 監査）へ合流する。
    pub(super) async fn run_script_source(
        &self,
        source: &str,
        audit_api: &str,
        input: Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        engine: Arc<script_runtime::engine::ScriptEngine>,
    ) -> Result<Value, PortError> {
        let compiled = script_runtime::compile::compile(source)
            .map_err(|e| PortError::invalid(format!("script コンパイル失敗: {e}")))?;

        let input_json = input.to_string();
        let limits = self.script_limits;
        let exec_id = format!("{}:{}", ctx.run_id.simple(), ctx.step_path);

        let bridge = HostBridge {
            ports: Arc::clone(&self.ports),
            journal: self.journal.clone(),
            audit: Arc::clone(&self.audit),
            ec: ec.clone(),
            ceiling: ctx.scope_ceiling.clone(),
            base_key: ctx.idempotency_key.clone(),
        };
        let handle = tokio::runtime::Handle::current();
        let compiled_js = compiled.compiled_js;
        let outcome = tokio::task::spawn_blocking(move || {
            let host_fn: HostFn =
                Box::new(move |call: &HostCall| handle.block_on(bridge.dispatch(call)));
            engine.run(&exec_id, &compiled_js, &input_json, limits, host_fn)
        })
        .await
        .map_err(|e| PortError::unavailable(format!("script 実行スレッド: {e}")))?;

        self.audit(
            &ec.tenant_id,
            audit_api,
            outcome.ok,
            &json!({ "termination": format!("{:?}", outcome.termination) }),
        );
        if outcome.ok {
            Ok(outcome.value.unwrap_or(Value::Null))
        } else {
            let (message, code, retryable) = outcome.error.unwrap_or_else(|| {
                (
                    "script 実行に失敗".to_string(),
                    "script_error".to_string(),
                    false,
                )
            });
            Err(PortError::new(&code, message, retryable))
        }
    }
}

/// `Shiki.*` を能力ゲートウェイへ橋渡しする（`spawn_blocking` 内から `block_on` される）。
#[derive(Clone)]
struct HostBridge {
    ports: Arc<dyn NodePorts>,
    journal: EffectJournal,
    audit: Arc<dyn CapabilityAudit>,
    ec: ExecCtx,
    ceiling: Vec<String>,
    base_key: String,
}

impl HostBridge {
    async fn dispatch(&self, call: &HostCall) -> HostResponse {
        // log はエンジン側で消費されるが、防御的に受ける。context はメタを返す。
        match call.api.as_str() {
            "log" => return HostResponse::Ok(Value::Null),
            "context" => {
                return HostResponse::Ok(json!({
                    "tenant_id": self.ec.tenant_id,
                    "org": self.ec.org,
                    "principal": self.ec.principal,
                }))
            }
            _ => {}
        }
        // scope ceiling ゲート（script 経路のホスト呼び出しも declared_scopes 内に縛る）。
        let effective: BTreeSet<String> = self.ceiling.iter().cloned().collect();
        if let ScopeCeiling::Denied(_) = check_scope_ceiling(&call.api, &effective) {
            self.audit.record(
                &self.ec.tenant_id,
                &call.api,
                false,
                &json!({ "reason": "out_of_scope", "via": "script" }),
            );
            return HostResponse::Err {
                message: format!("scope_ceiling 外の Shiki 呼び出し: {}", call.api),
                code: "out_of_scope".to_string(),
                retryable: false,
            };
        }
        match self.dispatch_capability(call).await {
            Ok(v) => HostResponse::Ok(v),
            Err(e) => HostResponse::Err {
                message: e.message,
                code: e.code,
                retryable: e.retryable,
            },
        }
    }

    /// ホスト呼び出し 1 件を能力ポートへ dispatch（args は concrete）。副作用は seq で冪等化。
    async fn dispatch_capability(&self, call: &HostCall) -> Result<Value, PortError> {
        let a = &call.args;
        let call_key = format!("{}#c{}", self.base_key, call.seq);
        match call.api.as_str() {
            "storage.read" => {
                let id = a
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .ok_or_else(|| PortError::invalid("storage.read: id が UUID ではありません"))?;
                let out = self.ports.storage_read(&self.ec, id).await?;
                self.audit.record(
                    &self.ec.tenant_id,
                    "storage.read",
                    true,
                    &json!({ "via": "script" }),
                );
                Ok(out)
            }
            "storage.list" => {
                let parent = a
                    .get("parent")
                    .and_then(Value::as_str)
                    .and_then(|s| uuid::Uuid::parse_str(s).ok());
                let out = self.ports.storage_list(&self.ec, parent).await?;
                self.audit.record(
                    &self.ec.tenant_id,
                    "storage.list",
                    true,
                    &json!({ "via": "script" }),
                );
                Ok(out)
            }
            "storage.write" => {
                let name = a
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| PortError::invalid("storage.write: name がありません"))?
                    .to_string();
                let parent = a
                    .get("parent")
                    .and_then(Value::as_str)
                    .and_then(|s| uuid::Uuid::parse_str(s).ok());
                let content = a.get("content").cloned().unwrap_or(Value::Null);
                let bytes = as_bytes(&content);
                let content_type = a
                    .get("contentType")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let digest = op_digest(
                    "storage.write",
                    &json!({ "parent": parent.map(|p| p.to_string()), "name": name, "content_sha256": sha_hex(&bytes) }),
                );
                let out = self
                    .ports
                    .storage_write(
                        &self.ec,
                        StorageWriteReq {
                            parent_id: parent,
                            name,
                            bytes,
                            content_type,
                            idempotency_key: call_key,
                            op_digest: digest,
                        },
                    )
                    .await?;
                self.audit.record(
                    &self.ec.tenant_id,
                    "storage.write",
                    true,
                    &json!({ "via": "script" }),
                );
                Ok(out)
            }
            "rag.search" => {
                let query = a
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or_else(|| PortError::invalid("rag.search: query がありません"))?;
                let top_k = a
                    .get("topK")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok());
                let out = self.ports.rag_search(&self.ec, query, top_k).await?;
                self.audit.record(
                    &self.ec.tenant_id,
                    "rag.search",
                    true,
                    &json!({ "via": "script" }),
                );
                Ok(out)
            }
            "workflow.start" => {
                let name = a
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| PortError::invalid("workflow.start: name がありません"))?;
                let input = a.get("input").cloned().unwrap_or(Value::Null);
                self.workflow_start_journaled(&call_key, name, &input).await
            }
            // http.request from script は Stage A 未対応（宛先束縛の concrete 経路は後続）。
            "http.request" => Err(PortError::new(
                "unsupported_stage_a",
                "Shiki.http.request は Stage A の script では未対応",
                false,
            )),
            other => Err(PortError::invalid(format!("未知の Shiki API: {other}"))),
        }
    }

    /// workflow.start（cross-TX effect_journal で start-once）。
    async fn workflow_start_journaled(
        &self,
        key: &str,
        name: &str,
        input: &Value,
    ) -> Result<Value, PortError> {
        let digest = op_digest("workflow.start", &json!({ "name": name, "input": input }));
        match self
            .journal
            .check(&self.ec.tenant_id, key, &digest)
            .await
            .map_err(|e| PortError::unavailable(format!("journal: {e}")))?
        {
            JournalDecision::Proceed => {
                let out = self.ports.workflow_start(&self.ec, name, input).await?;
                self.journal
                    .record(&self.ec.tenant_id, key, &digest, &out)
                    .await
                    .map_err(|e| PortError::unavailable(format!("journal record: {e}")))?;
                self.audit.record(
                    &self.ec.tenant_id,
                    "workflow.start",
                    true,
                    &json!({ "via": "script", "name": name }),
                );
                Ok(out)
            }
            JournalDecision::AlreadyDone(v) => Ok(v),
            JournalDecision::InProgress => Err(PortError::new(
                "effect_in_progress",
                "別ワーカーが起動処理中",
                true,
            )),
            JournalDecision::DigestMismatch => Err(PortError::new(
                "effect_conflict",
                "同一冪等キーで別の起動要求",
                false,
            )),
        }
    }
}

/// バイト列の sha256（16 進）。storage.write の op_digest 素材（script 経路）。
pub(super) fn sha_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}
