//! プロバイダへ名乗る識別ヘッダ（User-Agent・会話 ID）。全 HTTP アダプタで共有する。
//!
//! 呼び出し元の名乗りと会話ごとの ID を求めるプロバイダ（OpenCode Go は `x-opencode-session`
//! が無いと Chat Completions・Messages・Responses のどれも 400 `MissingSessionID`）に向けて、
//! 設定（`llm.user_agent` / `llm.session_header`）のとおりにヘッダを足す。未設定なら何も足さない。

use reqwest::header::{HeaderName, HeaderValue};
use reqwest::RequestBuilder;

use crate::provider::LlmError;

/// アダプタ自身が送る（または HTTP クライアントが管理する）ヘッダ。会話 ID のヘッダ名には使えない。
/// `HeaderName` は小文字に正規化されるので小文字で持つ。
const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "anthropic-version",
    "content-type",
    "content-length",
    "user-agent",
    "host",
];

/// 送出する識別ヘッダの設定。
#[derive(Debug, Clone, Default)]
pub(crate) struct IdentityHeaders {
    user_agent: Option<HeaderValue>,
    session_header: Option<HeaderName>,
}

impl IdentityHeaders {
    /// User-Agent を設定する。ヘッダ値にできない文字列は設定エラーにする
    /// （起動時に気づけるように。リクエストごとに失敗させない）。
    pub(crate) fn set_user_agent(&mut self, user_agent: Option<&str>) -> Result<(), LlmError> {
        self.user_agent = user_agent
            .map(|ua| {
                HeaderValue::from_str(ua.trim())
                    .map_err(|e| LlmError::Config(format!("invalid llm.user_agent: {e}")))
            })
            .transpose()?;
        Ok(())
    }

    /// 会話 ID を載せるヘッダ名を設定する。ヘッダ名にできない文字列と、アダプタ自身が送る
    /// ヘッダ（認証・本文の型など）と重なる名前は設定エラーにする（値が二重に載って弾かれる）。
    pub(crate) fn set_session_header(&mut self, header: Option<&str>) -> Result<(), LlmError> {
        self.session_header = header
            .map(|h| {
                let name = HeaderName::from_bytes(h.trim().as_bytes())
                    .map_err(|e| LlmError::Config(format!("invalid llm.session_header: {e}")))?;
                if RESERVED_HEADERS.contains(&name.as_str()) {
                    return Err(LlmError::Config(format!(
                        "invalid llm.session_header: `{name}` is reserved"
                    )));
                }
                Ok(name)
            })
            .transpose()?;
        Ok(())
    }

    /// リクエストにヘッダを足す。ヘッダ名を設定しているのに会話 ID が無い（またはヘッダ値に
    /// できない）呼び出しには、呼び出しごとの ID を付ける（ヘッダ必須のプロバイダで落ちないように）。
    pub(crate) fn apply(
        &self,
        mut builder: RequestBuilder,
        session_id: Option<&str>,
    ) -> RequestBuilder {
        if let Some(ua) = &self.user_agent {
            builder = builder.header(reqwest::header::USER_AGENT, ua.clone());
        }
        if let Some(header) = &self.session_header {
            let sid = session_id
                .and_then(|s| HeaderValue::from_str(s).ok())
                .unwrap_or_else(|| {
                    HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
                        .unwrap_or_else(|_| HeaderValue::from_static("shiki"))
                });
            builder = builder.header(header.clone(), sid);
        }
        builder
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_settings_are_config_errors() {
        let mut h = IdentityHeaders::default();
        assert!(matches!(
            h.set_session_header(Some("x opencode session")),
            Err(LlmError::Config(_))
        ));
        assert!(matches!(
            h.set_user_agent(Some("shiki/1.0\nx-injected: 1")),
            Err(LlmError::Config(_))
        ));
        // アダプタ自身が送るヘッダと重なる名前は拒否する（大文字小文字を問わない）。
        for reserved in ["Authorization", "x-api-key", "user-agent"] {
            assert!(matches!(
                h.set_session_header(Some(reserved)),
                Err(LlmError::Config(_))
            ));
        }
        // 前後の空白は落として受け付ける（.env の書き方の揺れ）。
        assert!(h.set_session_header(Some(" x-opencode-session ")).is_ok());
    }

    #[test]
    fn apply_sends_session_id_or_a_per_call_id() {
        let mut h = IdentityHeaders::default();
        h.set_user_agent(Some("shiki/1.0")).unwrap();
        h.set_session_header(Some("x-opencode-session")).unwrap();
        let http = reqwest::Client::new();
        let req = h
            .apply(http.post("http://x/"), Some("thread-1"))
            .build()
            .unwrap();
        assert_eq!(req.headers()["user-agent"], "shiki/1.0");
        assert_eq!(req.headers()["x-opencode-session"], "thread-1");
        let req = h.apply(http.post("http://x/"), None).build().unwrap();
        let sid = req.headers()["x-opencode-session"].to_str().unwrap();
        assert!(uuid::Uuid::parse_str(sid).is_ok(), "per-call uuid: {sid}");
    }

    #[test]
    fn unset_adds_nothing() {
        let req = IdentityHeaders::default()
            .apply(reqwest::Client::new().post("http://x/"), Some("t"))
            .build()
            .unwrap();
        assert!(req.headers().get("user-agent").is_none());
        assert!(req.headers().is_empty());
    }
}
