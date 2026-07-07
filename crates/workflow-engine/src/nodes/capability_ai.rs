//! 能力呼び出しの本体（llm.invoke / agent.invoke / http.request）。node 経路と `Shiki.*` で共用。
//!
//! - llm/agent/http は外部 API のためレート制御を通す（`rate_limited` は attempt 非消費）。
//! - http.request は **宛先束縛（secret.allowed_hosts）× egress allowlist の AND** を URL ホスト部
//!   リテラルで照合し、リダイレクトは既定拒否（SSRF/DNS rebinding/近似ホストを弾く・PIT-36）。
//! - secret 平文・レスポンス本文は監査/journal に載せない（run 履歴に平文を出さない）。

use secrets::DestinationBinding;
use serde_json::{json, Value};

use crate::run::NodeContext;

use super::exec::CapabilityNodeExecutor;
use super::http::{check_destination, redirect_denied, summarize_response, HttpDenied};
use super::ports::{AgentInvokeReq, ExecCtx, HttpSendReq, LlmInvokeReq, PortError};
use super::resolver::{as_bytes, as_string, as_u32, resolve_field, ParamResolver};

/// レスポンス本文を JSON（不能ならテキスト）に整形する（1MB 上限）。
fn parse_body(bytes: &[u8]) -> Value {
    const CAP: usize = 1024 * 1024;
    let slice: &[u8] = if bytes.len() > CAP {
        &bytes[..CAP]
    } else {
        bytes
    };
    serde_json::from_slice::<Value>(slice)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(slice).into_owned()))
}

impl CapabilityNodeExecutor {
    // --- llm.invoke ------------------------------------------------------

    pub(super) async fn node_llm_invoke(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        self.rate_check(ec, "llm.invoke").await?;
        let prompt = resolve_field(params, "prompt", r)
            .and_then(|v| as_string(&v))
            .ok_or_else(|| PortError::invalid("llm.invoke: prompt がありません"))?;
        let model = resolve_field(params, "model", r).and_then(|v| as_string(&v));
        let system = resolve_field(params, "system", r).and_then(|v| as_string(&v));
        let max_tokens = resolve_field(params, "max_tokens", r).and_then(|v| as_u32(&v));
        let out = self
            .ports
            .llm_invoke(
                ec,
                LlmInvokeReq {
                    model: model.clone(),
                    system,
                    prompt,
                    max_tokens,
                    idempotency_key: ctx.idempotency_key.clone(),
                },
            )
            .await?;
        // prompt 本文は監査に載せない（モデル名のみ）。
        self.audit(
            &ec.tenant_id,
            "llm.invoke",
            true,
            &json!({ "model": model }),
        );
        Ok(out)
    }

    // --- agent.invoke ----------------------------------------------------

    pub(super) async fn node_agent_invoke(
        &self,
        params: &Value,
        _ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        self.rate_check(ec, "agent.invoke").await?;
        let code = resolve_field(params, "instruction", r)
            .and_then(|v| as_string(&v))
            .ok_or_else(|| PortError::invalid("agent.invoke: instruction がありません"))?;
        // egress_allowlist は縮小のみ（リテラル配列）。ポートが上限として spec を組む。
        let egress_allowlist: Vec<String> = params
            .get("egress_allowlist")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let timeout_ms = resolve_field(params, "max_duration_sec", r)
            .and_then(|v| as_u32(&v))
            .map(|s| u64::from(s) * 1000);
        let out = self
            .ports
            .agent_invoke(
                ec,
                AgentInvokeReq {
                    code,
                    timeout_ms,
                    egress_allowlist,
                },
            )
            .await?;
        self.audit(
            &ec.tenant_id,
            "agent.invoke",
            true,
            &json!({ "sandboxed": true }),
        );
        Ok(out)
    }

    // --- http.request ----------------------------------------------------

    pub(super) async fn node_http_request(
        &self,
        params: &Value,
        ctx: &NodeContext,
        ec: &ExecCtx,
        r: &ParamResolver<'_>,
    ) -> Result<Value, PortError> {
        self.rate_check(ec, "http.request").await?;

        let method = resolve_field(params, "method", r)
            .and_then(|v| as_string(&v))
            .unwrap_or_else(|| "GET".to_string());
        let base_url = resolve_field(params, "url", r)
            .and_then(|v| as_string(&v))
            .ok_or_else(|| PortError::invalid("http.request: url がありません"))?;
        let suffix = resolve_field(params, "path_suffix", r)
            .and_then(|v| as_string(&v))
            .unwrap_or_default();
        let url = format!("{base_url}{suffix}");

        // secret 解決（あれば）→ 宛先束縛の照合材料。
        let secret_spec = params.get("secret");
        let resolved_secret = if let Some(s) = secret_spec {
            let name = s
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| PortError::invalid("http.request: secret.name がありません"))?;
            Some(self.ports.resolve_secret(ec, name).await?)
        } else {
            None
        };

