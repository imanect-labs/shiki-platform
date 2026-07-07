//! `ChatStore` — チャットの単一チョークポイント（`&AuthContext` 経由・PgPool 内包）。
//!
//! - [`threads`]: thread/message CRUD・線形取得・ReBAC 共有。
//! - [`runs`]: 接続非依存生成（Task 3.11）の generation_run / generation_event 操作
//!   （claim/lease/fencing/append/replay/cancel/sweep）と SSE 用イベントストリーム。
//!
//! 生成の真実のソースは append-only な `generation_event(run_id, seq)`。Redis pub/sub は
//! **best-effort の起床通知**で、正しさは常に DB replay が担保する（取りこぼしても DB で補填）。

mod runs;
mod stream;
mod threads;

pub use runs::{ClaimedRun, PostResult, CHAT_GENERATION_QUEUE};

use std::sync::Arc;

use authz::AuthzClient;
use durable::RedisPubSub;
use sqlx::PgPool;
use storage::audit::AuditRecorder;

use crate::ChatError;

/// チャットのデータチョークポイント。全公開メソッドは第一引数に `&AuthContext` を取り、
/// 内部で OpenFGA authz と監査を行う（アンビエント権限の排除）。
///
/// Clone は安価（PgPool / Arc / ConnectionManager は共有ハンドル）。SSE ストリームの
/// バックグラウンドタスクへクローンを渡すために `Clone`。
#[derive(Clone)]
pub struct ChatStore {
    db: PgPool,
    authz: Arc<dyn AuthzClient>,
    audit: AuditRecorder,
    redis: Option<RedisPubSub>,
}

impl ChatStore {
    /// ストアを構築する。`redis_url` があれば pub/sub を有効化する（無ければ DB replay のみで動作）。
    pub async fn connect(
        db: PgPool,
        authz: Arc<dyn AuthzClient>,
        redis_url: Option<&str>,
    ) -> Result<Self, ChatError> {
        let audit = AuditRecorder::new(db.clone());
        let redis = match redis_url {
            Some(url) => Some(
                RedisPubSub::connect(url)
                    .await
                    .map_err(|e| ChatError::Internal(format!("redis connect: {e}")))?,
            ),
            None => None,
        };
        Ok(ChatStore {
            db,
            authz,
            audit,
            redis,
        })
    }

    /// 生成イベントを Redis へ publish する（best-effort・失敗は警告のみ）。
    async fn publish_event(&self, run_id: uuid::Uuid, payload: &str) {
        let Some(r) = &self.redis else {
            return;
        };
        r.publish_best_effort(&run_channel(run_id), payload).await;
    }
}

/// run ごとの Redis pub/sub チャネル名。
fn run_channel(run_id: uuid::Uuid) -> String {
    format!("chat:run:{run_id}")
}
