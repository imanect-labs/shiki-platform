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

/// セッション Cookie の値（不透明 session id ＋ テナントスコープ）。形式 `{session_id}.{tenant_id}`。
///
/// セッションストアのキーは `tenant_id` でスコープされるため（[`SessionStore`]）、後続リクエストで
/// session を引くには **Cookie だけからテナントを解決**できる必要がある。single テナントは設定の
/// 固定値で解決できるが、multi テナント（SaaS）では従来 host/サブドメイン解決（SAAS.1）が必要で
/// 単一ホストの dev では成立しなかった。そこで発行時にテナントスコープを Cookie へ束ねる。
///
/// `session_id` は [`new_opaque_token`]＝base64url(no-pad) で `.` を含まないため、**最初の `.`** で
/// 分割すれば tenant slug が任意でも曖昧さなく復元できる。唯一の秘密は推測不能な `session_id` で、
/// tenant slug は SAAS.1 の host ベース解決でも URL に現れる非機密のルーティング情報。
pub fn encode_session_cookie(session_id: &str, tenant_id: &str) -> String {
    format!("{session_id}.{tenant_id}")
}

/// セッション Cookie の値を `(session_id, tenant_id)` に分解する。形式不正なら `None`。
pub fn decode_session_cookie(value: &str) -> Option<(&str, &str)> {
    value
        .split_once('.')
        .filter(|(session_id, tenant_id)| !session_id.is_empty() && !tenant_id.is_empty())
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
    fn session_cookie_round_trips_session_id_and_tenant() {
        // 発行時のスコープ束ね（session_id.tenant）が往復で復元できること。
        let sid = new_opaque_token();
        let cookie = encode_session_cookie(&sid, "a-corp");
        let (got_sid, got_tenant) = decode_session_cookie(&cookie).unwrap();
        assert_eq!(got_sid, sid);
        assert_eq!(got_tenant, "a-corp");
    }

    #[test]
    fn session_cookie_splits_on_first_dot_only() {
        // session_id は base64url で `.` を含まないため、最初の `.` 分割で tenant に `.` が
        // あっても session_id を曖昧さなく復元できる。
        let (sid, tenant) = decode_session_cookie("abc123.tenant.with.dots").unwrap();
        assert_eq!(sid, "abc123");
        assert_eq!(tenant, "tenant.with.dots");
    }

    #[test]
    fn session_cookie_rejects_malformed() {
        // テナント無し（`.` 欠落）や空セグメントは不正として拒否。
        assert!(decode_session_cookie("nodot").is_none());
        assert!(decode_session_cookie(".tenant").is_none());
        assert!(decode_session_cookie("sid.").is_none());
        assert!(decode_session_cookie("").is_none());
    }

    #[test]
    fn opaque_tokens_are_unique() {
        // 連続生成でも衝突しない（推測不能・乱数源）。
        let count = 1000;
        let tokens: HashSet<String> = (0..count).map(|_| new_opaque_token()).collect();
        assert_eq!(tokens.len(), count);
    }
}
