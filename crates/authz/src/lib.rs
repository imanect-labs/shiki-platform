//! shiki-authz — OpenFGA(ReBAC) クライアントと認可コンテキスト。
//!
//! 設計上の不変条件（docs/design.md §4.1, architecture-invariants）:
//! - 認可判定は [`AuthzClient::check`] 経由の単一の問いに帰着させる（単一チョークポイント）。
//!   個別ハンドラに権限ロジックを散らさない。
//! - 全データアクセスは [`AuthContext`]（`principal` + `org`）を受け取る。アンビエント権限を持たない。
//!   将来 `tenant_id` を足す継ぎ目もここに置く。
//! - OpenFGA に送る relation / object type 名は [`vocab`] の enum を単一ソースとし、
//!   手書き文字列を作らない（LLM ハルシネーション境界）。

// #[cfg(test)] のユニットテストは本番コードのみ厳格化する pedantic/安全系 lint を許容する。
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::pedantic,
        clippy::cognitive_complexity
    )
)]

pub mod client;
pub mod context;
pub mod error;
// `.fga` ↔ `.json` の userset 構造 drift 検査（テスト専用・外部依存なし・#66）。
#[cfg(test)]
mod fga_dsl;
pub mod fga_http;
pub mod ident;
pub mod migrate;
pub mod model;
pub mod object;
pub mod vocab;

pub use client::{AuthzClient, Consistency, OpenFgaClient, OpenFgaConfig, ReadTupleKey};
pub use context::{AuthContext, Principal, PrincipalKind};
pub use error::AuthzError;
pub use ident::{validate_local_id, validate_tenant_id, IdentViolation};
pub use object::{FgaObject, Namespace, Subject, TENANT_SEP};
pub use vocab::{ObjectType, Relation};
