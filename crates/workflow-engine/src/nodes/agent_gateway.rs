//! agent.invoke の道具の呼び出しを能力ゲートウェイへ合流させる口（[`AgentToolGateway`] の実装・BUG-6）。
//!
//! ループ（api の agent-core）から来る道具の呼び出しを、script の `Shiki.*`（`script.rs` の HostBridge）と
//! 同じ順の一段目に通してから [`NodePorts`] へ渡す:
//!
//! 1. 提示した道具か（外れたら `invalid`・監査 reason=not_offered）
//! 2. scope 天井（`ctx.scope_ceiling ∩ 提示した道具の要求スコープ`・外れたら `out_of_scope` の監査）
//! 3. 引数の上限（モデルが書く引数は敵対的入力として扱う・PIT-35 と同型）
//! 4. 宛先の照合（web.fetch だけ・`egress_allowlist ∩ web の天井` を URL のホスト部の文字列で照合）
//! 5. 監査（via=agent）→ ポート → 結果の上限（超えたら切って目印を付ける）
//!
//! 二段目（OpenFGA・二段 authz・存在秘匿）はポートの先のチョークポイントが担う。

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use secrets::DestinationBinding;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::capability::{check_scope_ceiling, CapabilityAudit, ScopeCeiling};
use crate::run::CancelSignal;
use crate::vocab::{
    AgentTool, DEFAULT_READ_WINDOW_CHARS, MAX_CODE_BYTES, MAX_QUERY_CHARS, MAX_READ_WINDOW_CHARS,
    MAX_SEARCH_SNIPPET_CHARS, MAX_SEARCH_TOP_K, MAX_TOOL_RESULT_CHARS, MAX_URL_CHARS,
    MAX_WEB_RESULTS, STORAGE_LIST_MAX_CURSOR_CHARS,
};

use super::http::{check_destination, HttpDenied};
use super::ports::{
    AgentToolGateway, CodeRunReq, DocReadReq, ExecCtx, NodePorts, PortError, StorageListReq,
    WebFetchReq,
};

/// コードの 1 回の実行の時間の上限（ミリ秒）。
const CODE_RUN_TIMEOUT_MS: u64 = 60_000;

/// エンジン側の道具の口（1 つの agent.invoke の step ごとに作る）。
pub(super) struct ExecutorAgentGateway {
    pub(super) ports: Arc<dyn NodePorts>,
    pub(super) audit: Arc<dyn CapabilityAudit>,
    pub(super) ec: ExecCtx,
    /// 提示した道具。
    pub(super) offered: Vec<AgentTool>,
    /// scope 天井（`ctx.scope_ceiling ∩ 提示した道具の要求スコープ`）。
    pub(super) ceiling: BTreeSet<String>,
    /// web.fetch が読んでよいサイト（`egress_allowlist ∩ web の天井`）。
    pub(super) web_hosts: Vec<String>,
    pub(super) cancel: CancelSignal,
}

