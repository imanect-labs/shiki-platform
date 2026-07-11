//! B2 サーバ関数の実行（Task 9.12・script-runtime 再利用）。
//!
//! **INV-1: runtime は資格情報を持たない。** ゲスト（QuickJS-in-wasmtime）は net/fs を
//! 持たず（構造的 default-deny）、能力は HostCall 1 本に集約される。ホスト側
//! （[`GatewayHostCallHandler`]）が **ゲートウェイ HTTP** へ委譲し、Bearer（token-exchange
//! 済みユーザー代理 or service identity）はホストだけが付与する＝ゲストは token も
//! confidential secret も観測できない。認可はゲートウェイの二重ゲートが行う（ここで
//! 独自判定しない・単一チョークポイント）。
//!
//! 外部 HTTP（`http.request`）のみホストが直接実行し、manifest `egress_allowlist`
//! （完全一致 / `*.suffix`・default-deny）で判定する。リダイレクトは自動追従しない
//! （PIT-36: 追従先が allowlist を迂回するのを防ぐ・ゲストが再要求すれば各ホップを再検証）。

use std::sync::Arc;

use script_runtime::{compile, HostCall, HostResponse, Limits, ScriptEngine};
use storage::content_address::{miniapp_bundle_key, sha256_hex};
use storage::ObjectStore;
use uuid::Uuid;

use crate::AppPlatformError;

/// ミニアプリ関数用の Shiki API 拡張（ゲスト wasm を再ビルドせずに data/notify を足す）。
///
/// ゲストのブートストラップが公開する低レベル import `__shiki_hostcall` を直接使い、
/// `Shiki.data.*` / `Shiki.notify.*` を定義する（api 名はホスト側 ALLOWED_APIS の閉集合に
/// 照合される・ここは糖衣のみで権限は増えない）。コンパイル済みユーザーコードの**前**に
/// 実行される。
const MINIAPP_PRELUDE: &str = r"
(function () {
  function call(api, args) {
    var resp = __shiki_hostcall(JSON.stringify({ api: api, args: args || {} }));
    var parsed = JSON.parse(resp);
    if (parsed && parsed.ok) { return parsed.value; }
    var err = (parsed && parsed.error) || {};
    var e = new Error(err.message || 'host call failed');
    e.name = 'ShikiError';
    e.code = err.code || 'internal';
    e.retryable = !!err.retryable;
    throw e;
  }
  Shiki.data = {
    listTables: function () { return call('data.list_tables', {}); },
    query: function (tableId, body) { return call('data.query', { table_id: tableId, body: body || {} }); },
    get: function (tableId, recordId) { return call('data.get', { table_id: tableId, record_id: recordId }); },
    create: function (tableId, data) { return call('data.create', { table_id: tableId, body: { data: data } }); },
    update: function (tableId, recordId, patch, expectedRev) {
      return call('data.update', { table_id: tableId, record_id: recordId, body: { patch: patch, expected_rev: expectedRev } });
    }
  };
  Shiki.notify = {
    send: function (recipient, title, body) {
      return call('notify.send', { body: { recipient: recipient, title: title, body: body || null } });
    }
  };
})();
";

/// 1 関数実行の上限（アルファ既定・fuel/メモリ/ホスト呼び出し数/実行時間）。
fn function_limits() -> Limits {
    Limits {
        fuel: 500_000_000,
        memory_bytes: 64 * 1024 * 1024,
        max_host_calls: 64,
        epoch_deadline: std::time::Duration::from_secs(20),
    }
}

/// egress allowlist 判定（完全一致 or `*.suffix`・大文字小文字非区別・default-deny）。
///
/// sandbox-orchestrator の egress ルール（rules.rs）と同じ**ホスト名セマンティクス**。
/// ポートは判定に含めない（http/https の既定ポートのみ許可・URL 解析側で制限）。
pub fn egress_allowed(allowlist: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    allowlist.iter().any(|rule| {
        let rule = rule.to_ascii_lowercase();
        if let Some(suffix) = rule.strip_prefix("*.") {
            // `*.example.com` はサブドメインのみ（apex は明示ルールで）。
            host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}"))
        } else {
            host == rule
        }
    })
}

/// 実行主体（Bearer の出所を明示する・ログ/監査用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionActor {
    /// ユーザー起点（token-exchange 済み・sub=ユーザー維持）。
    User,
    /// event/cron 起点（B2 service account・client_credentials）。
    Service,
}

