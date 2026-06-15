//! axum extractor 群（Principal / AuthContext）。

pub mod auth_context;
pub mod principal;

pub use auth_context::AuthContextExt;
pub use principal::AuthPrincipal;
