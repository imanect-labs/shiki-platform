//! HTTP ルートハンドラ群。

pub mod admin;
pub mod auth;
pub mod directory;
pub mod files;
pub mod folders;
pub mod me;
pub mod search;
pub mod shares;

pub use me::get_me;