#[async_trait]
impl AgentToolGateway for ExecutorAgentGateway {
    fn tools(&self) -> &[AgentTool] {
        &self.offered
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    async fn call(&self, tool: AgentTool, args: &Value) -> Result<Value, PortError> {
        let api = tool.as_str();
        if !self.offered.contains(&tool) {
            self.audit.record(
                &self.ec.tenant_id,
                api,
                false,
                &json!({ "via": "agent", "reason": "not_offered", "step": self.ec.step_path }),
            );
            return Err(PortError::invalid(format!(
                "道具 {api} はこの手順では使えません"
            )));
        }
        if let ScopeCeiling::Denied(_) = check_scope_ceiling(api, &self.ceiling) {
            self.audit.record_out_of_scope(
                &self.ec,
                api,
                &json!({ "reason": "out_of_scope", "via": "agent", "step": self.ec.step_path }),
            );
            return Err(PortError::new(
                "out_of_scope",
                format!("scope_ceiling 外の道具: {api}"),
                false,
            ));
        }
        let out = self.dispatch(tool, args).await;
        let meta = match &out {
            Ok(_) => json!({ "via": "agent" }),
            Err(e) => json!({ "via": "agent", "code": e.code }),
        };
        self.audit
            .record(&self.ec.tenant_id, api, out.is_ok(), &meta);
        out.map(cap_result)
    }
}

impl ExecutorAgentGateway {
    async fn dispatch(&self, tool: AgentTool, a: &Value) -> Result<Value, PortError> {
        let ec = &self.ec;
        match tool {
            AgentTool::RagSearch => {
                let query = text_arg(a, "query", MAX_QUERY_CHARS)?;
                let top_k = u32_arg(a, "top_k")?.map(|k| k.clamp(1, MAX_SEARCH_TOP_K));
                let mut out = self.ports.rag_search(ec, &query, top_k).await?;
                trim_search_results(&mut out);
                Ok(out)
            }
            AgentTool::StorageList => {
                let parent_id = opt_uuid_arg(a, "folder_id")?;
                let cursor = opt_text_arg(a, "cursor", STORAGE_LIST_MAX_CURSOR_CHARS)?
                    .filter(|c| !c.is_empty());
                self.ports
                    .storage_list(ec, StorageListReq { parent_id, cursor })
                    .await
            }
            AgentTool::StorageRead => {
                let file = uuid_arg(a, "file_id")?;
                let offset = u64_arg(a, "offset")?.unwrap_or(0);
                let max_chars = window_arg(a)?;
                self.ports
                    .storage_read_window(ec, file, offset, max_chars)
                    .await
            }
            AgentTool::DocRead => {
                let req = DocReadReq {
                    file_id: uuid_arg(a, "file_id")?,
                    version: i64_arg(a, "version")?,
                    page: i64_arg(a, "page")?,
                    heading: opt_text_arg(a, "heading", MAX_QUERY_CHARS)?,
                    from: i64_arg(a, "from")?,
                    offset: i64_arg(a, "offset")?,
                    max_chars: window_arg(a)?,
                };
                self.ports.doc_read(ec, req).await
            }
            AgentTool::WebSearch => {
                let query = text_arg(a, "query", MAX_QUERY_CHARS)?;
                let mut out = self.ports.web_search(ec, &query).await?;
                if let Some(results) = out.get_mut("results").and_then(Value::as_array_mut) {
                    results.truncate(MAX_WEB_RESULTS);
                }
                Ok(out)
            }
            AgentTool::WebFetch => {
                let url = text_arg(a, "url", MAX_URL_CHARS)?;
                let url = self.check_web_destination(&url)?;
                let req = WebFetchReq {
                    url,
                    query: opt_text_arg(a, "query", MAX_QUERY_CHARS)?,
                    offset: u64_arg(a, "offset")?,
                };
                self.ports.web_fetch(ec, req).await
            }
            AgentTool::CodeRun => {
                // コードは前後の空白も意味を持つので、文字の引数の整形（trim）を通さない。
                let code = match a.get("code") {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err(invalid_arg("code", "文字")),
                };
                if code.len() > MAX_CODE_BYTES {
                    return Err(PortError::invalid(format!(
                        "code が長すぎます（{} KB まで）",
                        MAX_CODE_BYTES / 1024
                    )));
                }
                self.ports
                    .code_run(
                        ec,
                        CodeRunReq {
                            code,
                            timeout_ms: Some(CODE_RUN_TIMEOUT_MS),
                        },
                    )
                    .await
            }
            // 予約の道具は提示しない（offered に入らない）が、届いても fail-closed にする。
            AgentTool::DataListTables | AgentTool::DataQuery | AgentTool::DataGet => Err(
                PortError::invalid(format!("道具 {} はまだ使えません", tool.as_str())),
            ),
        }
    }

    /// web.fetch の宛先を照合する（リテラルのホスト部・照合の後でポートが解決後の IP を検証する）。
    fn check_web_destination(&self, url: &str) -> Result<String, PortError> {
        let binding = DestinationBinding::new(self.web_hosts.clone());
        let denied = |reason: &str, detail: &str| {
            self.audit.record(
                &self.ec.tenant_id,
                AgentTool::WebFetch.as_str(),
                false,
                &json!({ "via": "agent", "reason": reason, "detail": detail }),
            );
        };
        match check_destination(url, &binding) {
            Ok(_) => Ok(url.to_string()),
            Err(HttpDenied::HostNotAllowed(h)) => {
                denied("host_not_allowed", &h);
                Err(PortError::forbidden(format!(
                    "このサイトは読めません（許可されたサイト: {}）: {h}",
                    self.web_hosts.join(", ")
                )))
            }
            Err(HttpDenied::BadScheme) => {
                denied("bad_scheme", "");
                Err(PortError::invalid("http か https の URL だけを読めます"))
            }
            Err(HttpDenied::BadUrl | HttpDenied::RedirectDenied) => {
                denied("bad_url", "");
                Err(PortError::invalid("URL を読み取れません"))
            }
        }
    }
}

/// 社内検索の 1 件の本文を上限で切る（数 MB の断片でコンテキストの窓を溢れさせない）。
fn trim_search_results(out: &mut Value) {
    let Some(results) = out.get_mut("results").and_then(Value::as_array_mut) else {
        return;
    };
    for r in results {
        if let Some(Value::String(content)) = r.get_mut("content") {
            if let Some((at, _)) = content.char_indices().nth(MAX_SEARCH_SNIPPET_CHARS) {
                content.truncate(at);
                content.push('…');
            }
        }
    }
}

/// どの道具の結果にも掛ける最終の上限。超えたら JSON の先頭だけを返し、切ったことを書く。
pub(super) fn cap_result(v: Value) -> Value {
    let text = v.to_string();
    match text.char_indices().nth(MAX_TOOL_RESULT_CHARS) {
        None => v,
        Some((at, _)) => json!({
            "truncated": true,
            "note": format!("結果が長いため先頭の {MAX_TOOL_RESULT_CHARS} 字だけを返しました。範囲を絞って呼び直してください"),
            "partial": &text[..at],
        }),
    }
}

fn invalid_arg(key: &str, what: &str) -> PortError {
    PortError::invalid(format!("引数 {key} は{what}で指定してください"))
}

/// 必須の文字の引数（上限を超えたら invalid）。
fn text_arg(a: &Value, key: &str, max: usize) -> Result<String, PortError> {
    opt_text_arg(a, key, max)?.ok_or_else(|| invalid_arg(key, "文字"))
}

fn opt_text_arg(a: &Value, key: &str, max: usize) -> Result<Option<String>, PortError> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.chars().count() <= max => Ok(Some(s.trim().to_string())),
        Some(Value::String(_)) => Err(PortError::invalid(format!(
            "引数 {key} が長すぎます（{max} 字まで）"
        ))),
        Some(_) => Err(invalid_arg(key, "文字")),
    }
}

