//! 能力呼び出しの本体（llm.invoke / http.request）。node 経路と `Shiki.*` で共用。
//! agent.invoke は `capability_agent.rs`（ツールループ・BUG-6）。
//!
//! - llm/http は外部 API のためレート制御を通す（`rate_limited` は attempt 非消費）。
//! - http.request は **宛先束縛（secret.allowed_hosts）× egress allowlist の AND** を URL ホスト部
//!   リテラルで照合し、リダイレクトは既定拒否（SSRF/DNS rebinding/近似ホストを弾く・PIT-36）。
//! - secret 平文・レスポンス本文は監査/journal に載せない（run 履歴に平文を出さない）。

use secrets::DestinationBinding;
use serde_json::{json, Value};

use crate::ir::params::{HttpRequestParams, LlmInvokeParams, SecretAttach, SecretAttachKind};
use crate::run::NodeContext;

use super::capability::parse_params;
use super::exec::CapabilityNodeExecutor;
use super::http::{check_destination, redirect_denied, summarize_response, HttpDenied};
use super::ports::{ExecCtx, HttpSendReq, LlmInvokeReq, PortError, ResolvedSecretView};
use super::resolver::{as_bytes, as_string, as_u32, opt_param, param, typed, ParamResolver};

/// 出力に載せる本文の上限（バイト）。
const BODY_CAP: usize = 1024 * 1024;

/// レスポンス本文を JSON（不能ならテキスト）に整形する（1MB 上限）。
#[cfg(test)]
pub(super) fn parse_body(bytes: &[u8]) -> Value {
    response_body(bytes, None)
}

