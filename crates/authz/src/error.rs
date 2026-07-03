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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unexpected_display_includes_status_and_body() {
        // Unexpected はステータスとボディを表示文字列に含むこと。
        let err = AuthzError::Unexpected {
            status: 404,
            body: "not found".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("404"));
        assert!(msg.contains("not found"));
    }

    #[test]
    fn invalid_model_display_includes_detail() {
        // InvalidModel は詳細メッセージを含むこと。
        let err = AuthzError::InvalidModel("type_definitions が空".to_string());
        let msg = err.to_string();
        assert!(msg.contains("type_definitions が空"));
        assert!(msg.contains("不正"));
    }

    #[test]
    fn store_unavailable_display_includes_name() {
        // StoreUnavailable は store 名を含むこと。
        let err = AuthzError::StoreUnavailable("shiki".to_string());
        let msg = err.to_string();
        assert!(msg.contains("shiki"));
    }

    #[test]
    fn decode_from_serde_json_error() {
        // serde_json::Error は #[from] で Decode に変換され、元エラーを表示に含むこと。
        let json_err = serde_json::from_str::<serde_json::Value>("{ invalid").unwrap_err();
        let inner_msg = json_err.to_string();
        let err: AuthzError = json_err.into();
        assert!(matches!(err, AuthzError::Decode(_)));
        let msg = err.to_string();
        assert!(msg.contains("解釈に失敗"));
        assert!(msg.contains(&inner_msg));
    }

    #[test]
    fn debug_format_available() {
        // Debug 実装が利用可能であること（ログ出力等で使用）。
        let err = AuthzError::StoreUnavailable("s".to_string());
        let dbg = format!("{err:?}");
        assert!(dbg.contains("StoreUnavailable"));
    }

    #[test]
    fn decode_variant_matches() {
        // 各 variant のパターンマッチが意図通り効くこと（負例含む）。
        let err = AuthzError::InvalidModel("x".to_string());
        assert!(!matches!(err, AuthzError::Decode(_)));
        assert!(matches!(err, AuthzError::InvalidModel(_)));
    }
}
