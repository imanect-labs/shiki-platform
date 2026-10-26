//! agent.invoke の道具のポート実装（web.search・web.fetch・code.run・doc.read・storage.read の窓読み・BUG-6）。
//!
//! エンジンの能力ゲートウェイ（scope 天井・宛先の照合・監査）を通った呼び出しだけが来る。ここでは実行主体の
//! `AuthContext`（[`auth_ctx`]）でチョークポイント（StorageService・SearchService・SearchProvider・Sandbox）を呼び、
//! 結果を道具の上限の中に収める。権限なしと存在しないは同じ文言に畳む（存在秘匿・監査では区別が残る）。

use std::sync::Arc;

use agent_core::Tool as _;
use futures::StreamExt;
use sandbox_client::{ExecEvent, ExecRequest, SandboxSpec};
use serde_json::{json, Value};
use uuid::Uuid;
use workflow_engine::vocab::{MAX_CODE_STDOUT_BYTES, MAX_WEB_RESULTS};
use workflow_engine::{CodeRunReq, DocReadReq, ExecCtx, PortError, WebFetchReq};

use super::auth::auth_ctx;
use super::ports::ProdNodePorts;

/// 窓読みする前に断るテキストのファイルの大きさ（バイト）。これより大きいものは全体をメモリへ載せない。
const MAX_WINDOW_FILE_BYTES: i64 = 32 * 1024 * 1024;

/// 権限なしと存在しないを同じにした文言（存在秘匿）。
const NOT_READABLE: &str = "ファイルが見つからないか、読む権限がありません。";

/// テキストでないファイルを storage.read で読もうとしたときの文言（doc.read へ誘導する）。
const NOT_TEXT: &str =
    "このファイルはテキストではありません。PDF・Word などの文書は doc.read で読んでください。";

fn hidden() -> PortError {
    PortError::new("not_found", NOT_READABLE, false)
}

/// storage.read の窓読み（テキストだけ・`offset` 字目から `max_chars` 字）。
pub(super) async fn storage_read_window(
    ports: &ProdNodePorts,
    ec: &ExecCtx,
    file_id: Uuid,
    offset: u64,
    max_chars: u32,
) -> Result<Value, PortError> {
    use storage::StorageError as S;
    let ctx = auth_ctx(ec);
    let trace = ec.trace_id.as_deref();
    let meta = match ports.storage.get_metadata(&ctx, file_id, trace).await {
        Ok(n) => n,
        Err(S::Forbidden | S::NotFound) => return Err(hidden()),
        Err(e) => return Err(PortError::unavailable(format!("storage: {e}"))),
    };
    if meta.kind != storage::NodeKind::File {
        return Err(PortError::invalid(
            "フォルダは読めません。中のファイルは storage.list で一覧してください。",
        ));
    }
    if meta.content_type.as_deref().is_some_and(is_binary_type) {
        return Err(PortError::invalid(NOT_TEXT));
    }
    if meta.size_bytes.unwrap_or(0) > MAX_WINDOW_FILE_BYTES {
        return Err(PortError::invalid(format!(
            "ファイルが大きすぎます（{} MB まで）。",
            MAX_WINDOW_FILE_BYTES / 1024 / 1024
        )));
    }
    let (node, bytes) = match ports.storage.read_file_internal(&ctx, file_id, trace).await {
        Ok(v) => v,
        Err(S::Forbidden | S::NotFound) => return Err(hidden()),
        Err(S::Invalid(m)) => return Err(PortError::invalid(m)),
        Err(e) => return Err(PortError::unavailable(format!("storage: {e}"))),
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) if !t.contains('\0') => t,
        _ => return Err(PortError::invalid(NOT_TEXT)),
    };
    let (window, next) = char_window(text, offset, max_chars);
    Ok(json!({
        "id": node.id.to_string(),
        "name": node.name,
        "content_type": node.content_type,
        "size": node.size_bytes,
        "offset": offset,
        "text": window,
        "next_offset": next,
    }))
}