/// 外部の応答の本文を JSON（1MB 以下で読めるとき・不能ならテキスト）に整形し、反射した secret の平文を
/// `[REDACTED]` に置き換える（この呼び出しで解決した平文だけ・[`secrets::Redactor`] の形）。
///
/// テキストにする本文は、全体をマスクしてから 1MB に切る（切れ目が平文に掛かって前半だけが残らないように）。
pub(super) fn response_body(bytes: &[u8], secret: Option<&ResolvedSecretView>) -> Value {
    let redactor = reflected_redactor(secret);
    if bytes.len() <= BODY_CAP {
        if let Ok(v) = serde_json::from_slice::<Value>(bytes) {
            return match &redactor {
                Some(r) => r.redact_json(&v),
                None => v,
            };
        }
    }
    let text = String::from_utf8_lossy(bytes);
    let mut text = match &redactor {
        Some(r) => r.redact_str(&text),
        None => text.into_owned(),
    };
    if text.len() > BODY_CAP {
        let mut end = BODY_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Value::String(text)
}

/// llm_invoke ポートの出力から、実際に使ったモデルの名前を取る（ポートが返さなければ指定値）。
pub(super) fn resolved_model<'a>(out: &'a Value, requested: Option<&'a str>) -> Option<&'a str> {
    out.get("model").and_then(Value::as_str).or(requested)
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
        let what = "llm.invoke: prompt";
        let prompt = typed(&param(&p.prompt, r, what)?, what, "文字列", as_string)?;
        // 空文字は省略と同じ（組織の既定・保存時検証 V3 と揃える）。
        let model = p.model.clone().filter(|m| !m.is_empty());
        let what = "llm.invoke: system";
        let system = opt_param(p.system.as_ref(), r, what)?
            .map(|v| typed(&v, what, "文字列", as_string))
            .transpose()?;
        let what = "llm.invoke: max_tokens";
        let max_tokens = opt_param(p.max_tokens.as_ref(), r, what)?
            .map(|v| typed(&v, what, "0 以上の整数", as_u32))
            .transpose()?;
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
        // prompt 本文は監査に載せない（モデル名のみ）。model は実際に使ったモデル（ポートが解決した名前）、
        // requested_model はノードの設定（null は組織の既定に従った）。
        self.audit(
            &ec.tenant_id,
            "llm.invoke",
            true,
            &json!({ "model": resolved_model(&out, model.as_deref()), "requested_model": model }),
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
        // path_suffix を書いたのに解決できないときはベース URL へ送らない（BUG-13）。ホスト部の url は
        // String 型のリテラルで、値式を解決しない（PIT-36 の型防御）。
        let what = "http.request: path_suffix";
        let suffix = opt_param(p.path_suffix.as_ref(), r, what)?
            .map(|v| typed(&v, what, "文字列", as_string))
            .transpose()?
            .unwrap_or_default();
        let url = format!("{}{suffix}", p.url);
        // 本文の中の入れ子の $from / $template も展開する（`{"$from":…}` のまま外部へ送らない）。
        // 値の解決は secret に触れる前に済ませる。
        let body = opt_param(p.body.as_ref(), r, "http.request: body")?;
        // オブジェクト・配列の本文は JSON として送る（script 経路の `Shiki.http.request` と揃える・GAP-19）。
        // 文字列はそのまま・`{ "base64": … }` はバイト列なので、種類を決めつけない。
        let json_body = body.as_ref().is_some_and(is_json_body);
        let body = body.map(|v| as_bytes(&v));

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
        if json_body {
            headers.push(("Content-Type".to_string(), "application/json".to_string()));
        }

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
        // 出力は次ノードへ本文を渡す（監査には出さない）。外部の応答に反射した secret の平文は、step の出力
        // （実行履歴・画面に返す結果）へ書く前にマスクする（記録時のレダクト・write-only 原則・GAP-09 の T0）。
        let body = response_body(&resp.body, resolved_secret.as_ref());
        Ok(json!({ "status": resp.status, "host": host, "body": body }))
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

/// 外部の応答に反射した secret の平文をマスクする Redactor（この呼び出しで解決した平文だけ・無ければ `None`）。
///
/// 完全一致と登録した変形（前後の空白を落とした形・JSON のエスケープ・%エンコード）だけをマスクする
/// （4 文字未満の平文・ほかの符号化はマスクできない・`secrets::Redactor`）。
fn reflected_redactor(secret: Option<&ResolvedSecretView>) -> Option<secrets::Redactor> {
    let mut redactor = secrets::Redactor::new();
    redactor.add(&String::from_utf8_lossy(&secret?.plaintext));
    (!redactor.is_empty()).then_some(redactor)
}

/// http.request の本文を JSON として送るか（オブジェクト・配列）。
///
/// `{ "base64": "…" }` は [`as_bytes`] がバイト列に戻すので JSON ではない（解読できないときは
/// JSON のまま送られるので JSON 扱い）。
fn is_json_body(v: &Value) -> bool {
    use base64::Engine as _;
    match v {
        Value::Array(_) => true,
        Value::Object(map) => !matches!(
            map.get("base64"),
            Some(Value::String(b64))
                if base64::engine::general_purpose::STANDARD.decode(b64).is_ok()
        ),
        _ => false,
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
    use super::{attach_secret, is_json_body, parse_body};
    use crate::ir::params::{SecretAttach, SecretAttachKind};
    use serde_json::json;

    /// 反射した平文は、テキストの本文でも切る前に全体でマスクする（1MB の切れ目に掛かった前半も残さない）。
    /// JSON のエスケープされた形・キーに載った形もマスクする。
    #[test]
    fn reflected_secrets_are_masked_before_the_body_is_cut() {
        use super::{response_body, BODY_CAP};
        let secret = super::ResolvedSecretView {
            plaintext: b"abc/def+ghi=".to_vec(),
            allowed_hosts: vec![],
        };
        let mut big = vec![b'x'; BODY_CAP - 5];
        big.extend_from_slice(b"abc/def+ghi=");
        big.extend_from_slice(&[b'y'; 10]);
        let v = response_body(&big, Some(&secret));
        let text = v.as_str().unwrap();
        assert_eq!(text.len(), BODY_CAP);
        assert!(text.ends_with("x[REDA"), "{}", &text[text.len() - 20..]);
        // JSON として読めない本文は、エスケープされた形のまま反射しても落とす。
        let v = response_body(br#"echo: {"auth": "Bearer abc\/def+ghi="#, Some(&secret));
        assert_eq!(v, json!(r#"echo: {"auth": "Bearer [REDACTED]"#));
        // JSON の本文はキーも値（%エンコードされた形）もマスクする。
        let v = response_body(
            br#"{"abc/def+ghi=": 1, "next": "https://x.test/?t=abc%2Fdef%2Bghi%3D"}"#,
            Some(&secret),
        );
        assert_eq!(
            v,
            json!({ "[REDACTED]": 1, "next": "https://x.test/?t=[REDACTED]" })
        );
        // secret が無ければそのまま。
        assert_eq!(response_body(b"abc/def+ghi=", None), json!("abc/def+ghi="));
    }

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

    /// オブジェクト・配列の本文だけを JSON として送る（GAP-19）。
    #[test]
    fn json_body_is_objects_and_arrays_but_not_base64_bytes() {
        assert!(is_json_body(&json!({ "text": "hi" })));
        assert!(is_json_body(&json!([1, 2])));
        // 文字列・数値はそのまま（種類を決めつけない）。
        assert!(!is_json_body(&json!("plain")));
        assert!(!is_json_body(&json!(3)));
        // base64 はバイト列に戻る（解読できなければ JSON のまま送られる）。
        assert!(!is_json_body(&json!({ "base64": "aGk=" })));
        assert!(is_json_body(&json!({ "base64": "***" })));
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
