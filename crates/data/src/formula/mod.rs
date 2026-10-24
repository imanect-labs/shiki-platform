//! 数式（閉じた AST ＋ Rust インタプリタ・`numeric` 精度・data-platform §7.3・Task 15.4）。
//!
//! - [`ast`]: 式の型（serde で保存・utoipa で OpenAPI へ）と構造の検証（深さ・ノード数）
//! - [`decimal`]: 正確な 10 進演算（金額に浮動小数を使わない）
//! - [`eval`]: インタプリタ（null 伝播・型不一致は null）
//!
//! 参照の解決・循環検出・材料化可否は [`crate::derived::graph`]、行への適用は [`crate::derived`]。

pub(crate) mod ast;
pub(crate) mod decimal;
pub(crate) mod eval;
mod schema;

pub use ast::{FormulaCmpOp, FormulaExpr, SwitchCase, MAX_FORMULA_DEPTH};
