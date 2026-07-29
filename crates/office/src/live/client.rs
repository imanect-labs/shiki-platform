//! CoolWSD への headless WS クライアント（issue #352）。
//!
//! AI を Collabora 共同編集セッションの**独立 view**として接続する。tile は
//! 一切要求しない（headless）。編集はすべて**自 view のカーソル・選択**に対して
//! 行われ、人間参加者の選択には影響しない。
//!
//! 応答待ちの設計（プロトコル事実は `protocol.rs` の doc 参照）:
//! - 検索 ack: `searchnotfound:` / 選択系コールバック（`unocommandresult:` は返らない）
//! - paste ack: `pasteresult: success|fallback`
//! - 保存 ack: `unocommandresult:`（`.uno:Save` は notify 対象）
//!
//! ops 開始後は再接続・再送しない（paste 非冪等・PIT-47）。

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use super::error::LiveError;

/// WS 接続エラーから **URL を落として種別だけ**残す（URL にはクエリの access_token が乗る）。
fn redact_ws_error(err: &tokio_tungstenite::tungstenite::Error) -> String {
    use tokio_tungstenite::tungstenite::Error as E;
    match err {
        E::Http(response) => format!("HTTP {}", response.status()),
        E::Url(_) => "URL が不正です".to_string(),
        E::Io(e) => format!("IO エラー: {}", e.kind()),
        E::Tls(_) => "TLS エラー".to_string(),
        E::Protocol(e) => format!("プロトコルエラー: {e}"),
        _ => "接続に失敗しました".to_string(),
    }
}
use super::protocol::{self, Loaded};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// headless セッションのタイムアウト設定。
#[derive(Debug, Clone)]
pub struct CoolWsConfig {
    /// `ws(s)://` の CoolWSD ベース URL（`collabora_base_url` の http→ws 置換）。
    pub ws_base: String,
    /// WS 接続＋ハンドシェイク送信の上限。
    pub connect_timeout: Duration,
    /// `loaded:` 到達までの上限（kit 起動＋WOPI GetFile 込み・大文書は遅い）。
    pub load_timeout: Duration,
    /// 1 op（検索 ack・選択照合・paste ack）の応答待ち上限。
    pub op_timeout: Duration,
    /// 保存 ack（`unocommandresult:`）の待ち上限。
    pub save_timeout: Duration,
    /// セッション全体のハード上限（暴走防止・chat worker の可視性タイムアウト内）。
    pub total_deadline: Duration,
}

impl CoolWsConfig {
    /// 既定タイムアウトで構築する。
    pub fn new(ws_base: impl Into<String>) -> Self {
        Self {
            ws_base: ws_base.into(),
            connect_timeout: Duration::from_secs(10),
            load_timeout: Duration::from_mins(1),
            op_timeout: Duration::from_secs(15),
            save_timeout: Duration::from_secs(30),
            total_deadline: Duration::from_mins(2),
        }
    }
}

/// 検索（`.uno:ExecuteSearch`）の一次判定。
///
/// `Unconfirmed` は ack コールバックが期限内に来なかっただけで、成否は
/// 直後の [`CoolWsClient::selection_text`] 照合が最終判定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchOutcome {
    /// `searchnotfound:` を受信（この view に選択は作られていない）。
    NotFound,
    /// 選択系コールバックを受信（見つかった箇所が自 view の選択になった）。
    Selected,
    /// ack が観測できなかった（選択照合で最終判定する）。
    Unconfirmed,
}

/// 保存の確認レベル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveAck {
    /// core の保存完了 ack を受信（ストレージ反映は呼び出し側が版で検証する）。
    CoreSaved,
    /// ack が期限内に来なかった（編集はセッション適用済み・永続化は他 view の
    /// 保存/autosave に委ねられる可能性がある）。
    Unverified,
}

/// CoolWSD セッションへの headless クライアント（1 接続 = 1 view）。
pub struct CoolWsClient {
    stream: WsStream,
    loaded: Loaded,
    op_timeout: Duration,
    save_timeout: Duration,
    hard_deadline: tokio::time::Instant,
}

