//! 行レベル認可述語エンジン（Task 9.3・design §4.10）。
//!
//! テーブルスキーマの宣言的 `row_policy` を、クエリ時に**省略不可・上書き不可の
//! WHERE 述語**へコンパイルして AND 強制付与する。合成点は [`crate::query`]
//! （クエリ実行チョークポイント）に一本化し、合成しない SQL を crate 外へ出さない。
//!
//! 落とし穴対応（docs/design-caveats.md）:
//! - **PIT-18**: 述語材料（ロール集合・個別共有 ID）は OpenFGA 側にある。上限
//!   （[`material::MAX_ROLE_SET`] / [`material::MAX_SHARED_IDS`]）・TTL＋世代キャッシュ・
//!   超過時 fail-closed（可視が減る方向）で `IN(数千)` の肥大とタプル爆発を防ぐ。
//!   RAG の可読集合（PIT-1）と同一方針。
//! - **PIT-21**: WHERE 注入は必要条件にすぎない。文法を閉じ（[`ast`]）、値は常に
//!   バインドし、フィールド名はスキーマ検証済み識別子のみ埋め込む。漏れ口ごとの
//!   対策テストは `tests/policy_threat_it.rs`。

pub(crate) mod ast;
pub(crate) mod compile;
pub(crate) mod material;
pub(crate) mod validate;

pub use ast::{CmpOp, PolicyExpr, PolicyOperand, RowPolicy};
