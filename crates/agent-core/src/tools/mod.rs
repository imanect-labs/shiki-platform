//! 具体ツール群（doc_search＝安全な読み取り系 / code_interpreter＝隔離 Python 実行 /
//! fs_*・grep＝ワークスペース CRUD / shell＝任意コマンド実行）。

mod artifacts;
mod attachments;
pub mod code_interpreter;
pub mod doc_search;
pub mod fs;
pub mod fs_append;
pub mod fs_write;
mod mime;
mod sandbox_exec;
pub mod shell;
pub mod subagent;
/// 委譲の入力解釈（ロール・必須項目）。
mod subagent_input;
/// サブエージェントの system プロンプト（subagent.rs から分割）。
mod subagent_prompts;
mod subagent_sink;
pub mod web_fetch;
pub mod web_search;

pub use code_interpreter::CodeInterpreterTool;
pub use doc_search::{run_doc_search, DocSearchResult, DocSearchTool};
pub use fs::{FsListTool, FsReadTool, GrepTool};
pub use fs_append::FsAppendTool;
pub use fs_write::{FsDeleteTool, FsEditTool, FsWriteTool};
pub use shell::ShellTool;
pub use subagent::{SubagentLimits, SubagentTool};
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
