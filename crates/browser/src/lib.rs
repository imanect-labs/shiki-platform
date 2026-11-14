//! エージェントブラウザ（docs/agent/browser.md）。
//!
//! - [`engine`] — エンジンのトレイト境界（[`BrowserEngine`] / [`EnginePage`]）。
//! - [`session`] — run 単位のセッション（cookie jar・ページ・開いてよい URL の出所）。
//! - [`cookie_jar`] — RFC 6265bis の属性を落とさない jar（SameSite は送信時に強制・PIT-80）。
//! - [`snapshot`] — エンジンによらない共通のスナップショット形式（ref は `t3.12`）。
//! - [`text`] — JS を実行しないテキストエンジン（案 D）。
//!
//! Chromium エンジン（AB.4）とエンジン切替（AB.5）、ツール（AB.3・AB.6）は後続。

pub mod cookie_jar;
pub mod engine;
pub mod session;
pub mod snapshot;
pub mod text;

pub use cookie_jar::CookieJar;
pub use engine::{
    Action, BrowserEngine, BrowserError, EnginePage, LoadRequest, Method, PlannedRequest,
    RedirectCheck, SessionOpts,
};
pub use session::{BrowserSession, Provenance};
pub use snapshot::{ElementInfo, ElementKind, EngineKind, NeedsJs, RefId, Snapshot};
pub use text::{TextEngine, TextLimits, TextPage};
