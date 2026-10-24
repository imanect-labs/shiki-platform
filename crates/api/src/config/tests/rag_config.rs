//! RAG の設定（`SHIKI__RAG__*`）の読み込みのテスト。

use super::*;

#[test]
fn empty_min_rerank_score_env_means_disabled() {
    // compose は未設定を空文字で渡す（`${SHIKI__RAG__MIN_RERANK_SCORE:-}`）。空なら足切りしない。
    let parsed: rag::RagConfig = figment::Figment::from(figment::providers::Serialized::defaults(
        rag::RagConfig::default(),
    ))
    .merge(("min_rerank_score", ""))
    .extract()
    .unwrap();
    assert_eq!(parsed.min_rerank_score, None);
    let parsed: rag::RagConfig = figment::Figment::from(figment::providers::Serialized::defaults(
        rag::RagConfig::default(),
    ))
    .merge(("min_rerank_score", "0.45"))
    .extract()
    .unwrap();
    assert_eq!(parsed.min_rerank_score, Some(0.45));
}

#[test]
fn rag_query_hash_key_is_read_from_env_and_empty_means_unset() {
    // 検索語ハッシュの鍵（#833）は `SHIKI__RAG__QUERY_HASH_KEY` から読む。compose は未設定の env を
    // 空文字で渡すので、空は未設定（起動時乱数）として扱う。数字だけの鍵も文字列として受ける。
    use figment::providers::{Env, Serialized};
    let load = |prefix: &str| -> Result<AppConfig, Box<figment::Error>> {
        Figment::new()
            .merge(Serialized::defaults(valid_config()))
            .merge(Env::prefixed(prefix).split("__"))
            .extract()
            .map_err(Box::new)
    };
    std::env::set_var("SHIKITEST833A__RAG__QUERY_HASH_KEY", "k".repeat(32));
    std::env::set_var("SHIKITEST833B__RAG__QUERY_HASH_KEY", "");
    std::env::set_var("SHIKITEST833C__RAG__QUERY_HASH_KEY", "short");
    std::env::set_var("SHIKITEST833D__RAG__QUERY_HASH_KEY", "1".repeat(40));
    assert!(load("SHIKITEST833A__")
        .unwrap()
        .rag
        .query_hash_key
        .is_some());
    assert!(load("SHIKITEST833B__")
        .unwrap()
        .rag
        .query_hash_key
        .is_none());
    assert!(load("SHIKITEST833C__").is_err(), "短すぎる鍵は起動時に弾く");
    assert!(load("SHIKITEST833D__")
        .unwrap()
        .rag
        .query_hash_key
        .is_some());
}

#[test]
fn rag_service_secrets_treat_empty_as_unset_and_never_serialize() {
    // compose は未設定を空文字で渡す。空なら「未設定」（無認証で送り、起動時に警告する）。
    let extract = |key: &str, value: &str| -> rag::RagConfig {
        figment::Figment::from(figment::providers::Serialized::defaults(
            rag::RagConfig::default(),
        ))
        .merge((key, value))
        .extract()
        .unwrap()
    };
    assert_eq!(extract("qdrant_api_key", "").qdrant_api_key, None);
    assert_eq!(extract("worker_auth_token", "  ").worker_auth_token, None);
    assert_eq!(
        extract("qdrant_api_key", "k-1").qdrant_api_key.as_deref(),
        Some("k-1")
    );
    assert_eq!(
        extract("worker_auth_token", "t-1")
            .worker_auth_token
            .as_deref(),
        Some("t-1")
    );
    // 秘密値は直列化に出さない（設定の直列化ダンプ経由の漏えいを防ぐ。`Debug` は対象外で、config を Debug 出力する箇所は無い）。
    let cfg = rag::RagConfig {
        qdrant_api_key: Some("k-secret".into()),
        worker_auth_token: Some("t-secret".into()),
        ..rag::RagConfig::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(
        !json.contains("k-secret") && !json.contains("t-secret"),
        "{json}"
    );
}
