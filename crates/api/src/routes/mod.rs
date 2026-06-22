//! HTTP ルートハンドラ群。

pub mod auth;
pub mod files;
pub mod me;

pub use me::get_me;
