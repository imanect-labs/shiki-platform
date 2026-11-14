//! エンジンのトレイト境界（docs/agent/browser.md §3・§12-1 で承認済み）。
//!
//! cloud/onprem・エンジン差（JS 非実行のテキストエンジン / Chromium）はここで吸収する。
//! cookie jar はエンジンではなく `BrowserSession` が持ち、呼び出しのたびに渡す（§5.1）。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use authz::AuthContext;
use url::{Origin, Url};

use crate::cookie_jar::CookieJar;
use crate::snapshot::{EngineKind, RefId, Snapshot};

/// リダイレクトの転送先を照合する関数（true なら追従してよい）。
pub type RedirectCheck = Arc<dyn Fn(&Url) -> bool + Send + Sync>;

/// セッションを開くときの設定。
#[derive(Clone, Default)]
pub struct SessionOpts {
    /// ref の世代の開始値。エンジンを切り替えても世代が巻き戻らないよう、セッションが渡す。
    pub first_generation: u32,
    /// リダイレクトの転送先の照合（出所の規則・browser.md §6.2）。None なら関門の検証だけで追従する。
    pub redirect_check: Option<RedirectCheck>,
}

impl fmt::Debug for SessionOpts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionOpts")
            .field("first_generation", &self.first_generation)
            .field("redirect_check", &self.redirect_check.is_some())
            .finish()
    }
}

/// 操作が送ることになる要求（送る前に、ツールが承認の要否を判定するために使う・browser.md §6.1）。
#[derive(Debug, Clone)]
pub struct PlannedRequest {
    pub request: LoadRequest,
    /// 送る値のうち、承認ダイアログで伏せる項目の名前（パスワード欄）。
    pub masked_fields: Vec<String>,
}

/// HTTP メソッド（ブラウザが送るものだけ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// ページの読み込み要求。
#[derive(Debug, Clone)]
pub struct LoadRequest {
    pub url: Url,
    pub method: Method,
    /// `application/x-www-form-urlencoded` の本文（POST のとき）。
    pub form: Vec<(String, String)>,
    /// 要求を起こしたページ（SameSite の判定に使う）。None はアドレスバー相当。
    pub initiator: Option<Url>,
    /// 承認した送り先のオリジン。POST が 307 / 308 でこれ以外へ転送されたら止める（§8）。
    pub approved_origin: Option<Origin>,
}

impl LoadRequest {
    /// アドレスバー相当の GET（ユーザーが書いた URL・モデルの navigate）。
    #[must_use]
    pub fn get(url: Url) -> Self {
        LoadRequest {
            url,
            method: Method::Get,
            form: Vec::new(),
            initiator: None,
            approved_origin: None,
        }
    }
}

/// ページへの操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// リンク・ボタン・チェックボックス・ラジオをクリックする。
    Click(RefId),
    /// テキスト入力欄に値を入れる。
    Fill(RefId, String),
    /// 選択肢を選ぶ（value か表示名）。
    Select(RefId, String),
    /// 入力欄を含むフォームを送信する（Enter 相当）。
    Submit(RefId),
    /// 前のページに戻る（再取得せず、保持した応答を表示し直す・§5.3）。
    Back,
}

/// エンジンの失敗。文言はモデルに返してよい（ページ由来の値は含めない）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrowserError {
    #[error("この URL は開けません: {0}")]
    Rejected(String),
    #[error("取得に失敗しました: {0}")]
    Fetch(String),
    #[error("リダイレクトが多すぎます")]
    TooManyRedirects,
    #[error("承認した送り先（{approved}）から {to} への転送が要求されたため、送信を止めました")]
    RedirectBlocked { approved: String, to: Url },
    #[error("HTML ではありません（{content_type}）。文書は web_fetch で読んでください")]
    NotHtml { content_type: String },
    #[error("ref {0} は現在のページにありません。snapshot を取り直してください")]
    StaleRef(RefId),
    #[error("この要素は {0} できません")]
    NotInteractive(&'static str),
    #[error("このボタンは JavaScript で動作します（JS 非実行のエンジンでは押せません）")]
    NeedsJs { label: String },
    #[error("その選択肢はありません")]
    NoSuchOption,
    #[error("この送信方式（{0}）には対応していません")]
    Unsupported(String),
    #[error("リダイレクトの転送先（{to}）はこの操作では開けません")]
    RedirectNotAllowed { to: Url },
    #[error("戻れる履歴がありません")]
    NoHistory,
    #[error("まだページを開いていません")]
    NoPage,
}

/// エンジン（ページを開く工場）。
#[async_trait]
pub trait BrowserEngine: Send + Sync {
    fn kind(&self) -> EngineKind;

    /// セッション用のページを開く（まだ何も読み込まない）。`ctx` は監査・上限の単位。
    async fn open(
        &self,
        ctx: &AuthContext,
        opts: &SessionOpts,
    ) -> Result<Box<dyn EnginePage>, BrowserError>;
}

/// 1 セッションのページ。操作は 1 つずつ順に呼ばれる（同時には呼ばれない）。
#[async_trait]
pub trait EnginePage: Send {
    /// ページを読み込む。
    async fn load(
        &mut self,
        req: LoadRequest,
        jar: &mut CookieJar,
    ) -> Result<Snapshot, BrowserError>;

    /// ページへの操作。遷移しない操作（入力）も、反映後のスナップショットを返す。
    async fn act(&mut self, action: Action, jar: &mut CookieJar) -> Result<Snapshot, BrowserError>;

    /// 現在のスナップショット。
    async fn snapshot(&mut self) -> Result<Snapshot, BrowserError>;

    /// 操作が送ることになる要求を、送らずに返す（遷移しない操作は None）。ツールはこれで
    /// 送り先・メソッドを確かめてから、範囲外なら実行を拒否する（browser.md §6.1）。
    fn plan(&self, action: &Action) -> Result<Option<PlannedRequest>, BrowserError>;

    /// 次に使う世代（エンジン切替でセッションが引き継ぐ）。
    fn next_generation(&self) -> u32;
}