/// `offset` 字目から `max_chars` 字を切り出す（続きがあれば次の開始位置）。
fn char_window(text: &str, offset: u64, max_chars: u32) -> (String, Option<u64>) {
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let take = usize::try_from(max_chars).unwrap_or(usize::MAX);
    let mut chars = text.chars().skip(start);
    let window: String = chars.by_ref().take(take).collect();
    let next = chars
        .next()
        .map(|_| offset + u64::try_from(window.chars().count()).unwrap_or(0));
    (window, next)
}

/// テキストとして読めない種類（窓読みの前に断る・中身でも確かめる）。
fn is_binary_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.starts_with("image/")
        || ct.starts_with("audio/")
        || ct.starts_with("video/")
        || ct.contains("pdf")
        || ct.contains("officedocument")
        || ct.contains("msword")
        || ct.contains("ms-excel")
        || ct.contains("ms-powerpoint")
        || ct.contains("opendocument")
        || ct.contains("zip")
}

/// doc.read（chat と同じ `DocReadTool`＝viewer 判定・可読集合の pre-filter・存在秘匿・索引待ちの応答）。
pub(super) async fn doc_read(
    ports: &ProdNodePorts,
    ec: &ExecCtx,
    req: DocReadReq,
) -> Result<Value, PortError> {
    let search = ports
        .search
        .as_ref()
        .ok_or_else(|| PortError::new("unsupported", "社内検索が構成されていません", false))?;
    let tool =
        chat::doc_read_tool::DocReadTool::new(Arc::clone(search), Arc::clone(&ports.storage));
    let mut input = json!({ "file_id": req.file_id.to_string(), "max_chars": req.max_chars });
    for (key, v) in [
        ("version", req.version),
        ("page", req.page),
        ("from", req.from),
        ("offset", req.offset),
    ] {
        if let Some(v) = v {
            input[key] = json!(v);
        }
    }
    if let Some(h) = req.heading {
        input["heading"] = json!(h);
    }
    let ctx = auth_ctx(ec);
    let out = tool
        .call(&ctx, input, ec.trace_id.as_deref())
        .await
        .map_err(|e| match e {
            agent_core::ToolError::Invalid(m) => PortError::invalid(m),
            other => PortError::unavailable(other.to_string()),
        })?;
    if out.is_error {
        // 権限なしと不在は同じ文言（DocReadTool の DENIED）。
        return Err(PortError::new("doc_read_failed", out.content, false));
    }
    Ok(json!({ "file_id": req.file_id.to_string(), "text": out.content }))
}

/// web.search（`SearchProvider`・検索語が検索サービスへ出る）。
pub(super) async fn web_search(
    ports: &ProdNodePorts,
    ec: &ExecCtx,
    query: &str,
) -> Result<Value, PortError> {
    let provider = ports
        .web_search
        .as_ref()
        .ok_or_else(|| PortError::new("unsupported", "Web 検索が構成されていません", false))?;
    let hits = provider
        .search(&auth_ctx(ec), query, MAX_WEB_RESULTS)
        .await
        .map_err(|e| match e {
            websearch::SearchError::Invalid(m) => PortError::invalid(m),
            websearch::SearchError::Unavailable(m) => {
                tracing::warn!(target: "workflow.agent", error = %m, "web 検索に失敗");
                PortError::unavailable("Web 検索に失敗しました。しばらくしてから試してください。")
            }
        })?;
    let results: Vec<Value> = hits
        .iter()
        .map(|h| json!({ "title": h.title, "url": h.url, "snippet": h.snippet }))
        .collect();
    Ok(json!({ "results": results }))
}

/// web.fetch（agent-core の WebFetchTool＝解決後の IP の検証・リダイレクト非追従・PIT-48・PIT-56）。
pub(super) async fn web_fetch(ec: &ExecCtx, req: WebFetchReq) -> Result<Value, PortError> {
    let tool = agent_core::WebFetchTool::new();
    let mut input = json!({ "url": req.url });
    if let Some(q) = req.query {
        input["query"] = json!(q);
    }
    if let Some(o) = req.offset {
        input["offset"] = json!(o);
    }
    let out = tool
        .call(&auth_ctx(ec), input, ec.trace_id.as_deref())
        .await
        .map_err(|e| match e {
            agent_core::ToolError::Invalid(m) => PortError::invalid(m),
            other => PortError::unavailable(other.to_string()),
        })?;
    if out.is_error {
        return Err(PortError::new("fetch_failed", out.content, false));
    }
    let title = out
        .citations
        .iter()
        .find_map(|c| c.web.as_ref().map(|w| w.title.clone()))
        .unwrap_or_default();
    Ok(json!({ "url": req.url, "title": title, "text": out.content }))
}

