//! `web_fetch` ツール（Phase 4 web ツール・ホストネイティブ取得・issue #348）。
//!
//! HTTP GET はサンドボックスを介さずホスト側の reqwest で撃つ（Pyodide 初期化 ~6s の
//! オーバーヘッドを外す）。**egress の封じ込めはポリシ層で等価に維持する**:
//! - **アプリ層の一次防壁**: [`validate_url`] が http/https 以外・userinfo 付き・IP リテラル・
//!   単一ラベル名・localhost/.local/.internal 等を拒否（SSRF の素地を断つ）。
//! - **解決後 IP の再検証＋アドレス固定**（DNS リバインディング対策）: 名前解決の結果が
//!   全て公開 IP であることを確認し（[`net_guard::ensure_all_public`]）、**その検証済み
//!   アドレスへ接続を固定する**（接続時に再解決させない）。検証と接続で解決結果が変わる
//!   攻撃はここでしか塞げない。
//! - **リダイレクト非追従**（PIT-36）: 検証を迂回する誘導を遮断する。3xx は Location を
//!   観測として返し、モデルが改めて（＝再検証を通して）取得できるようにする。
//! - **シークレット非添付・プロキシ非経由**: 資格情報を載せず、環境プロキシで
//!   アドレス固定を迂回されない（`no_proxy`）。
//! - 応答は untrusted（PIT-23）。**テキストとして読むだけ**でサイズ上限を課し、実行経路は作らない。

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use authz::AuthContext;
use sandbox_client::net_guard::{self, HostResolver, SystemResolver};
use url::{Host, Url};

use super::sandbox_exec::truncate;
use crate::tool::{Tool, ToolError, ToolOutcome};

#[cfg(test)]
mod tests;

/// 取得本文の読み取り上限（モデル向け整形上限は別途 [`truncate`] が掛かる）。
const FETCH_BODY_CAP: usize = 256 * 1024;

/// 1 リクエストの上限（接続〜読み切りまで）。
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// 接続確立の上限（到達不能な宛先で 20 秒待たない）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

const USER_AGENT: &str = "shiki-web-fetch/1.0";

/// `web_fetch` ツール（ホストネイティブ取得・宛先は解決後 IP 検証＋アドレス固定）。
pub struct WebFetchTool {
    resolver: Arc<dyn HostResolver>,
    /// **テスト専用**の解決後 IP 検証スキップ（ループバックのスタブサーバへ繋ぐため）。
    /// `cfg(test)` 限定なのでリリースビルドにはこのフィールド自体が存在しない。
    #[cfg(test)]
    skip_addr_guard: bool,
}

impl WebFetchTool {
    #[must_use]
    pub fn new() -> Self {
        Self::with_resolver(Arc::new(SystemResolver))
    }

    /// リゾルバを差し替える（既定は OS の DNS）。
    #[must_use]
    pub fn with_resolver(resolver: Arc<dyn HostResolver>) -> Self {
        WebFetchTool {
            resolver,
            #[cfg(test)]
            skip_addr_guard: false,
        }
    }

