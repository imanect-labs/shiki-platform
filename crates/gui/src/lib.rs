//! shiki-gui — generative UI の信頼境界（Phase 6 Task 6.2〜6.5）。
//!
//! 設計の正本: docs/design.md §4.7、docs/miniapp-platform.md、docs/roadmap/phase-6.md。
//!
//! - **信頼コンポーネント・カタログ**（[`vocab`]）と **UI スペックの型付きツリー**（[`spec`]）:
//!   カタログ外・生 HTML・任意コードはスキーマ上表現不可能（Rust 型が単一ソース・ts-rs で TS 生成）。
//! - **検証層**（[`validate`] / [`validator`]）: 保存・発話・解決の全経路の前段で必ず検証し、
//!   違反は拒否（部分描画・暗黙補正なし）。拒否は監査に残す。
//! - **宣言的バックエンド束縛**（[`action`] / [`dispatch`]）: UI からの操作は宣言済みアクション
//!   経由のみ・実行ユーザー権限で認可（アンビエント権限なし・confused-deputy 防御）。
//! - **emit_ui ツール**（[`emit_tool`]）: LLM が UI を出す唯一の手段。検証失敗はテキストへ
//!   フォールバックし、未検証スペックは永続化されない。

// #[cfg(test)] は本番のみ厳格化する lint を許容する。
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::pedantic
    )
)]

pub mod action;
pub mod chart;
pub mod dispatch;
pub mod emit_tool;
pub mod form_fields;
pub mod layout;
pub mod miniapp;
pub mod miniapp_store;
pub mod question;
pub mod skill;
pub mod skill_store;
pub mod spec;
pub mod store;
pub mod validate;
pub mod validator;
pub mod vocab;

pub use action::{ActionBinding, HandlerBinding, ToolBinding, WorkflowBinding, WorkflowPin};
pub use chart::{ChartPoint, ChartSpec};
pub use dispatch::{ActionDispatcher, ActionError, ActionHandler, ActionSource, WorkflowStarter};
pub use emit_tool::EmitUiTool;
pub use miniapp::{ComponentPin, MiniAppBody, NamedComponentPin};
pub use miniapp_store::{MiniAppStore, ResolvedMiniApp};
pub use skill::{
    validate_skill_body, FewShotExample, KnowledgeScope, ModelDefaults, ScriptKind, SkillBody,
    SkillScript,
};
pub use skill_store::SkillStore;
pub use spec::{ActionRef, UiNode, UiSpecDoc};
pub use store::{GuiError, UiSpecStore};
pub use validate::{validate_spec, GuiValidationError};
pub use validator::{ResolvedSpec, SpecValidator};
pub use vocab::{ChartKind, ComponentKind, HandlerKind};
