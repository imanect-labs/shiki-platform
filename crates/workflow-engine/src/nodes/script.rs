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

/// `run_script_source` の引数束（script.run と skill.invoke で共用・7 引数制限対応）。
pub(super) struct ScriptRunReq<'a> {
    pub source: &'a str,
    pub audit_api: &'a str,
    pub input: Value,
    pub ctx: &'a NodeContext,
    pub ec: &'a ExecCtx,
    pub engine: Arc<script_runtime::engine::ScriptEngine>,
    pub ceiling_override: Option<Vec<String>>,
}

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
        // script.run は node の scope_ceiling をそのまま使う（絞り込みなし）。
        self.run_script_source(ScriptRunReq {
            source: &source,
            audit_api: "script.run",
            input,
            ctx,
            ec,
            engine,
            ceiling_override: None,
        })
        .await
    }

    /// shiki script 本文を script-runtime で 1 回実行する（script.run と skill.invoke の共用・#344）。
    ///
    /// `Shiki.*` ホスト呼び出しは同じ能力ゲートウェイ（scope ceiling → journal → 監査）へ合流する。
    ///
    /// `req.ceiling_override` が Some のとき、その集合を scope ceiling として使う
    /// （skill.invoke は「workflow ceiling ∩ skill 宣言スコープ」を渡し、広い workflow scope の
    /// 下でも skill script が宣言外の `Shiki.*` を呼べないようにする・レビュー指摘）。
    pub(super) async fn run_script_source(
        &self,
        req: ScriptRunReq<'_>,
    ) -> Result<Value, PortError> {
        let ScriptRunReq {
            source,
            audit_api,
            input,
            ctx,
            ec,
            engine,
            ceiling_override,
        } = req;
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
            ceiling: ceiling_override.unwrap_or_else(|| ctx.scope_ceiling.clone()),
            base_key: ctx.idempotency_key.clone(),
            http_allowlist: self.http_allowlist.clone(),
            http_timeout_ms: self.http_timeout_ms,
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
    /// http.request（Shiki.http.request）の egress allowlist（executor と同一値・#344）。
    http_allowlist: Vec<String>,
    http_timeout_ms: u64,
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
            // http.request（実行時は args が concrete のため宛先束縛を照合できる・#344/10.15）。
            "http.request" => self.http_request(a, &call_key).await,
            other => Err(PortError::invalid(format!("未知の Shiki API: {other}"))),
        }
    }

    /// Shiki.http.request（宛先束縛 × egress allowlist の AND・リダイレクト拒否・#344/10.15）。
    ///
    /// ノード経路（`node_http_request`）と同じ防御: secret 添付時は URL ホストを
    /// `secret.allowed_hosts`（∩ グローバル allowlist）とリテラル照合し、3xx は一律拒否する。
    /// secret 平文・レスポンス本文は監査に載せない（status + host のみ）。
    async fn http_request(&self, a: &Value, call_key: &str) -> Result<Value, PortError> {
        use super::http::{check_destination, redirect_denied, summarize_response, HttpDenied};
        use secrets::DestinationBinding;

        let method = a
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let url = a
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| PortError::invalid("http.request: url がありません"))?
            .to_string();
        let secret_name = a
            .get("secret")
            .and_then(|s| s.get("name"))
            .and_then(Value::as_str);
        let resolved = match secret_name {
            Some(name) => Some(self.ports.resolve_secret(&self.ec, name).await?),
            None => None,
        };

        // 宛先束縛（secret 有り）または global allowlist（無し）で照合し、secret 有りは AND。
        // 拒否は監査に残す（scripted egress の遮断も監査可能に・レビュー指摘）。
        let audit_denied = |reason: &str, detail: &str| {
            self.audit.record(
                &self.ec.tenant_id,
                "http.request",
                false,
                &json!({ "reason": reason, "detail": detail, "via": "script" }),
            );
        };
        let map_denied = |d: HttpDenied| match d {
            HttpDenied::HostNotAllowed(h) => {
                audit_denied("host_not_allowed", &h);
                PortError::forbidden(format!("宛先束縛外のホスト: {h}"))
            }
            HttpDenied::RedirectDenied => {
                audit_denied("redirect_denied", "");
                PortError::new("redirect_denied", "リダイレクト拒否", false)
            }
            HttpDenied::BadUrl => {
                audit_denied("bad_url", "");
                PortError::invalid("URL が解析不能")
            }
            HttpDenied::BadScheme => {
                audit_denied("bad_scheme", "");
                PortError::invalid("http/https のみ許可")
            }
        };
        let primary = resolved
            .as_ref()
            .map_or_else(|| self.http_allowlist.clone(), |s| s.allowed_hosts.clone());
        let host =
            check_destination(&url, &DestinationBinding::new(primary)).map_err(map_denied)?;
        if resolved.is_some() && !self.http_allowlist.is_empty() {
            let global = DestinationBinding::new(self.http_allowlist.clone());
            if !global.allows(&host) {
                audit_denied("egress_allowlist", &host);
                return Err(PortError::forbidden(format!(
                    "egress allowlist 外のホスト: {host}"
                )));
            }
        }

        let mut headers: Vec<(String, String)> = Vec::new();
        if let Some(sec) = resolved.as_ref() {
            let value = String::from_utf8_lossy(&sec.plaintext).into_owned();
            let attach = a
                .get("secret")
                .and_then(|s| s.get("attach"))
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let (hname, hval) = super::capability_ai::attach_secret(attach.as_ref(), &value);
            headers.push((hname, hval));
        }
        headers.push(("Idempotency-Key".to_string(), call_key.to_string()));
        if a.get("body").is_some() {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
        }
        let body = a.get("body").map(|v| match v {
            Value::String(s) => s.clone().into_bytes(),
            other => other.to_string().into_bytes(),
        });

        let resp = self
            .ports
            .http_send(
                &self.ec,
                super::ports::HttpSendReq {
                    method,
                    url,
                    headers,
                    body,
                    follow_redirects: false,
                    timeout_ms: Some(self.http_timeout_ms),
                },
            )
            .await?;
        if redirect_denied(resp.status) {
            self.audit.record(
                &self.ec.tenant_id,
                "http.request",
                false,
                &json!({ "host": host, "status": resp.status, "reason": "redirect_denied", "via": "script" }),
            );
            return Err(PortError::new(
                "redirect_denied",
                "リダイレクトは拒否されます（非追従）",
                false,
            ));
        }
        let mut meta = summarize_response(resp.status, &host);
        meta["via"] = json!("script");
        self.audit
            .record(&self.ec.tenant_id, "http.request", true, &meta);
        Ok(json!({
            "status": resp.status,
            "host": host,
            "body": super::capability_ai::parse_body(&resp.body),
        }))
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