impl CoolWsClient {
    /// 接続→`coolclient`→`load`→`loaded:` 待ちまでを行う。
    ///
    /// 既存セッションがあれば同一ドキュメントへ別 view として合流し、無ければ
    /// 新規セッションが立つ（どちらも正しい動作）。
    pub async fn connect(
        cfg: &CoolWsConfig,
        wopi_src: &str,
        access_token: &str,
        lang: &str,
    ) -> Result<Self, LiveError> {
        let url = protocol::session_ws_url(&cfg.ws_base, wopi_src, access_token);
        // CoolWSD は Origin 無しの WS アップグレードを拒否する（ClientRequestDispatcher の
        // allowedOrigin: `http(s)://<Host>` と same-origin なら許可）。ブラウザの
        // 同一オリジン接続と同じく Collabora 自身のオリジンを名乗る。
        // Origin は **スキーム＋オーソリティのみ**（ws_base にパスが含まれていても付けない）。
        let origin = protocol::http_origin(&cfg.ws_base)
            .ok_or_else(|| LiveError::Connect("ws_base から Origin を作れません".to_string()))?;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| LiveError::Connect(format!("WS リクエスト構築に失敗: {e}")))?;
        request.headers_mut().insert(
            "Origin",
            origin
                .parse()
                .map_err(|e| LiveError::Connect(format!("Origin ヘッダが不正: {e}")))?,
        );
        // 失敗メッセージに URL（＝クエリの access_token）を載せない。tungstenite の Error は
        // Http/Url 種別で URL 全体を含むことがあるため、**種別だけ**を残す（PIT-23 の秘匿）。
        let (mut stream, _response) =
            tokio::time::timeout(cfg.connect_timeout, connect_async(request))
                .await
                .map_err(|_| LiveError::Timeout("connect"))?
                .map_err(|e| LiveError::Connect(redact_ws_error(&e)))?;

        let hello = protocol::coolclient_line(chrono::Utc::now().timestamp_millis());
        send_text(&mut stream, hello)
            .await
            .map_err(|e| LiveError::Connect(e.to_string()))?;
        send_text(&mut stream, protocol::load_line(wopi_src, lang))
            .await
            .map_err(|e| LiveError::Connect(e.to_string()))?;