impl FunctionActor {
    fn as_str(self) -> &'static str {
        match self {
            FunctionActor::User => "user",
            FunctionActor::Service => "service",
        }
    }
}

/// 関数実行の入力（ゲートウェイ層/トリガ層が組み立てる）。
pub struct FunctionInvocation {
    pub tenant_id: String,
    pub app_id: Uuid,
    /// 実行する関数名（manifest server.functions 宣言内・呼び出し側で検証済み）。
    pub function: String,
    pub payload: serde_json::Value,
    /// ゲートウェイへ付与する Bearer（ゲストには渡らない）。
    pub bearer: String,
    pub actor: FunctionActor,
    /// manifest server.egress_allowlist（インストール時点の宣言）。
    pub egress_allowlist: Vec<String>,
}

/// 関数実行の結果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FunctionOutcome {
    pub ok: bool,
    /// スクリプトの戻り値（`ok=false` は打ち切り理由）。
    pub value: serde_json::Value,
    pub logs: Vec<String>,
}

/// B2 関数ランナ（engine＋コード取得＋ホスト委譲の束）。
pub struct FunctionRunner {
    engine: Arc<ScriptEngine>,
    store: Arc<dyn ObjectStore>,
    http: reqwest::Client,
    /// サーバ内から到達するゲートウェイの origin（例 `http://127.0.0.1:8090`）。
    gateway_origin: String,
}

impl FunctionRunner {
    pub fn new(
        engine: Arc<ScriptEngine>,
        store: Arc<dyn ObjectStore>,
        gateway_origin: String,
    ) -> Result<Self, AppPlatformError> {
        // 外部 egress・ゲートウェイ委譲共通の HTTP。リダイレクト自動追従なし（PIT-36）。
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| AppPlatformError::Internal(format!("http client: {e}")))?;
        Ok(FunctionRunner {
            engine,
            store,
            http,
            gateway_origin,
        })
    }

    /// サーバコード（content address ピン）を取得・整合検証してから関数を実行する。
    pub async fn run(
        &self,
        server_bundle_sha: &str,
        inv: FunctionInvocation,
    ) -> Result<FunctionOutcome, AppPlatformError> {
        let key = miniapp_bundle_key(&inv.tenant_id, server_bundle_sha);
        let code = self
            .store
            .get_object(&key)
            .await
            .map_err(|_| AppPlatformError::NotFound)?;
        if sha256_hex(&code) != server_bundle_sha {
            return Err(AppPlatformError::Internal(
                "サーバコードの content address が一致しません（配信拒否）".into(),
            ));
        }
        let source = String::from_utf8(code)
            .map_err(|_| AppPlatformError::Invalid("サーバコードが UTF-8 ではありません".into()))?;
        let compiled = compile::compile(&source)
            .map_err(|e| AppPlatformError::Invalid(format!("サーバコードのコンパイル: {e}")))?;

        let exec_id = format!("miniapp:{}:{}:{}", inv.app_id, inv.function, Uuid::new_v4());
        let input = serde_json::json!({
            "function": inv.function,
            "payload": inv.payload,
            "app_id": inv.app_id,
            "actor": inv.actor.as_str(),
        })
        .to_string();

        let handler = Arc::new(GatewayHostCallHandler {
            http: self.http.clone(),
            gateway_origin: self.gateway_origin.clone(),
            bearer: inv.bearer,
            egress_allowlist: inv.egress_allowlist,
            app_id: inv.app_id,
            function: inv.function.clone(),
            actor: inv.actor,
        });
        let engine = Arc::clone(&self.engine);
        let limits = function_limits();
        let handle = tokio::runtime::Handle::current();
        // prelude はコンパイル後に前置する（compile の禁止構文 lint はユーザーコードにのみ適用）。
        let compiled_js = format!("{MINIAPP_PRELUDE}\n{}", compiled.compiled_js);
        let outcome = tokio::task::spawn_blocking(move || {
            let host_fn: script_runtime::engine::HostFn =
                Box::new(move |call: &HostCall| handle.block_on(handler.handle(call)));
            engine.run(&exec_id, &compiled_js, &input, limits, host_fn)
        })
        .await
        .map_err(|e| AppPlatformError::Internal(format!("関数実行スレッド: {e}")))?;

        Ok(FunctionOutcome {
            ok: outcome.ok,
            value: outcome.value.unwrap_or(serde_json::Value::Null),
            logs: outcome.logs,
        })
    }
}

