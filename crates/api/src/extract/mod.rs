//! axum extractor 群（Principal / AuthContext）。

pub mod auth_context;
pub mod principal;

pub use auth_context::AuthContextExt;
pub(crate) use auth_context::{resolve_tenant_id, TenantId};
pub use principal::AuthPrincipal;