/// code.run（隔離環境で Python を実行する・egress 全遮断の code_interpreter spec・成果物は保存しない）。
pub(super) async fn code_run(
    ports: &ProdNodePorts,
    ec: &ExecCtx,
    req: CodeRunReq,
) -> Result<Value, PortError> {
    let sandbox = ports.sandbox.as_ref().ok_or_else(|| {
        PortError::new(
            "unsupported",
            "プログラムを動かす環境が構成されていません",
            false,
        )
    })?;
    // capability 縮小のみ: egress 全遮断の code_interpreter spec（ノード設定で egress/権限は拡大不能）。
    // 隔離ティアだけは admin ポリシーに従う（wasm→gVisor は縮小方向＝より強い隔離なので不変条件を破らない）。
    let spec = SandboxSpec::code_interpreter(
        ports.sandbox_backend,
        ec.tenant_id.clone(),
        ec.org.clone(),
        ec.principal.clone(),
    );
    let handle = sandbox
        .create(spec)
        .await
        .map_err(|e| PortError::unavailable(format!("sandbox create: {e}")))?;
    let exec = sandbox
        .exec(
            &handle,
            ExecRequest::Python {
                code: req.code,
                timeout_ms: req.timeout_ms,
            },
        )
        .await;
    // exec の失敗は空成功にせず伝播する（destroy してから返す）。
    let mut stream = match exec {
        Ok(s) => s,
        Err(e) => {
            let _ = sandbox.destroy(&handle).await;
            return Err(PortError::unavailable(format!("sandbox exec: {e}")));
        }
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut exit_code = None;
    while let Some(ev) = stream.next().await {
        match ev {
            Ok(ExecEvent::Stdout(b)) => push_capped(&mut stdout, &b),
            Ok(ExecEvent::Stderr(b)) => push_capped(&mut stderr, &b),
            Ok(ExecEvent::Exited { code }) => exit_code = Some(code),
            Ok(ExecEvent::LimitExceeded { kind, .. }) => {
                let _ = sandbox.destroy(&handle).await;
                return Err(PortError::new(
                    "sandbox_limit",
                    format!("プログラムが上限を超えました: {kind:?}"),
                    false,
                ));
            }
            Err(e) => {
                let _ = sandbox.destroy(&handle).await;
                return Err(PortError::unavailable(format!("sandbox stream: {e}")));
            }
        }
    }
    let _ = sandbox.destroy(&handle).await;
    Ok(json!({
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "exit_code": exit_code,
    }))
}

/// 出力を上限まで溜める（超えた分は捨てる）。
fn push_capped(buf: &mut Vec<u8>, chunk: &[u8]) {
    let room = MAX_CODE_STDOUT_BYTES.saturating_sub(buf.len());
    buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_cut_by_characters_with_a_next_offset() {
        let text = "あいうえおかきくけこ";
        assert_eq!(char_window(text, 0, 4), ("あいうえ".to_string(), Some(4)));
        assert_eq!(char_window(text, 8, 4), ("けこ".to_string(), None));
        assert_eq!(char_window(text, 10, 4), (String::new(), None));
        assert_eq!(char_window(text, 6, 4), ("きくけこ".to_string(), None));
    }

    #[test]
    fn binary_types_are_refused_before_reading() {
        for ct in [
            "application/pdf",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "image/png",
            "application/zip",
        ] {
            assert!(is_binary_type(ct), "{ct}");
        }
        for ct in [
            "text/plain",
            "text/markdown",
            "text/csv",
            "application/json",
        ] {
            assert!(!is_binary_type(ct), "{ct}");
        }
    }

    #[test]
    fn code_output_is_capped() {
        let mut buf = Vec::new();
        push_capped(&mut buf, &vec![b'a'; MAX_CODE_STDOUT_BYTES - 1]);
        push_capped(&mut buf, b"bcd");
        assert_eq!(buf.len(), MAX_CODE_STDOUT_BYTES);
    }
}
