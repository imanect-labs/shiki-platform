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
//! 名前解決・アドレスの確かめ・接続の固定・本文の上限は [`crate::function_http`]（BUG-196）。

use std::sync::Arc;

use sandbox_client::net_guard::{HostResolver, SystemResolver};
use script_runtime::{compile, EngineHandle, HostCall, HostResponse, Limits};
use storage::content_address::{miniapp_bundle_key, sha256_hex};
use storage::ObjectStore;
use uuid::Uuid;

use crate::AppPlatformError;

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
    /// スクリプトの戻り値（`ok=false` は null）。
    pub value: serde_json::Value,
    pub logs: Vec<String>,
    /// 失敗の理由と文（`ok=false` のときだけ・GAP-60）。スタックの行・内部の文を含まない。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::FunctionFailure>,
}

/// B2 関数ランナ（engine＋コード取得＋ホスト委譲の束）。
pub struct FunctionRunner {
    /// 共有エンジンなら、コンパイルの完了は関数を実行するときに待つ（起動の経路で待たない）。
    engine: EngineHandle,
    store: Arc<dyn ObjectStore>,
    /// ゲートウェイへの委譲の HTTP（外部への送信は [`crate::function_http`] が 1 回ごとに組む）。
    http: reqwest::Client,
    /// サーバ内から到達するゲートウェイの origin（例 `http://127.0.0.1:8090`）。
    gateway_origin: String,
    /// 外部への送信の名前解決（既定は OS の DNS。テストで解決先を決める）。
    resolver: Arc<dyn HostResolver>,
}

impl FunctionRunner {
    pub fn new(
        engine: impl Into<EngineHandle>,
        store: Arc<dyn ObjectStore>,
        gateway_origin: String,
    ) -> Result<Self, AppPlatformError> {
        // ゲートウェイ委譲の HTTP。リダイレクト自動追従なし（PIT-36）。
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| AppPlatformError::Internal(format!("http client: {e}")))?;
        Ok(FunctionRunner {
            engine: engine.into(),
            store,
            http,
            gateway_origin,
            resolver: Arc::new(SystemResolver),
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
            resolver: Arc::clone(&self.resolver),
            bearer: inv.bearer,
            egress_allowlist: inv.egress_allowlist,
            app_id: inv.app_id,
            function: inv.function.clone(),
            actor: inv.actor,
        });
        let engine = self.engine.get().await.map_err(|e| {
            AppPlatformError::Internal(format!("script エンジンを準備できません: {e}"))
        })?;
        let limits = function_limits();
        let limits_secs = limits.epoch_deadline.as_secs();
        let handle = tokio::runtime::Handle::current();
        // prelude はコンパイル後に前置する（compile の禁止構文 lint はユーザーコードにのみ適用）。
        // `Shiki.data.*` / `Shiki.notify.*` の糖衣（ゲスト wasm を再ビルドせずに足す・script-runtime の共通定義）。
        let compiled_js = format!(
            "{}\n{}",
            script_runtime::prelude::miniapp_prelude(),
            compiled.compiled_js
        );
        let outcome = tokio::task::spawn_blocking(move || {
            let host_fn: script_runtime::engine::HostFn =
                Box::new(move |call: &HostCall| handle.block_on(handler.handle(call)));
            engine.run(&exec_id, &compiled_js, &input, limits, host_fn)
        })
        .await
        .map_err(|e| AppPlatformError::Internal(format!("関数実行スレッド: {e}")))?;

        let error = (!outcome.ok).then(|| {
            crate::function_failure::function_failure(
                outcome.error,
                &outcome.termination,
                limits_secs,
            )
        });
        Ok(FunctionOutcome {
            ok: outcome.ok,
            value: outcome.value.unwrap_or(serde_json::Value::Null),
            logs: outcome.logs,
            error,
        })
    }
}

/// HostCall → ゲートウェイ HTTP／egress HTTP の委譲（資格情報はここだけが持つ）。
struct GatewayHostCallHandler {
    http: reqwest::Client,
    gateway_origin: String,
    resolver: Arc<dyn HostResolver>,
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
            // FSM 遷移（status を進める唯一の経路・ガードと actor 述語はゲートウェイ先の DataStore が評価）。
            "data.transition" => {
                let (table, id) = match table_and_id(&call.args) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                self.gateway(
                    "POST",
                    &format!("/gw/data/tables/{table}/records/{id}/transition"),
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

    /// 外部 HTTP（egress allowlist・default-deny・リダイレクト非追従・確かめたアドレスに接続を固定・BUG-196）。
    async fn external_http(&self, args: &serde_json::Value) -> HostResponse {
        let Some(url_str) = args.get("url").and_then(|v| v.as_str()) else {
            return invalid("url がありません");
        };
        let Ok(url) = url::Url::parse(url_str) else {
            return invalid(
                "URL の書き方が正しくありません。https:// から、ドメインまで書いてください。",
            );
        };
        if !matches!(url.scheme(), "http" | "https") {
            return invalid("URL は http:// か https:// で始めてください。");
        }
        let Some(host) = url.host_str().map(str::to_string) else {
            return invalid("URL にホストがありません");
        };
        // 既定のポート（http は 80・https は 443）だけ（内部のポートへ届かないように・アルファの制約）。
        let Some(port @ (80 | 443)) = url.port_or_known_default() else {
            return deny(format!(
                "{host} へは、決まったポート（http は 80、https は 443）でだけ接続できます。"
            ));
        };
        if !egress_allowed(&self.egress_allowlist, &host) {
            return deny(format!(
                "このアプリが通信してよい先に {host} が含まれていないため、接続しませんでした。"
            ));
        }
        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();
        let method = match method.as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "DELETE" => reqwest::Method::DELETE,
            _ => {
                return invalid(
                    "送り方（メソッド）は GET・POST・PUT・DELETE のどれかにしてください。",
                )
            }
        };
        // SSRF 防御: 名前を 1 回だけ引き、許可ホストが内部・メタデータの IP に解決されるなら断る（PIT-23・PIT-48）。
        let (domain, addrs) =
            match crate::function_http::resolve_checked(&*self.resolver, &url, port).await {
                Ok(v) => v,
                Err(refused) => return refused,
            };
        crate::function_http::send_pinned(
            url,
            method,
            args.get("body"),
            domain.as_deref(),
            &addrs,
            crate::function_http::MAX_BODY,
        )
        .await
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

pub(crate) fn invalid(message: &str) -> HostResponse {
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

/// 外部への送信を断った（code は `egress_denied`・やり直さない）。
pub(crate) fn deny(message: String) -> HostResponse {
    HostResponse::Err {
        message,
        code: "egress_denied".into(),
        retryable: false,
    }
}

#[cfg(test)]
#[path = "functions_tests.rs"]
mod tests;