/// HostCall → ゲートウェイ HTTP／egress HTTP の委譲（資格情報はここだけが持つ）。
struct GatewayHostCallHandler {
    http: reqwest::Client,
    gateway_origin: String,
    bearer: String,
    egress_allowlist: Vec<String>,
    app_id: Uuid,
    function: String,
    actor: FunctionActor,
}

impl GatewayHostCallHandler {
    async fn handle(&self, call: &HostCall) -> HostResponse {
        match call.api.as_str() {
            "log" => {
                tracing::info!(app_id = %self.app_id, function = %self.function,
                    message = %call.args.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                    "miniapp function log");
                HostResponse::Ok(serde_json::Value::Null)
            }
            "context" => HostResponse::Ok(serde_json::json!({
                "app_id": self.app_id,
                "function": self.function,
                "actor": self.actor.as_str(),
            })),
            "http.request" => self.external_http(&call.args).await,
            // 能力面はすべてゲートウェイ HTTP（二重ゲート＝granted ∩ 呼出主体 ReBAC）。
            "data.list_tables" => self.gateway("GET", "/gw/data/tables", None).await,
            "data.query" => {
                let (table, body) = match table_and_body(&call.args) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                self.gateway("POST", &format!("/gw/data/tables/{table}/query"), body)
                    .await
            }
            "data.get" => {
                let (table, id) = match table_and_id(&call.args) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                self.gateway(
                    "GET",
                    &format!("/gw/data/tables/{table}/records/{id}"),
                    None,
                )
                .await
            }
            "data.create" => {
                let (table, body) = match table_and_body(&call.args) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                self.gateway("POST", &format!("/gw/data/tables/{table}/records"), body)
                    .await
            }
            "data.update" => {
                let (table, id) = match table_and_id(&call.args) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                self.gateway(
                    "PATCH",
                    &format!("/gw/data/tables/{table}/records/{id}"),
                    call.args.get("body").cloned(),
                )
                .await
            }
            "rag.search" => {
                self.gateway("POST", "/gw/rag/query", call.args.get("body").cloned())
                    .await
            }
            "notify.send" => {
                self.gateway("POST", "/gw/notify/send", call.args.get("body").cloned())
                    .await
            }
            "storage.read" | "storage.list" | "storage.write" | "workflow.start" => {
                HostResponse::Err {
                    message: format!("api {} は B2 関数では未提供です", call.api),
                    code: "unsupported".into(),
                    retryable: false,
                }
            }
            other => HostResponse::Err {
                message: format!("未知の api: {other}"),
                code: "unknown_api".into(),
                retryable: false,
            },
        }
    }

    /// ゲートウェイへの委譲（Bearer はここで付与・ゲストは知らない）。
    async fn gateway(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> HostResponse {
        let url = format!("{}{path}", self.gateway_origin);
        let mut req = match method {
            "GET" => self.http.get(&url),
            "POST" => self.http.post(&url),
            "PATCH" => self.http.patch(&url),
            _ => return internal("未知の HTTP メソッド"),
        };
        req = req.bearer_auth(&self.bearer);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                return HostResponse::Err {
                    message: format!("ゲートウェイ呼び出しに失敗: {e}"),
                    code: "gateway_unreachable".into(),
                    retryable: true,
                }
            }
        };
        let status = resp.status();
        let value: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        if status.is_success() {
            HostResponse::Ok(value)
        } else {
            HostResponse::Err {
                message: value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ゲートウェイがエラーを返しました")
                    .to_string(),
                code: format!("http_{}", status.as_u16()),
                retryable: status.is_server_error(),
            }
        }
    }

    /// 外部 HTTP（egress allowlist・default-deny・リダイレクト非追従）。
    async fn external_http(&self, args: &serde_json::Value) -> HostResponse {
        let Some(url_str) = args.get("url").and_then(|v| v.as_str()) else {
            return invalid("url がありません");
        };
        let Ok(url) = url::Url::parse(url_str) else {
            return invalid("url が不正です");
        };
        if !matches!(url.scheme(), "http" | "https") {
            return invalid("http/https のみ使用できます");
        }
        if url.port().is_some() && url.port() != url.port_or_known_default() {
            // 既定ポート以外は拒否（内部ポートへの到達を防ぐ・アルファ制約）。
        }
        let Some(host) = url.host_str() else {
            return invalid("host がありません");
        };
        if url.port_or_known_default() != Some(80) && url.port_or_known_default() != Some(443) {
            return deny(host, "既定ポート以外は許可されません");
        }
        if !egress_allowed(&self.egress_allowlist, host) {
            return deny(host, "egress allowlist に含まれていません");
        }
        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();
        let mut req = match method.as_str() {
            "GET" => self.http.get(url.clone()),
            "POST" => self.http.post(url.clone()),
            "PUT" => self.http.put(url.clone()),
            "DELETE" => self.http.delete(url.clone()),
            _ => return invalid("未対応のメソッドです"),
        };
        if let Some(b) = args.get("body") {
            req = req.json(b);
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let text = resp.text().await.unwrap_or_default();
                // 応答サイズ有界化（1MiB・ゲストメモリ保護）。
                let text: String = text.chars().take(1024 * 1024).collect();
                HostResponse::Ok(serde_json::json!({ "status": status, "body": text }))
            }
            Err(e) => HostResponse::Err {
                message: format!("外部 HTTP 失敗: {e}"),
                code: "egress_failed".into(),
                retryable: true,
            },
        }
    }
}

