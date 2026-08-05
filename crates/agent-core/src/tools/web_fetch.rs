//! `web_fetch` ツール（Phase 4 web ツール・ホストネイティブ取得・issue #348 / #405）。
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
//! - 応答は untrusted（PIT-23）。実行経路は作らず、サイズ上限を課す。
//!
//! # コンテキスト効率（#405）
//!
//! 取得本文は**そのままモデルへ渡さない**。生 HTML は先頭が `<head>`・CSS・JSON-LD・ナビで、
//! 本文は中盤にある。旧実装（生 HTML の先頭 16KiB 切り）では**本文が 1 文字も入らない**ことが
//! 普通に起き、4,000 トークン払って収穫ゼロだった。現在は 4 段で処理する:
//!
//! 1. [`decode`] — Content-Type / meta / BOM から文字コードを決める（Shift_JIS の国内サイト対策）
//! 2. [`extract`] — ノイズ除去 → 本文特定（Readability 相当）→ Markdown 化
//! 3. [`doc`] — PDF/Office は ingestion-worker（Docling）へ回す。**URL ではなくバイト列を渡す**
//! 4. [`render`] — 自己要約ヘッダ ＋ query 絞り込み ／ offset 続き読み（境界は [`envelope`]）

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use authz::AuthContext;
use rag::DocumentParser;
use sandbox_client::net_guard::{self, HostResolver, SystemResolver};

use crate::tool::{Tool, ToolError, ToolOutcome};

mod decode;
mod doc;
mod envelope;
mod extract;
mod input;
mod render;
mod sections;

use input::{
    file_name_of, is_html, is_textual, parse_options, sniff_html, validate_url, FetchTarget,
};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_documents;
#[cfg(test)]
mod tests_efficiency;

/// テキスト応答の読み取り上限（モデル向け整形上限は [`render`] が別途掛ける）。
const FETCH_BODY_CAP: usize = 256 * 1024;

/// PDF/Office の読み取り上限。テキストと違い「本文の密度」がバイト数に比例しないため
/// 別枠で広く取る（worker 側の `max_download_bytes` より小さく保つ）。
const FETCH_DOC_CAP: usize = 16 * 1024 * 1024;

/// モデルへ渡す本文の既定上限（**文字**数。バイトではない）。
///
/// 日本語は 1 文字 3 バイトで、バイト上限だと英語ページの 1/3 しか読めなかった。
/// 抽出後の Markdown は生 HTML の 1/10 前後になるため、文字数を増やしても
/// 実トークンは旧実装より小さい。
const DEFAULT_MAX_CHARS: usize = 12_000;

/// `offset` に許す上限（桁を打ち間違えた指定で無を返さないためのサニティ）。
const MAX_OFFSET: usize = 10_000_000;

/// `query` の上限（長文を投げられても照合語は増えない）。
const MAX_QUERY_CHARS: usize = 200;

/// 1 リクエストの上限（接続〜読み切りまで）。
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// 接続確立の上限（到達不能な宛先で 20 秒待たない）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 名前解決の上限。reqwest の timeout は client 構築後にしか効かないため、
/// **解決フェーズにも独立して期限を掛ける**（応答しない DNS で張り付かせない）。
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

const USER_AGENT: &str = "shiki-web-fetch/1.0";

