//! 具体ツール群（Phase 3 は doc_search のみ・安全な読み取り系）。

pub mod doc_search;

pub use doc_search::{run_doc_search, DocSearchResult, DocSearchTool};