fn table_and_body(
    args: &serde_json::Value,
) -> Result<(Uuid, Option<serde_json::Value>), HostResponse> {
    let table = args
        .get("table_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| invalid("table_id が不正です"))?;
    Ok((table, args.get("body").cloned()))
}

fn table_and_id(args: &serde_json::Value) -> Result<(Uuid, Uuid), HostResponse> {
    let table = args
        .get("table_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| invalid("table_id が不正です"))?;
    let id = args
        .get("record_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| invalid("record_id が不正です"))?;
    Ok((table, id))
}

fn invalid(message: &str) -> HostResponse {
    HostResponse::Err {
        message: message.into(),
        code: "invalid".into(),
        retryable: false,
    }
}

fn internal(message: &str) -> HostResponse {
    HostResponse::Err {
        message: message.into(),
        code: "internal".into(),
        retryable: false,
    }
}

fn deny(host: &str, reason: &str) -> HostResponse {
    HostResponse::Err {
        message: format!("egress 拒否（{host}）: {reason}"),
        code: "egress_denied".into(),
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn egress_is_default_deny_with_exact_and_suffix() {
        let allow = vec!["api.example.com".to_string(), "*.trusted.io".to_string()];
        assert!(egress_allowed(&allow, "api.example.com"));
        assert!(egress_allowed(&allow, "API.EXAMPLE.COM"));
        assert!(egress_allowed(&allow, "a.trusted.io"));
        assert!(egress_allowed(&allow, "deep.a.trusted.io"));
        // default-deny: 部分一致・apex・前方付加はすべて拒否。
        assert!(!egress_allowed(&allow, "example.com"));
        assert!(!egress_allowed(&allow, "evil-api.example.com.attacker.net"));
        assert!(!egress_allowed(&allow, "trusted.io"));
        assert!(!egress_allowed(&allow, "xtrusted.io"));
        assert!(!egress_allowed(&[], "api.example.com"));
    }

    #[test]
    fn actor_str_round_trip() {
        assert_eq!(FunctionActor::User.as_str(), "user");
        assert_eq!(FunctionActor::Service.as_str(), "service");
    }

    /// エラーヘルパの code は分類のために安定していること（監査・再試行判断に使う）。
    #[test]
    fn error_helpers_carry_stable_codes() {
        assert!(
            matches!(invalid("x"), HostResponse::Err { code, retryable, .. } if code == "invalid" && !retryable)
        );
        assert!(
            matches!(internal("x"), HostResponse::Err { code, retryable, .. } if code == "internal" && !retryable)
        );
        assert!(
            matches!(deny("h", "r"), HostResponse::Err { code, retryable, .. } if code == "egress_denied" && !retryable)
        );
    }

    #[test]
    fn arg_parsers_reject_missing_and_malformed_ids() {
        let good = Uuid::new_v4();
        // table_and_body
        assert!(table_and_body(&serde_json::json!({})).is_err());
        assert!(table_and_body(&serde_json::json!({ "table_id": "not-a-uuid" })).is_err());
        let (t, body) =
            table_and_body(&serde_json::json!({ "table_id": good, "body": { "k": 1 } }))
                .expect("valid table_id parses");
        assert_eq!(t, good);
        assert_eq!(body, Some(serde_json::json!({ "k": 1 })));
        // table_and_id
        assert!(table_and_id(&serde_json::json!({ "table_id": good })).is_err());
        assert!(
            table_and_id(&serde_json::json!({ "table_id": good, "record_id": "bad" })).is_err()
        );
        let rec = Uuid::new_v4();
        let (t2, r2) = table_and_id(&serde_json::json!({ "table_id": good, "record_id": rec }))
            .expect("valid ids parse");
        assert_eq!((t2, r2), (good, rec));
    }

    fn handler(allow: Vec<String>) -> GatewayHostCallHandler {
        GatewayHostCallHandler {
            http: reqwest::Client::new(),
            // 到達不能な origin: ネットワークに出る分岐はこのテストでは踏まない。
            gateway_origin: "http://127.0.0.1:1".into(),
            bearer: "test-bearer".into(),
            egress_allowlist: allow,
            app_id: Uuid::nil(),
            function: "fn".into(),
            actor: FunctionActor::Service,
        }
    }

    fn call(api: &str, args: serde_json::Value) -> HostCall {
        HostCall {
            exec_id: "e".into(),
            seq: 1,
            api: api.into(),
            args,
        }
    }

    /// ネットワークに出ない分岐（ローカル応答・引数不正・未提供 api）を網羅する。
    #[tokio::test]
    async fn handle_local_and_rejection_branches() {
        let h = handler(vec![]);

        // log / context はゲートウェイに出ずローカルで応答。
        assert!(matches!(
            h.handle(&call("log", serde_json::json!({ "message": "hi" })))
                .await,
            HostResponse::Ok(_)
        ));
        let ctx = h.handle(&call("context", serde_json::json!({}))).await;
        match ctx {
            HostResponse::Ok(v) => {
                assert_eq!(v["actor"], serde_json::json!("service"));
                assert_eq!(v["function"], serde_json::json!("fn"));
            }
            HostResponse::Err { message, .. } => panic!("context should be Ok: {message}"),
        }

        // Stage A 能力は B2 では未提供（unsupported・fail-closed）。
        assert!(matches!(
            h.handle(&call("storage.read", serde_json::json!({}))).await,
            HostResponse::Err { code, .. } if code == "unsupported"
        ));
        // 閉集合外は unknown_api。
        assert!(matches!(
            h.handle(&call("evil.exfiltrate", serde_json::json!({}))).await,
            HostResponse::Err { code, .. } if code == "unknown_api"
        ));

        // data.* は table_id/record_id 不正ならゲートウェイへ出る前に invalid で弾く。
        for api in ["data.query", "data.get", "data.create", "data.update"] {
            assert!(
                matches!(
                    h.handle(&call(api, serde_json::json!({ "table_id": "bad" }))).await,
                    HostResponse::Err { code, .. } if code == "invalid"
                ),
                "{api} should reject bad table_id before network"
            );
        }
    }

    /// external_http のガード（url/scheme/port/allowlist）は send() 前に判定される。
    #[tokio::test]
    async fn external_http_guards_reject_before_send() {
        let h = handler(vec!["api.allowed.com".to_string()]);
        // url 欠落・不正 url・非 http スキームは invalid。
        for (args, code) in [
            (serde_json::json!({}), "invalid"),
            (serde_json::json!({ "url": "::::" }), "invalid"),
            (
                serde_json::json!({ "url": "ftp://api.allowed.com/x" }),
                "invalid",
            ),
            // 既定ポート以外は拒否（内部ポート到達防止）。
            (
                serde_json::json!({ "url": "http://api.allowed.com:8080/x" }),
                "egress_denied",
            ),
            // allowlist 外は default-deny。
            (
                serde_json::json!({ "url": "https://evil.example/x" }),
                "egress_denied",
            ),
        ] {
            match h.external_http(&args).await {
                HostResponse::Err { code: c, .. } => assert_eq!(c, code, "for {args}"),
                HostResponse::Ok(v) => panic!("expected Err for {args}: got Ok({v})"),
            }
        }
    }
}
