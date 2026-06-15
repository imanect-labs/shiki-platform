use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthzError {
    #[error("OpenFGA への HTTP 通信に失敗しました: {0}")]
    Http(#[from] reqwest::Error),

    #[error("OpenFGA が予期しない応答を返しました (status {status}): {body}")]
    Unexpected { status: u16, body: String },

    #[error("OpenFGA の応答 JSON の解釈に失敗しました: {0}")]
    Decode(#[from] serde_json::Error),

    #[error("authorization model の構成が不正です: {0}")]
    InvalidModel(String),

    #[error("OpenFGA store '{0}' が見つからず、作成にも失敗しました")]
    StoreUnavailable(String),
}
