//! 能力呼び出しの本体（llm.invoke / agent.invoke / http.request）。node 経路と `Shiki.*` で共用。
//!
//! - llm/agent/http は外部 API のためレート制御を通す（`rate_limited` は attempt 非消費）。
//! - http.request は **宛先束縛（secret.allowed_hosts）× egress allowlist の AND** を URL ホスト部
//!   リテラルで照合し、リダイレクトは既定拒否（SSRF/DNS rebinding/近似ホストを弾く・PIT-36）。
//! - secret 平文・レスポンス本文は監査/journal に載せない（run 履歴に平文を出さない）。

use secrets::DestinationBinding;
use serde_json::{json, Value};

use crate::control::eval::resolve_value;
use crate::ir::params::{
    AgentInvokeParams, HttpRequestParams, LlmInvokeParams, SecretAttach, SecretAttachKind,
};
use crate::run::NodeContext;

use super::capability::parse_params;
use super::exec::CapabilityNodeExecutor;
use super::http::{check_destination, redirect_denied, summarize_response, HttpDenied};
use super::ports::{AgentInvokeReq, ExecCtx, HttpSendReq, LlmInvokeReq, PortError};
use super::resolver::{as_bytes, as_string, as_u32, ParamResolver};

/// レスポンス本文を JSON（不能ならテキスト）に整形する（1MB 上限）。
pub(super) fn parse_body(bytes: &[u8]) -> Value {
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
        let p: LlmInvokeParams = parse_params(params)?;
        let prompt = resolve_value(&p.prompt, r)
            .and_then(|v| as_string(&v))
            .ok_or_else(|| PortError::invalid("llm.invoke: prompt が解決できません"))?;
        let model = p.model.clone();
        let system = p
            .system
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .and_then(|v| as_string(&v));
        let max_tokens = p
            .max_tokens
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .and_then(|v| as_u32(&v));
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
        let p: AgentInvokeParams = parse_params(params)?;
        let code = resolve_value(&p.instruction, r)
            .and_then(|v| as_string(&v))
            .ok_or_else(|| PortError::invalid("agent.invoke: instruction が解決できません"))?;
        // egress_allowlist は縮小のみ（リテラル配列）。ポートが上限として spec を組む。
        let egress_allowlist = p.egress_allowlist.clone();
        let timeout_ms = p
            .max_duration_sec
            .as_ref()
            .and_then(|e| resolve_value(e, r))
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

        let p: HttpRequestParams = parse_params(params)?;
        let method = p.method.unwrap_or_default().as_str().to_string();
        let suffix = p
            .path_suffix
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .and_then(|v| as_string(&v))
            .unwrap_or_default();
        let url = format!("{}{suffix}", p.url);

        // secret 解決（あれば）→ 宛先束縛の照合材料。
        let resolved_secret = if let Some(sref) = &p.secret {
            Some(self.ports.resolve_secret(ec, &sref.name).await?)
        } else {
            None
        };

        // 宛先束縛 × egress allowlist の AND（URL ホスト部リテラル照合）。
        let host =
            self.check_http_destination(&url, resolved_secret.as_ref().map(|s| &s.allowed_hosts))?;

        // ヘッダ組み立て（secret 注入 ＋ Idempotency-Key）。平文はここだけに載る。
        let mut headers: Vec<(String, String)> = Vec::new();
        if let (Some(sref), Some(sec)) = (&p.secret, resolved_secret.as_ref()) {
            let value = String::from_utf8_lossy(&sec.plaintext).into_owned();
            let (hname, hval) = attach_secret(sref.attach.as_ref(), &value);
            headers.push((hname, hval));
        }
        headers.push(("Idempotency-Key".to_string(), ctx.idempotency_key.clone()));

        let body = p
            .body
            .as_ref()
            .and_then(|e| resolve_value(e, r))
            .map(|v| as_bytes(&v));

        // Stage A は **常にリダイレクト非追従**（3xx は拒否）。`follow_stripped` は追従先の宛先束縛
        // 再照合（各ホップの host ∈ binding）が要るため後続で実装するまで fail-closed で扱う
        // （auto-follow は SSRF/内部 IP rebind の窓になるため絶対に有効化しない）。
        let resp = self
            .ports
            .http_send(
                ec,
                HttpSendReq {
                    method: method.clone(),
                    url,
                    headers,
                    body,
                    follow_redirects: false,
                    timeout_ms: Some(self.http_timeout_ms),
                },
            )
            .await?;

        if redirect_denied(resp.status) {
            self.audit(
                &ec.tenant_id,
                "http.request",
                false,
                &json!({ "host": host, "status": resp.status, "reason": "redirect_denied" }),
            );
            return Err(PortError::new(
                "redirect_denied",
                "リダイレクトは拒否されます（Stage A は非追従）",
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
pub(super) fn attach_secret(attach: Option<&SecretAttach>, value: &str) -> (String, String) {
    match attach.map_or(SecretAttachKind::Bearer, |a| a.kind) {
        SecretAttachKind::Header => {
            let name = attach
                .and_then(|a| a.header.clone())
                .unwrap_or_else(|| "Authorization".to_string());
            (name, value.to_string())
        }
        SecretAttachKind::Bearer => ("Authorization".to_string(), format!("Bearer {value}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{attach_secret, parse_body};
    use crate::ir::params::{SecretAttach, SecretAttachKind};
    use serde_json::json;

    #[test]
    fn parse_body_json_text_and_cap() {
        // 有効 JSON はそのまま Value に。
        assert_eq!(parse_body(br#"{"a":1}"#), json!({ "a": 1 }));
        // 非 JSON はテキスト（String）へフォールバック。
        assert_eq!(parse_body(b"not json"), json!("not json"));
        // 1MB 超は先頭 1MB だけ見る（String 化される非 JSON で長さを確認）。
        let big = vec![b'x'; 1024 * 1024 + 500];
        let v = parse_body(&big);
        assert_eq!(
            v.as_str().map(str::len),
            Some(1024 * 1024),
            "CAP で切り詰め"
        );
    }

    #[test]
    fn attach_secret_bearer_default_and_explicit() {
        // None（既定）と明示 Bearer はどちらも Authorization: Bearer。
        assert_eq!(
            attach_secret(None, "tok"),
            ("Authorization".to_string(), "Bearer tok".to_string())
        );
        let bearer = SecretAttach {
            kind: SecretAttachKind::Bearer,
            header: None,
        };
        assert_eq!(
            attach_secret(Some(&bearer), "tok"),
            ("Authorization".to_string(), "Bearer tok".to_string())
        );
    }

    #[test]
    fn attach_secret_header_named_and_defaulted() {
        let named = SecretAttach {
            kind: SecretAttachKind::Header,
            header: Some("X-Api-Key".to_string()),
        };
        assert_eq!(
            attach_secret(Some(&named), "tok"),
            ("X-Api-Key".to_string(), "tok".to_string())
        );
        // header 省略時は Authorization に生値（Bearer 前置なし）。
        let unnamed = SecretAttach {
            kind: SecretAttachKind::Header,
            header: None,
        };
        assert_eq!(
            attach_secret(Some(&unnamed), "tok"),
            ("Authorization".to_string(), "tok".to_string())
        );
    }
}
