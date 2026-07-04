//! HTTP ルートハンドラ群。

pub mod admin;
pub mod auth;
pub mod directory;
pub mod files;
pub mod folders;
pub mod me;
pub mod shares;

pub use me::get_me;
