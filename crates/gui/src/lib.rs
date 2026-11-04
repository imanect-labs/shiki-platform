//! shiki-gui — skill・ミニアプリと、図のボタンの実行系（Phase 6・#604）。
//!
//! 設計の正本: docs/design.md §4.7、docs/miniapp-platform.md、docs/roadmap/phase-6.md。
//!
//! - **skill・ミニアプリの本文**（[`skill`] / [`miniapp`]）: 保存・解決の前段で必ず検証し、
//!   違反は拒否する（暗黙補正なし）。ミニアプリの画面は Yui の ```ui を含む Markdown。
//! - **宣言的バックエンド束縛**（[`action`] / [`dispatch`]）: 図のボタンからの操作はサーバーが
//!   引いた束縛経由のみ・実行ユーザー権限で認可（アンビエント権限なし・confused-deputy 防御）。
//! - **下書きと公開**（[`miniapp_publish`]・D-16）: ミニアプリの保存は下書きの版になり、使う人には公開した版だけを
//!   出す。公開は editor が版を選んで行う。
//! - **ワークフロー名の解決**（[`resolver`]）: ボタンの `-> workflow 名前` を版に固定する。
//! - **使う人の権限の確かめ**（[`access`]・GAP-37）: アプリの部品とワークフローが要る権限を、共有の相手ごと
//!   （作る人）と自分（使う人）について前もって確かめる。判定は OpenFGA の BatchCheck だけで、実際の読み書きの認可は変えない。
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

pub mod access;
mod access_parts;
pub mod action;
pub mod data_grid;
pub mod dispatch;
pub mod miniapp;
pub mod miniapp_actions;
mod miniapp_data;
pub mod miniapp_publish;
pub mod miniapp_runs;
pub mod miniapp_store;
pub mod resolver;
pub mod skill;
pub mod skill_command;
pub mod skill_store;
pub mod store;
pub mod validate;
pub mod vocab;

pub use access::{
    AccessChecker, AccessKind, AccessNeedView, AccessObject, AccessPart, AccessReport,
    AccessStatus, AccessTarget, MyAccessItem, MyAppAccess, MyPartAccess, PartNeeds, SubjectAccess,
    MAX_ACCESS_CHECKS, MAX_ACCESS_OBJECTS, MAX_ACCESS_SUBJECTS,
};
pub use access_parts::workflow_needs;
pub use action::{ActionBinding, DataViewBinding, HandlerBinding, WorkflowBinding, WorkflowPin};
pub use data_grid::{data_grid_action_id, data_grid_alias, data_grid_blocks, DataGridBlock};
pub use dispatch::{
    ActionDispatcher, ActionError, ActionHandler, ActionLedger, ActionSource, WorkflowStarter,
};
pub use miniapp::{screen_problems, ComponentPin, MiniAppBody, NamedComponentPin};
pub use miniapp_actions::{MiniAppActionRequest, MiniAppRun, MiniAppRunStatus, MiniAppRunValue};
pub use miniapp_publish::{
    diff_bodies, problems_block_publish, MiniAppChange, PinChange, PublishPreview,
};
pub use miniapp_runs::displayable_reply;
pub use miniapp_store::{MiniAppStore, ResolvedMiniApp, ResolvedSkill, SaveMode, SavedMiniApp};
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
