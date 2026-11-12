//! 公開製品ヘルプ専用の不変スナップショット。tenant の索引・本文ストアを参照しない。
mod corpus;
mod index;
mod search;
pub use corpus::{HelpChunk, HelpConfig, HelpManifest, VectorManifest};
pub use search::{HelpHit, HelpSearchService};
#[cfg(test)]
mod tests;
