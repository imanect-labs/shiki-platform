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
