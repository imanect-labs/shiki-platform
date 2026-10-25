//! shiki-gui — skill・ミニアプリと、図のボタンの実行系（Phase 6・#604）。
//!
//! 設計の正本: docs/design.md §4.7、docs/miniapp-platform.md、docs/roadmap/phase-6.md。
//!
//! - **skill・ミニアプリの本文**（[`skill`] / [`miniapp`]）: 保存・解決の前段で必ず検証し、
//!   違反は拒否する（暗黙補正なし）。ミニアプリの画面は Yui の ```ui を含む Markdown。
//! - **宣言的バックエンド束縛**（[`action`] / [`dispatch`]）: 図のボタンからの操作はサーバーが
//!   引いた束縛経由のみ・実行ユーザー権限で認可（アンビエント権限なし・confused-deputy 防御）。
//! - **ワークフロー名の解決**（[`resolver`]）: ボタンの `-> workflow 名前` を版に固定する。
//! - **表のデータ束縛**（[`data_grid`]・Task 15.7）: 画面の ```data_grid は束ねた保存ビューの
//!   ピン版を、閲覧者本人の権限で毎回評価して描く（[`ActionBinding::DataView`]）。

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
pub mod data_grid;
pub mod dispatch;
pub mod miniapp;
pub mod miniapp_actions;
mod miniapp_data;
pub mod miniapp_store;
pub mod resolver;
pub mod skill;
pub mod skill_command;
pub mod skill_store;
pub mod store;
pub mod validate;
pub mod vocab;

pub use action::{ActionBinding, DataViewBinding, HandlerBinding, WorkflowBinding, WorkflowPin};
pub use data_grid::{data_grid_action_id, data_grid_alias, data_grid_blocks, DataGridBlock};
pub use dispatch::{
    ActionDispatcher, ActionError, ActionHandler, ActionLedger, ActionSource, WorkflowStarter,
};
pub use miniapp::{ComponentPin, MiniAppBody, NamedComponentPin};
pub use miniapp_actions::{MiniAppActionRequest, MiniAppRun, MiniAppRunStatus, MiniAppRunValue};
pub use miniapp_store::{MiniAppStore, ResolvedMiniApp};
pub use resolver::WorkflowResolver;
pub use skill::{
    validate_skill_body, FewShotExample, KnowledgeScope, ModelDefaults, ScriptKind, SkillBody,
    SkillScript,
};
pub use skill_command::{CommandPhase, SkillCommand, SkillCommandVariant};
pub use skill_store::SkillStore;
pub use store::GuiError;
pub use validate::GuiValidationError;
pub use vocab::HandlerKind;
