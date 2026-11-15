//! 職員間メッセージ（Phase 14・design §4.14・requirements FR-18）のドメイン。
//!
//! チャンネル（公開／非公開）と DM（`kind = Dm` のチャンネル）・参加者・発言（スレッド返信）・
//! リアクション・既読位置を持つ。HTTP・SSE 配信・全文検索・対話呼出しは持たない（Task 14.3 以降）。
//!
//! # 不変条件
//!
//! - **認可の正本は OpenFGA の `channel` 型**（Task 14.2）。DB の `channel_member` は表示・未読・
//!   参加前発言の境界（`visible_from`）のための写しで、読める／投稿できるの判定には使わない。
//! - **テナント文脈の無い DB 経路を持たない。** SQL は [`store`] に閉じ、全関数が
//!   [`storage::db_tenant::TenantTx`]（検証済みテナントを持つトランザクション）と org を受け取る。
//!   `tenant_id` は呼び出し側から渡せず、トランザクションのテナントだけを使う。
//! - **参加前の発言は読めない**（非公開・DM）。境界は [`store`] の読取関数 1 か所で掛け、
//!   一覧・スレッド・（後続の）検索・対話文脈が同じ関数を通る（PIT-63）。

mod error;
mod model;
mod service;
mod service_message;
pub mod store;

pub use error::Error;
pub use model::*;
pub use service::MessagingService;