/// `web_fetch` ツール（ホストネイティブ取得・宛先は解決後 IP 検証＋アドレス固定）。
pub struct WebFetchTool {
    resolver: Arc<dyn HostResolver>,
    /// PDF/Office を読むための Docling 経路（未配線ならバイナリ文書は従来どおり拒否）。
    parser: Option<Arc<dyn DocumentParser>>,
    /// モデルへ渡す本文の上限（文字）。
    max_chars: usize,
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
            parser: None,
            max_chars: DEFAULT_MAX_CHARS,
            #[cfg(test)]
            skip_addr_guard: false,
        }
    }

    /// PDF/Office を読めるようにする（ingestion-worker の `DocumentParser`・#405）。
    ///
    /// 渡すのは**取得済みバイト列**で、URL は worker へ渡らない（`doc` モジュール参照）。
    #[must_use]
    pub fn with_parser(mut self, parser: Arc<dyn DocumentParser>) -> Self {
        self.parser = Some(parser);
        self
    }

    /// モデルへ渡す本文の上限（文字）を差し替える。
    #[must_use]
    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        if max_chars > 0 {
            self.max_chars = max_chars;
        }
        self
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

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        crate::vocab::ToolName::WebFetch.as_str()
    }

    fn description(&self) -> &str {
        // PDF が読めるかは配線依存。宣伝と実体を一致させる（PIT-51 と同じ考え方）。
        if self.parser.is_some() {
            "URL のページを取得し、本文を抽出して Markdown で返す（広告・ナビ・スクリプトは除去済み・\
             リダイレクトは追従しない）。PDF や Office 文書も本文を抽出できる。web_search で得た URL の\
             内容を読むときに使う。長いページは query に知りたい論点を書くと関連する節だけ返り、\
             offset で続きを読める。取得できるのは公開ホストの http/https のみで、内部ネットワークへは通信しない。"
        } else {
            "URL のページを取得し、本文を抽出して Markdown で返す（広告・ナビ・スクリプトは除去済み・\
             リダイレクトは追従しない）。web_search で得た URL の内容を読むときに使う。\
             長いページは query に知りたい論点を書くと関連する節だけ返り、offset で続きを読める。\
             取得できるのは公開ホストの http/https のみで、内部ネットワークへは通信しない。"
        }
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "取得する URL（http/https）" },
                "query": {
                    "type": "string",
                    "description": "知りたい論点（任意）。長いページから関連する節だけを抜き出す。\
                                    ページ全体が要るときは指定しない。"
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "続きを読む開始位置（任意・文字単位）。前回の結果が\
                                    打ち切られたときに、示された値をそのまま渡す。"
                }
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
        ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let url_input = input
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::Invalid("missing 'url'".into()))?;
        let target = validate_url(url_input)?;
        let options = parse_options(&input);

        // 名前解決 → 解決後 IP の検証。以降の接続は「この検証済みアドレス」に固定する。
        //
        // 解決にも**期限を掛ける**（reqwest の timeout は client 構築後にしか効かないため、
        // 応答しない DNS で無期限に張り付くのを防ぐ）。
        let resolved = tokio::time::timeout(
            RESOLVE_TIMEOUT,
            self.resolver.lookup(&target.host, target.port),
        )
        .await
        .map_err(|_| ToolError::Invalid("URL が不正です: 名前解決がタイムアウトしました".into()))?
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

        // **3xx のときだけ**リダイレクト扱いにする（201/200 等が付ける Location で本文を捨てない）。
        if let (true, Some(loc)) = (status.is_redirection(), location) {
            let mut head = format!("HTTP {} | {}", status.as_u16(), target.url);
            let _ = write!(
                head,
                "\nLocation: {loc}\n（リダイレクトは追従しません。必要なら上の URL を web_fetch し直してください）"
            );
            return Ok(ToolOutcome::ok(head));
        }

        let declared_doc = doc::classify(content_type.as_deref(), &target.url);
        if !is_textual(content_type.as_deref()) && declared_doc.is_none() {
            return Ok(ToolOutcome::error(format!(
                "HTTP {} | {} | {}\nテキストではないため本文を返しません",
                status.as_u16(),
                content_type.as_deref().unwrap_or("(Content-Type なし)"),
                target.url
            )));
        }
        if declared_doc.is_some() && self.parser.is_none() {
            return Ok(ToolOutcome::error(format!(
                "HTTP {} | {} | {}\n文書解析（Docling）が未配線のため本文を返せません",
                status.as_u16(),
                content_type.as_deref().unwrap_or("(Content-Type なし)"),
                target.url
            )));
        }

        let cap = if declared_doc.is_some() {
            FETCH_DOC_CAP
        } else {
            FETCH_BODY_CAP
        };
        let (body, source_capped) = match read_capped(response, cap).await {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolOutcome::error(format!(
                    "HTTP {} | {}\n本文の読み取りに失敗: {e}",
                    status.as_u16(),
                    target.url
                )))
            }
        };

        // 宣言も拡張子も外れる配信（Content-Type 無しで PDF を返す等）への最後の砦。
        let doc_type =
            declared_doc.or_else(|| self.parser.is_some().then(|| doc::sniff(&body)).flatten());
        let bytes_in = body.len();
        let page = match (doc_type, self.parser.as_ref()) {
            (Some(mime), Some(parser)) => {
                // **切れたバイト列をパーサへ渡さない**。PDF の相互参照表も OOXML の
                // セントラルディレクトリも末尾にあるため、上限で切れた文書は必ず解析に失敗する。
                // 「解析失敗」ではなく「大きすぎる」と返す方が、モデルは次の手を打てる。
                if source_capped {
                    return Ok(ToolOutcome::error(format!(
                        "HTTP {} | {}\n文書が取得上限 {} で切れているため解析しません\
                         （PDF/Office は末尾の索引が欠けると必ず失敗します）。\
                         分割された版か HTML 版を探してください。",
                        status.as_u16(),
                        target.url,
                        human_cap(cap)
                    )));
                }
                match document_page(parser, ctx, &body, &target, mime).await {
                    Ok(page) => page,
                    Err(message) => {
                        return Ok(ToolOutcome::error(format!(
                            "HTTP {} | {}\n{message}",
                            status.as_u16(),
                            target.url
                        )))
                    }
                }
            }
            _ => text_page(body, content_type.as_deref(), &target).await?,
        };

        let rendered = render::render(
            &page,
            &render::RenderOpts {
                url: target.url.as_str(),
                status: status.as_u16(),
                content_type: content_type.as_deref(),
                query: options.query.as_deref(),
                offset: options.offset,
                max_chars: self.max_chars,
                source_capped,
            },
        );
        // 削減率の回帰検知（抽出が壊れると静かにトークンだけ焼けるため観測する）。
        tracing::info!(
            target: "web_fetch",
            url = %target.url,
            status = status.as_u16(),
            kind = page.kind,
            extractor = page.extractor,
            encoding = %page.encoding,
            bytes_in,
            chars_body = page.body.chars().count(),
            chars_out = rendered.chars().count(),
            "web_fetch 抽出完了"
        );
        Ok(ToolOutcome::ok(rendered))
    }
}

