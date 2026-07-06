//! サンドボックス操作のエラー。

/// `Sandbox` トレイトの操作エラー。gRPC ステータス・トランスポート障害・ポリシ違反を包む。
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// 入力不正（パス・サイズ・spec の検証違反）。呼び出し側の誤り。
    #[error("invalid sandbox request: {0}")]
    Invalid(String),
    /// サンドボックスが見つからない（TTL 破棄後・未知の ID）。
    #[error("sandbox not found: {0}")]
    NotFound(String),
    /// orchestrator/sidecar の一時障害・トランスポート断。
    #[error("sandbox unavailable: {0}")]
    Unavailable(String),
    /// 未実装の機能（アルファ外の backend / mounts など）。
    #[error("sandbox feature unimplemented: {0}")]
    Unimplemented(String),
    /// 内部エラー。
    #[error("sandbox internal error: {0}")]
    Internal(String),
}
