//! axum extractor 群（Principal / AuthContext）。

pub mod auth_context;
pub mod principal;
pub mod trace_id;

pub use auth_context::AuthContextExt;
pub(crate) use auth_context::{resolve_org, resolve_tenant_id, validate_tenant_id, TenantId};
pub use principal::AuthPrincipal;
pub use trace_id::{TraceId, TraceIdExt};
