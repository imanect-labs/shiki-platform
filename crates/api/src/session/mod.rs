//! BFF セッション層（オパーク Cookie + ストア）。
//!
//! ブラウザにはトークンを置かず、不透明な session id（Cookie）のみを渡す。
//! 本体は [`SessionStore`] に `tenant_id` スコープで保持する。

pub mod memory;
pub mod redis_store;
pub mod store;

use base64::Engine;
use rand::RngCore;

pub use memory::MemorySessionStore;
pub use redis_store::RedisSessionStore;
pub use store::{SessionError, SessionRecord, SessionStore};

/// セッション Cookie 名（不透明 session id を運ぶ・httpOnly）。
///
/// ブラウザの JS は読まない（httpOnly）が、CSRF Cookie 名（[`CSRF_COOKIE`]）はフロントが
/// double-submit のため読む。フロント（`web/src/lib/api.ts`）が同名をハードコードするので、
/// **設定可能にせず定数で固定**してドリフト（名前変更で web が CSRF を送れず 403）を防ぐ。
pub const SESSION_COOKIE: &str = "shiki_session";
/// CSRF Cookie 名（double-submit 用・JS から読めるよう httpOnly にしない）。
pub const CSRF_COOKIE: &str = "shiki_csrf";

/// 推測不能な不透明トークンを生成する（session id / CSRF / OIDC state など）。
///
/// 32 バイトの乱数を URL-safe base64（パディング無し）で表現する。
pub fn new_opaque_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn cookie_names_are_stable_constants() {
        // フロントがハードコードするため、名前ドリフトを防ぐ固定値であること。
        assert_eq!(SESSION_COOKIE, "shiki_session");
        assert_eq!(CSRF_COOKIE, "shiki_csrf");
    }

    #[test]
    fn opaque_token_length_is_43_chars() {
        // 32 バイトを base64url(no-pad) で表すと 43 文字（ceil(32*4/3)）。
        let token = new_opaque_token();
        assert_eq!(token.len(), 43);
    }

    #[test]
    fn opaque_token_is_url_safe_no_pad() {
        // URL-safe 文字種のみ・パディング無し（`+` `/` `=` を含まない）。
        let token = new_opaque_token();
        assert!(token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert!(!token.contains('='));
    }

    #[test]
    fn opaque_tokens_are_unique() {
        // 連続生成でも衝突しない（推測不能・乱数源）。
        let count = 1000;
        let tokens: HashSet<String> = (0..count).map(|_| new_opaque_token()).collect();
        assert_eq!(tokens.len(), count);
    }
}
