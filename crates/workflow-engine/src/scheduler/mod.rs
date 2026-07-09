//! トリガ: スケジューラ＋イベントマッチング（Task 10.3・engine.md §5）。
//!
//! - `leader`: リーダーリース（単一スケジューラループ・多重発火防止）。
//! - `cron`: cron 評価（5 フィールド＋IANA tz・watermark 前進）。
//! - `store`: occurrence 冪等記録＋trigger_firing（イベント）＋run 起動。
//!
//! run 起動は [`RunLauncher`] トレイト裏（実装は delegation チェック＋IR 取得＋create_run を束ねる）。

pub mod cron;
pub mod leader;
pub mod store;

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

pub use leader::LeaderLease;
pub use store::{SchedulerStore, SchedulerStoreError};

/// トリガ発火から run を起動する委譲先（delegation チェック＋IR 取得＋create_run）。
///
/// schedule/event の run は workflow プリンシパルで実行し、run 開始時委譲チェックを通る
/// （engine.md §6.2）。委譲不成立なら `None`（occurrence は記録済みで再発火しない）。
#[async_trait]
pub trait RunLauncher: Send + Sync {
    /// 指定ワークフローの run を起動し run_id を返す（起動しなければ `None`）。
    ///
    /// `payload` はトリガペイロード（event はイベントペイロード・schedule は `Null`）。run の入力に載せ、
    /// `$from trigger`/`$from input` で参照できるようにする（engine.md §6.1）。
    async fn launch(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_kind: &str,
        trigger_id: &str,
        payload: &Value,
    ) -> Option<Uuid>;
}
