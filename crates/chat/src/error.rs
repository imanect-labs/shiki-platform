//! チャットドメインのエラー。

/// チャットの操作エラー。api 層で `ApiError` へ写す（`From` 実装は api 側）。
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    /// スレッド/メッセージが見つからない。
    #[error("not found")]
    NotFound,
    /// 認可拒否（閲覧/編集/共有権限なし）。
    #[error("forbidden")]
    Forbidden,
    /// 入力不正（空メッセージ・不正な role 等）。
    #[error("invalid request: {0}")]
    Invalid(String),
    /// LLM プロバイダ等の一時障害（再試行可能・503 相当）。
    #[error("service unavailable: {0}")]
    Unavailable(String),
    /// 内部エラー（DB/authz/シリアライズ等）。
    #[error("internal error: {0}")]
    Internal(String),
}
