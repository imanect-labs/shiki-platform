//! RAG サブシステムのエラー型。

use authz::AuthzError;

#[derive(Debug, thiserror::Error)]
pub enum RagError {
    /// worker のパース失敗（422 の構造化エラー）。握りつぶさず job の last_error に記録する。
    #[error("パース失敗 [{code}]: {detail}")]
    Parse { code: String, detail: String },

    /// worker / Qdrant への HTTP 呼び出し失敗（接続・タイムアウト・5xx）。リトライ対象。
    #[error("HTTP エラー: {0}")]
    Http(#[from] reqwest::Error),

    /// 埋め込みモデル版の不一致（PIT-8 ガード）。設定と worker のモデルを揃えるまで
    /// インジェストを拒否する（インデックス単位で version 固定）。
    #[error("埋め込みモデル版の不一致: 設定={expected} worker={actual}")]
    EmbeddingVersionMismatch { expected: String, actual: String },

    /// worker の非 2xx（422 以外）。一時障害としてリトライする。
    #[error("worker エラー: {0}")]
    Worker(String),

    #[error("ベクタストアエラー: {0}")]
    Vector(String),

    #[error("全文索引エラー: {0}")]
    Fulltext(String),

    #[error("認可エラー: {0}")]
    Authz(#[from] AuthzError),

    #[error("DB エラー: {0}")]
    Db(#[from] sqlx::Error),

    #[error("設定エラー: {0}")]
    Config(String),
}

impl RagError {
    /// リトライで回復しうる一時エラーか（consumer のバックオフ/DLQ 判断に使う）。
    ///
    /// パース失敗・版不一致・設定エラーは再試行しても直らない恒久エラー。
    pub fn is_transient(&self) -> bool {
        match self {
            RagError::Http(_)
            | RagError::Worker(_)
            | RagError::Vector(_)
            | RagError::Fulltext(_)
            | RagError::Db(_)
            | RagError::Authz(_) => true,
            RagError::Parse { .. }
            | RagError::EmbeddingVersionMismatch { .. }
            | RagError::Config(_) => false,
        }
    }
}