        // `loaded:` を待つ（coolserver/lokitversion/progress 等は読み捨てる）。
        let loaded = wait_matching(
            &mut stream,
            cfg.load_timeout,
            "load",
            protocol::parse_loaded,
        )
        .await
        .map_err(|e| match e {
            // load 中の失敗はすべて Load に写す（呼び出し側の分類を単純に保つ）。
            LiveError::Server { cmd, kind } => LiveError::Load(format!("cmd={cmd} kind={kind}")),
            LiveError::Closed(reason) => LiveError::Load(format!("close: {reason}")),
            other => other,
        })?;
        tracing::debug!(view_id = %loaded.view_id, views = loaded.views, "coolwsd loaded");
        Ok(Self {
            stream,
            loaded,
            op_timeout: cfg.op_timeout,
            save_timeout: cfg.save_timeout,
            hard_deadline: tokio::time::Instant::now() + cfg.total_deadline,
        })
    }

    /// `loaded:` の内容（view id・総 view 数）。
    pub fn loaded(&self) -> &Loaded {
        &self.loaded
    }

    /// 自 view で `needle` を検索し、見つかった箇所を選択する。
    pub async fn execute_search(&mut self, needle: &str) -> Result<SearchOutcome, LiveError> {
        self.send(protocol::uno_execute_search(needle)).await?;
        let timeout = self.phase_timeout(self.op_timeout)?;
        let waited = wait_matching(&mut self.stream, timeout, "search", |msg| {
            if msg.starts_with("searchnotfound:") {
                return Some(SearchOutcome::NotFound);
            }
            // Writer/Impress は textselection:、Calc はセルカーソル系が返る。
            let selected = msg
                .strip_prefix("textselection:")
                .is_some_and(|rest| !rest.trim().is_empty())
                || msg.starts_with("celladdress:")
                || msg.starts_with("cellcursor:")
                || msg.starts_with("searchresultselection:");
            selected.then_some(SearchOutcome::Selected)
        })
        .await;
        match waited {
            // ack が来ない場合も selection_text 照合が最終判定するため潰さない。
            Err(LiveError::Timeout(_)) => Ok(SearchOutcome::Unconfirmed),
            other => other,
        }
    }

    /// 自 view の選択内容（text/plain）を取得する。
    ///
    /// 選択が巨大・複合（`complexselection:`）の場合はプロトコル不整合として
    /// 失敗させる（アンカー照合の対象は短文のはずで、照合不能＝安全側で中止）。
    pub async fn selection_text(&mut self) -> Result<String, LiveError> {
        self.send(protocol::GET_TEXT_SELECTION_LINE.to_string())
            .await?;
        let timeout = self.phase_timeout(self.op_timeout)?;
        let body = wait_matching(&mut self.stream, timeout, "selection", |msg| {
            if msg.starts_with("complexselection:") {
                return Some(None);
            }
            protocol::parse_textselectioncontent(msg).map(|s| Some(s.to_string()))
        })
        .await?;
        body.ok_or_else(|| LiveError::Protocol("complexselection（照合不能な選択）".into()))
    }

    /// 自 view のセルカーソルを `cell_ref` へ移す（Calc）。
    ///
    /// ack（`celladdress:`）は best-effort で短く待つ。メッセージはセッション内で
    /// 順序処理されるため、後続 paste の時点で移動は反映済み（ack 不達＝失敗ではない）。
    pub async fn go_to_cell(&mut self, cell_ref: &str) -> Result<(), LiveError> {
        self.send(protocol::uno_go_to_cell(cell_ref)).await?;
        let timeout = self.phase_timeout(self.op_timeout.min(Duration::from_secs(2)))?;
        let waited = wait_matching(&mut self.stream, timeout, "gotocell", |msg| {
            msg.starts_with("celladdress:").then_some(())
        })
        .await;
        match waited {
            Ok(()) | Err(LiveError::Timeout(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// 文書末尾へカーソルを移す（Writer・append 用）。ack なし（順序処理保証で足りる）。
    pub async fn go_to_end_of_doc(&mut self) -> Result<(), LiveError> {
        self.send(protocol::UNO_GO_TO_END_OF_DOC.to_string()).await
    }

    /// 選択中の列幅を内容に合わせる（Calc・#385）。ack なし（notify 対象外）。
    ///
    /// 引数を取らないコマンドなのでダイアログは開かず、後続の `.uno:Save` ack を
    /// 塞がない。効果の確認手段は無い＝呼び出し側は best-effort として扱うこと。
    pub async fn set_optimal_column_width(&mut self) -> Result<(), LiveError> {
        self.send(protocol::UNO_SET_OPTIMAL_COLUMN_WIDTH.to_string())
            .await
    }

    /// 自 view の現在選択（無選択ならカーソル位置）へ貼り付ける。
    ///
    /// 戻り値は `pasteresult:` の成否（`fallback` は core が処理できなかった形式）。
    pub async fn paste(&mut self, mime: &str, data: &[u8]) -> Result<bool, LiveError> {
        let frame = protocol::paste_frame(mime, data);
        self.stream
            .send(Message::Binary(frame.into()))
            .await
            .map_err(|e| LiveError::Closed(e.to_string()))?;
        let timeout = self.phase_timeout(self.op_timeout)?;
        wait_matching(
            &mut self.stream,
            timeout,
            "paste",
            protocol::parse_pasteresult,
        )
        .await
    }

    /// 保存する（`save dontTerminateEdit=1 dontSaveIfUnmodified=1`）。
    ///
    /// core の保存 ack までを待つ。ストレージ（WOPI PutFile）への反映は呼び出し側が
    /// 版の前進で検証する（shiki 自身が WOPI ホストなので観測できる）。
    pub async fn save(&mut self) -> Result<SaveAck, LiveError> {
        self.send(protocol::SAVE_LINE.to_string()).await?;
        let timeout = self.phase_timeout(self.save_timeout)?;
        let waited = wait_matching(&mut self.stream, timeout, "save", |msg| {
            protocol::parse_unocommandresult(msg)
                .filter(|(command, _)| command == ".uno:Save")
                .map(|(_, success)| success)
        })
        .await;
        match waited {
            Ok(true) => Ok(SaveAck::CoreSaved),
            Ok(false) => Err(LiveError::SaveFailed("core が保存失敗を返しました".into())),
            Err(LiveError::Timeout(_)) => Ok(SaveAck::Unverified),
            Err(LiveError::Server { cmd, kind }) if cmd == "storage" => {
                Err(LiveError::SaveFailed(format!("storage: {kind}")))
            }
            Err(e) => Err(e),
        }
    }

    /// セッションを閉じる（best-effort・kit プロセスのリーク防止のため必ず呼ぶ）。
    pub async fn close(mut self) {
        let _ = self.stream.close(None).await;
    }

    /// フェーズ待ち時間をハード上限で丸める（超過済みなら即タイムアウト）。
    fn phase_timeout(&self, want: Duration) -> Result<Duration, LiveError> {
        let remaining = self
            .hard_deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return Err(LiveError::Timeout("total"));
        }
        Ok(want.min(remaining))
    }

    async fn send(&mut self, line: String) -> Result<(), LiveError> {
        send_text(&mut self.stream, line).await
    }
}

/// テキスト 1 行を送る。
async fn send_text(stream: &mut WsStream, line: String) -> Result<(), LiveError> {
    stream
        .send(Message::Text(line.into()))
        .await
        .map_err(|e| LiveError::Closed(e.to_string()))
}

/// 次のアプリケーションフレームをテキストとして読む（Ping/Pong は読み飛ばす）。
async fn next_frame_text(stream: &mut WsStream) -> Result<String, LiveError> {
    loop {
        let msg = stream
            .next()
            .await
            .ok_or_else(|| LiveError::Closed("WS ストリームが終了しました".into()))?
            .map_err(|e| LiveError::Closed(e.to_string()))?;
        match msg {
            Message::Text(text) => return Ok(text.to_string()),
            // プロトコル上テキストメッセージも binary フレームで届き得る。
            // tile 等のバイナリ付きメッセージは lossy 変換で先頭行だけ意味を持つ
            // （headless は tile を要求しないため実質発生しない）。
            Message::Binary(bytes) => return Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Message::Close(frame) => {
                let reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
                return Err(LiveError::Closed(reason));
            }
            // Ping への Pong は tungstenite が read 時に自動応答する。
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

/// `matcher` が Some を返すメッセージを待つ。
///
/// 待機中に `error:` / `close:` を受けたら即座に失敗させる（fail fast）。
/// 関心外のメッセージ（viewinfo/statechanged/invalidate 等）は読み捨てる。
async fn wait_matching<T>(
    stream: &mut WsStream,
    timeout: Duration,
    phase: &'static str,
    mut matcher: impl FnMut(&str) -> Option<T>,
) -> Result<T, LiveError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let msg = match tokio::time::timeout_at(deadline, next_frame_text(stream)).await {
            Ok(result) => result?,
            Err(_) => return Err(LiveError::Timeout(phase)),
        };
        if let Some((cmd, kind)) = protocol::parse_error(&msg) {
            return Err(LiveError::Server { cmd, kind });
        }
        if let Some(reason) = protocol::parse_close_reason(&msg) {
            return Err(LiveError::Closed(reason));
        }
        if let Some(matched) = matcher(&msg) {
            return Ok(matched);
        }
    }
}