fn uuid_arg(a: &Value, key: &str) -> Result<Uuid, PortError> {
    opt_uuid_arg(a, key)?.ok_or_else(|| invalid_arg(key, "ID（uuid）"))
}

fn opt_uuid_arg(a: &Value, key: &str) -> Result<Option<Uuid>, PortError> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Uuid::parse_str(s.trim())
            .map(Some)
            .map_err(|_| invalid_arg(key, "ID（uuid）")),
        Some(_) => Err(invalid_arg(key, "ID（uuid）")),
    }
}

/// 0 以上の整数の引数（`10.0` のような小数表記の整数も受ける）。
fn u64_arg(a: &Value, key: &str) -> Result<Option<u64>, PortError> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .or_else(|| {
                v.as_f64()
                    .filter(|n| n.fract() == 0.0 && *n >= 0.0 && *n <= 1e15)
                    .map(|n| {
                        // 0 以上 1e15 以下の整数であることを確かめた。
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let n = n as u64;
                        n
                    })
            })
            .map(Some)
            .ok_or_else(|| invalid_arg(key, "0 以上の整数")),
    }
}

fn u32_arg(a: &Value, key: &str) -> Result<Option<u32>, PortError> {
    u64_arg(a, key)?
        .map(|n| u32::try_from(n).map_err(|_| invalid_arg(key, "小さい整数")))
        .transpose()
}

fn i64_arg(a: &Value, key: &str) -> Result<Option<i64>, PortError> {
    u64_arg(a, key)?
        .map(|n| i64::try_from(n).map_err(|_| invalid_arg(key, "小さい整数")))
        .transpose()
}

/// 1 回に読む字数（1〜20,000・省略は 8,000）。
fn window_arg(a: &Value) -> Result<u32, PortError> {
    Ok(u32_arg(a, "max_chars")?
        .unwrap_or(DEFAULT_READ_WINDOW_CHARS)
        .clamp(1, MAX_READ_WINDOW_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_over_the_cap_are_cut_with_a_marker() {
        let small = json!({ "text": "短い" });
        assert_eq!(cap_result(small.clone()), small);
        let big = json!({ "text": "あ".repeat(MAX_TOOL_RESULT_CHARS + 10) });
        let out = cap_result(big);
        assert_eq!(out["truncated"], true);
        assert_eq!(
            out["partial"].as_str().map(|s| s.chars().count()),
            Some(MAX_TOOL_RESULT_CHARS)
        );
    }

    #[test]
    fn search_snippets_are_trimmed() {
        let mut out = json!({ "results": [{ "content": "x".repeat(MAX_SEARCH_SNIPPET_CHARS + 5) }, { "content": "y" }] });
        trim_search_results(&mut out);
        assert_eq!(
            out["results"][0]["content"]
                .as_str()
                .map(|s| s.chars().count()),
            Some(MAX_SEARCH_SNIPPET_CHARS + 1)
        );
        assert_eq!(out["results"][1]["content"], "y");
    }

    #[test]
    fn arguments_are_bounded() {
        let a =
            json!({ "query": "q".repeat(MAX_QUERY_CHARS + 1), "top_k": 3.0, "file_id": "nope" });
        assert!(text_arg(&a, "query", MAX_QUERY_CHARS).is_err());
        assert_eq!(u32_arg(&a, "top_k").ok().flatten(), Some(3));
        assert!(uuid_arg(&a, "file_id").is_err());
        assert!(text_arg(&json!({}), "query", 10).is_err());
        assert_eq!(
            window_arg(&json!({ "max_chars": 999_999 })).ok(),
            Some(MAX_READ_WINDOW_CHARS)
        );
        assert_eq!(window_arg(&json!({})).ok(), Some(DEFAULT_READ_WINDOW_CHARS));
        assert!(u64_arg(&json!({ "offset": -1 }), "offset").is_err());
    }
}