/// PDF/Office を Docling でパースして本文へ落とす。
///
/// **URL ではなくバイト列**を渡す（`doc` モジュールの冒頭を参照。worker に取得させると
/// 宛先制限を迂回できてしまう）。
async fn document_page(
    parser: &Arc<dyn DocumentParser>,
    ctx: &AuthContext,
    body: &[u8],
    target: &FetchTarget,
    content_type: &str,
) -> Result<render::Page, String> {
    let file_name = file_name_of(&target.url);
    let parsed = doc::parse_to_markdown(parser, ctx, body, content_type, &file_name).await?;
    Ok(render::Page {
        title: parsed.title,
        byline: None,
        site_name: Some(target.host.clone()),
        published: None,
        body: parsed.markdown,
        kind: "document",
        extractor: if parsed.used_ocr {
            "docling+ocr"
        } else {
            "docling"
        },
        encoding: "binary".into(),
    })
}

/// テキスト応答（HTML / JSON / プレーン）を本文へ落とす。
///
/// HTML の抽出は DOM 構築を伴う CPU バウンド処理なので、**ランタイムをブロックしない**よう
/// `spawn_blocking` へ逃がす（敵対的な巨大 DOM で全 run のスケジューリングを止めない・PIT-23）。
async fn text_page(
    body: Vec<u8>,
    content_type: Option<&str>,
    target: &FetchTarget,
) -> Result<render::Page, ToolError> {
    let decoded = decode::decode(&body, content_type);
    let encoding = decoded.encoding.to_string();
    // Content-Type を返さないサーバでも HTML なら抽出へ回す（そうしないと生 HTML が流れる）。
    let as_html = is_html(content_type) || (content_type.is_none() && sniff_html(&decoded.text));
    if !as_html {
        return Ok(render::Page {
            title: None,
            byline: None,
            site_name: Some(target.host.clone()),
            published: None,
            body: decoded.text,
            kind: "text",
            extractor: "raw",
            encoding,
        });
    }
    let url = target.url.to_string();
    let host = target.host.clone();
    let article =
        tokio::task::spawn_blocking(move || extract::html_to_markdown(&decoded.text, &url))
            .await
            .map_err(|e| ToolError::Unavailable(format!("本文抽出に失敗しました: {e}")))?;
    Ok(render::Page {
        title: article.title,
        byline: article.byline,
        site_name: article.site_name.or(Some(host)),
        published: article.published,
        body: article.markdown,
        kind: "html",
        extractor: article.extractor,
        encoding,
    })
}

/// 取得上限を人が読む単位で表す（エラー文言用）。
fn human_cap(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MiB", bytes / (1024 * 1024))
    } else {
        format!("{} KiB", bytes / 1024)
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

/// 本文を `cap` バイトまで読む（上限に達したら以降は受け取らない）。
async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
) -> Result<(Vec<u8>, bool), reqwest::Error> {
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = cap - body.len();
        if chunk.len() >= remaining {
            body.extend_from_slice(&chunk[..remaining]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}