    /// 解決結果を検証する（**接続先を確定させる唯一の関門**）。
    ///
    /// テスト版（下）だけが `self` を読むが、呼び出し側を分岐させないためシグネチャを揃える。
    #[cfg(not(test))]
    #[allow(clippy::unused_self)]
    fn guard(&self, addrs: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str> {
        net_guard::ensure_all_public(addrs)
    }

    #[cfg(test)]
    fn guard(&self, addrs: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str> {
        if self.skip_addr_guard {
            return Ok(addrs);
        }
        net_guard::ensure_all_public(addrs)
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

/// 検証済みの取得先（接続を固定する host/port と正規化済み URL）。
struct FetchTarget {
    url: Url,
    host: String,
    port: u16,
}

/// 入力 URL を検証する（モデル/ユーザー由来＝敵対的として扱う）。
///
/// - スキームは http/https のみ（gopher/file 等を拒否）。
/// - userinfo（`user:pass@`）付きは拒否（ホスト偽装・資格情報混入の防止）。
/// - ホストは **ドットを含む公開 FQDN のみ**: IP リテラル（v4/v6）・単一ラベル名
///   （compose のサービス名 `minio` 等）・localhost/.local/.internal/.lan/.home.arpa を拒否。
///   名前が公開 IP に解決されるかは、この後の [`net_guard::ensure_all_public`] が判定する。
fn validate_url(input: &str) -> Result<FetchTarget, ToolError> {
    let invalid = |msg: &str| ToolError::Invalid(format!("URL が不正です: {msg}"));
    let url = Url::parse(input.trim()).map_err(|e| invalid(&e.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("http/https のみ取得できます"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("userinfo（user:pass@）付き URL は使えません"));
    }
    let host = match url.host() {
        Some(Host::Domain(d)) => d.to_ascii_lowercase(),
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => {
            return Err(invalid("IP アドレス直指定は使えません"));
        }
        None => return Err(invalid("ホストがありません")),
    };
    // 内部/ローカル名を拒否（SSRF・confused-deputy の素地を断つ）。
    let forbidden_suffixes = [".local", ".internal", ".localhost", ".lan", ".home.arpa"];
    if !host.contains('.')
        || host == "localhost"
        || forbidden_suffixes.iter().any(|s| host.ends_with(s))
    {
        return Err(invalid("内部/ローカルホストは取得できません"));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| invalid("ポートを特定できません"))?;
    Ok(FetchTarget { url, host, port })
}

/// モデルが読めるテキストか（バイナリを本文として渡さない）。
///
/// Content-Type 無しは許可する（省略するサーバが実在し、本文は lossy UTF-8 で読むため害がない）。
fn is_textual(content_type: Option<&str>) -> bool {
    let Some(ct) = content_type else { return true };
    let ct = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    ct.starts_with("text/")
        || ct.ends_with("+json")
        || ct.ends_with("+xml")
        || matches!(
            ct.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/ecmascript"
                | "application/x-ndjson"
                | "application/graphql"
                | ""
        )
}

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        crate::vocab::ToolName::WebFetch.as_str()
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        "URL のページを取得して本文を返す（リダイレクトは追従しない）。web_search で得た URL の\
         内容を読むときに使う。取得できるのは公開ホストの http/https のみで、内部ネットワークへは通信しない。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "取得する URL（http/https）" }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    // 読み取りのみ・シークレット非添付・宛先は公開ホストに限定。確認不要。
    fn requires_confirmation(&self) -> bool {
        false
    }

    // 冪等な read（副作用なし）。同一ステップ内で他の read と並列実行してよい（#349）。
    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        _ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let url_input = input
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::Invalid("missing 'url'".into()))?;
        let target = validate_url(url_input)?;

        // 名前解決 → 解決後 IP の検証。以降の接続は「この検証済みアドレス」に固定する。
        let resolved = self
            .resolver
            .lookup(&target.host, target.port)
            .await
            .map_err(|e| ToolError::Invalid(format!("URL が不正です: {e}")))?;
        let addrs = self
            .guard(resolved)
            .map_err(|e| ToolError::Invalid(format!("取得先が拒否されました: {e}")))?;

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none()) // PIT-36
            .no_proxy() // 環境プロキシでアドレス固定を迂回させない
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(FETCH_TIMEOUT)
            .user_agent(USER_AGENT)
            .resolve_to_addrs(&target.host, &addrs) // ← DNS を引き直させない
            .build()
            .map_err(|e| ToolError::Unavailable(format!("http client: {e}")))?;

        let response = match client.get(target.url.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolOutcome::error(format!("取得に失敗しました: {e}")));
            }
        };

        let status = response.status();
        let content_type = header(&response, reqwest::header::CONTENT_TYPE);
        let location = header(&response, reqwest::header::LOCATION);

        let mut head = format!("HTTP {}", status.as_u16());
        if let Some(ct) = &content_type {
            let _ = write!(head, "\nContent-Type: {ct}");
        }
        // 3xx は本文を読まずに Location だけ返す（追従しない・モデルが再検証付きで取り直す）。
        if let Some(loc) = location {
            let _ = write!(
                head,
                "\nLocation: {loc}\n（リダイレクトは追従しません。必要なら上の URL を web_fetch し直してください）"
            );
            return Ok(ToolOutcome::ok(truncate(&head)));
        }
        if !is_textual(content_type.as_deref()) {
            return Ok(ToolOutcome::error(format!(
                "{head}\nテキストではないため本文を返しません"
            )));
        }

        let (body, capped) = match read_capped(response).await {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolOutcome::error(format!(
                    "{head}\n本文の読み取りに失敗: {e}"
                )))
            }
        };
        if capped {
            head.push_str("\n（本文は 256KiB で打ち切り済み）");
        }
        let text = String::from_utf8_lossy(&body);
        Ok(ToolOutcome::ok(truncate(&format!("{head}\n\n{text}"))))
    }
}

/// 応答ヘッダを文字列で取り出す（非 ASCII 等で読めなければ無視する）。
fn header(response: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// 本文を [`FETCH_BODY_CAP`] まで読む（上限に達したら以降は受け取らない）。
async fn read_capped(mut response: reqwest::Response) -> Result<(Vec<u8>, bool), reqwest::Error> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = FETCH_BODY_CAP - body.len();
        if chunk.len() >= remaining {
            body.extend_from_slice(&chunk[..remaining]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}
