//! CoolWSD headless セッションのエラー分類（issue #352）。
//!
//! リトライ可否の判断単位で分ける:
//! - [`LiveError::Connect`] のみ再接続 1 回を許す（load 前＝冪等）。
//! - ops 開始後は**いかなる失敗でも再送しない**（paste は非冪等・二重貼付は
//!   選択ずれ以上の事故）。fail fast で部分適用を正直に報告する（PIT-45）。

/// headless セッション（`live::CoolWsClient` / `live::LiveEditor`）のエラー。
#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    /// WS 接続・ハンドシェイク失敗（load 前）。1 回だけ再接続してよい。
    #[error("CoolWSD へ接続できません: {0}")]
    Connect(String),
    /// ドキュメント load の失敗（`error: cmd=load` 等・`loaded:` 不達）。
    #[error("ドキュメントの読込に失敗しました: {0}")]
    Load(String),
    /// サーバの `error:` 通知（ops 中）。cmd/kind は内部ログ用で、ユーザー向け
    /// メッセージには詳細を流さない（ツール層が観測文言に変換する）。
    #[error("CoolWSD がエラーを返しました: cmd={cmd} kind={kind}")]
    Server { cmd: String, kind: String },
    /// フェーズ別タイムアウト（load / op / save）。
    #[error("CoolWSD の応答がタイムアウトしました（{0}）")]
    Timeout(&'static str),
    /// プロトコル不整合（想定外の応答・パース不能）。黙って続行しない。
    #[error("CoolWSD プロトコル不整合: {0}")]
    Protocol(String),
    /// セッション切断（`close: <reason>`・WS 切断）。ops 中なら部分適用あり得る。
    #[error("セッションが切断されました: {0}")]
    Closed(String),
    /// 保存の失敗（`error: cmd=storage` 等）。編集自体はセッション適用済みの
    /// 可能性が高い（他 view の保存/autosave で永続化され得る）。
    #[error("保存に失敗しました: {0}")]
    SaveFailed(String),
}
