//! shiki-authz — OpenFGA(ReBAC) クライアントと認可コンテキスト。
//!
//! 設計上の不変条件（docs/design.md §4.1, architecture-invariants）:
//! - 認可判定は [`AuthzClient::check`] 経由の単一の問いに帰着させる（単一チョークポイント）。
//!   個別ハンドラに権限ロジックを散らさない。
//! - 全データアクセスは [`AuthContext`]（`principal` + `org`）を受け取る。アンビエント権限を持たない。
//!   将来 `tenant_id` を足す継ぎ目もここに置く。
//! - OpenFGA に送る relation / object type 名は [`vocab`] の enum を単一ソースとし、
//!   手書き文字列を作らない（LLM ハルシネーション境界）。

pub mod client;
pub mod context;
pub mod error;
pub mod fga_http;
pub mod migrate;
pub mod model;
pub mod object;
pub mod vocab;

pub use client::{AuthzClient, Consistency, OpenFgaClient, OpenFgaConfig, ReadTupleKey};
pub use context::{AuthContext, Principal};
pub use error::AuthzError;
pub use object::{FgaObject, Namespace, Subject, TENANT_SEP};
pub use vocab::{ObjectType, Relation};
