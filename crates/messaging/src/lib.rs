//! 職員間メッセージ（Phase 14・design §4.14・requirements FR-18）のドメイン。
//!
//! チャンネル（公開／非公開）と 1 対 1 の DM（`kind = Dm` のチャンネル）・参加者・発言（スレッド返信）・
//! リアクション・既読位置・メンション・パーソナルエージェント・管理者閲覧・保存期間を持つ。
//! 「メッセージ」は人と人、「チャット」（§4.4・`crates/chat`）は人と AI を指す。
//!
//! # 不変条件
//!
//! - **認可の正本は OpenFGA の `channel` 型**。DB の `channel_member` は表示・未読・参加前発言の境界
//!   （`visible_from`）のための写しで、読める／投稿できるの判定には使わない。
//! - **テナント文脈の無い DB 経路を持たない。** SQL は `store*` に閉じ、全関数が
//!   [`storage::db_tenant::TenantTx`] の接続と [`authz::AuthContext`] を受け取る。
//! - **参加前の発言は読めない**（非公開・DM）。境界は [`store_message::fetch_messages`] の 1 か所で掛け、
//!   一覧・スレッド・配信・検索・AI の文脈が同じ関数を通る（PIT-63）。
//! - **管理者の閲覧は通常の read 経路に相乗りしない**（[`access`]・PIT-65）。
//! - **配信のトピック名は [`pubsub::topic`] だけが組み、必ず `tenant_id` を含む**（PIT-66）。

pub mod access;
pub mod agent;
pub mod agent_profile;
mod approval_watch;
mod error;
pub mod files;
mod model;
pub mod preview;
pub mod pubsub;
pub mod retention;
mod service;
mod service_member;
mod service_message;
mod service_ui;
pub mod store;
pub mod store_message;
pub mod stream;

pub use access::{
    AccessGrant, AccessRequest, AdminChannel, AdminMessage, AdminRevision, MessagingSettings,
};
pub use agent::{
    AgentAnswer, AgentReplyView, AgentRequest, AgentRuntime, ContextLine, PendingApprovalRef,
    PublishWarning, ReplyMode,
};
pub use agent_profile::{AgentProfileUpdate, AgentProfileView};
pub use error::Error;
pub use model::*;
pub use service::MessagingService;
pub use service_message::is_emoji;