        // 宛先束縛 × egress allowlist の AND（URL ホスト部リテラル照合）。
        let host =
            self.check_http_destination(&url, resolved_secret.as_ref().map(|s| &s.allowed_hosts))?;

        // ヘッダ組み立て（secret 注入 ＋ Idempotency-Key）。平文はここだけに載る。
        let mut headers: Vec<(String, String)> = Vec::new();
        if let (Some(spec), Some(sec)) = (secret_spec, resolved_secret.as_ref()) {
            let value = String::from_utf8_lossy(&sec.plaintext).into_owned();
            let (hname, hval) = attach_secret(spec.get("attach"), &value);
            headers.push((hname, hval));
        }
        headers.push(("Idempotency-Key".to_string(), ctx.idempotency_key.clone()));

        let body = resolve_field(params, "body", r).map(|v| as_bytes(&v));
        let follow = resolve_field(params, "redirect", r)
            .and_then(|v| as_string(&v))
            .is_some_and(|s| s == "follow_stripped");

        let resp = self
            .ports
            .http_send(
                ec,
                HttpSendReq {
                    method: method.clone(),
                    url,
                    headers,
                    body,
                    follow_redirects: follow,
                    timeout_ms: Some(self.http_timeout_ms),
                },
            )
            .await?;

        // リダイレクトは既定拒否（follow_stripped 未指定時）。
        if !follow && redirect_denied(resp.status) {
            self.audit(
                &ec.tenant_id,
                "http.request",
                false,
                &json!({ "host": host, "status": resp.status, "reason": "redirect_denied" }),
            );
            return Err(PortError::new(
                "redirect_denied",
                "リダイレクトは既定で拒否されます",
                false,
            ));
        }

        // 監査/journal には本文を載せない（ステータス＋ホストのみ）。
        self.audit(
            &ec.tenant_id,
            "http.request",
            true,
            &summarize_response(resp.status, &host),
        );
        // 出力は次ノードへ本文を渡す（履歴・監査には出さない）。
        Ok(json!({ "status": resp.status, "host": host, "body": parse_body(&resp.body) }))
    }

    /// 宛先束縛（secret.allowed_hosts）× egress allowlist の AND を URL ホスト部リテラルで照合する。
    fn check_http_destination(
        &self,
        url: &str,
        secret_hosts: Option<&Vec<String>>,
    ) -> Result<String, PortError> {
        let map_denied = |d: HttpDenied| match d {
            HttpDenied::HostNotAllowed(h) => {
                PortError::forbidden(format!("宛先束縛外のホスト: {h}"))
            }
            HttpDenied::RedirectDenied => {
                PortError::new("redirect_denied", "リダイレクト拒否", false)
            }
            HttpDenied::BadUrl => PortError::invalid("URL が解析不能"),
            HttpDenied::BadScheme => PortError::invalid("http/https のみ許可"),
        };
        // 一段目: secret があれば secret 束縛、無ければ global allowlist で照合。
        let primary = secret_hosts
            .cloned()
            .unwrap_or_else(|| self.http_allowlist.clone());
        let binding = DestinationBinding::new(primary);
        let host = check_destination(url, &binding).map_err(map_denied)?;
        // 二段目: secret 利用時は global allowlist も AND（設定時のみ・空なら secret 束縛で十分）。
        if secret_hosts.is_some() && !self.http_allowlist.is_empty() {
            let global = DestinationBinding::new(self.http_allowlist.clone());
            if !global.allows(&host) {
                return Err(PortError::forbidden(format!(
                    "egress allowlist 外のホスト: {host}"
                )));
            }
        }
        Ok(host)
    }
}

/// secret 添付方式からヘッダ名/値を決める（`bearer` → Authorization: Bearer、`header` → 指定ヘッダ）。
fn attach_secret(attach: Option<&Value>, value: &str) -> (String, String) {
    let kind = attach
        .and_then(|a| a.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("bearer");
    match kind {
        "header" => {
            let name = attach
                .and_then(|a| a.get("header"))
                .and_then(Value::as_str)
                .unwrap_or("Authorization")
                .to_string();
            (name, value.to_string())
        }
        // 既定 bearer。
        _ => ("Authorization".to_string(), format!("Bearer {value}")),
    }
}
