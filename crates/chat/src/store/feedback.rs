//! 回答への評価（👍 / 👎・#751）。
//!
//! FR-16 の既定どおり**メタデータだけ**を持つ（評価・👎 の理由カテゴリ・モデル）。会話本文や
//! 自由記述はスキーマ上置けない（`message_feedback` に列が無い）。本文添付・自由記述つきの
//! 共有は二段同意が要るので別タスク（Task 12.6）。
//!
//! 評価できるのはスレッドを閲覧できる人だけ（viewer）。共有されたスレッドで閲覧者が付けた
//! 評価も、その人自身の行として残る（1 人 1 回答 1 行）。

use authz::{AuthContext, Relation};
use uuid::Uuid;

use crate::error::ChatError;

use super::threads::map_db;
use super::ChatStore;

/// 👎 の理由（`message_feedback.category` の CHECK と同じ集合）。
pub const FEEDBACK_CATEGORIES: &[&str] = &[
    "inaccurate",
    "not_found",
    "not_followed",
    "slow",
    "how_to",
    "other",
];

/// 回答への評価（自分の分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageFeedback {
    pub message_id: Uuid,
    /// 1 = 👍、-1 = 👎。
    pub rating: i16,
    pub category: Option<String>,
}

impl ChatStore {
    /// 回答に評価を付ける（付け直しは上書き）。
    ///
    /// - `rating` は 1 か -1。👍 に理由は付けられない（付けたら 400）。
    /// - 対象はこのスレッドの assistant メッセージだけ（ユーザー発話や別スレッドの id は 404）。
    pub async fn set_feedback(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        rating: i16,
        category: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        if rating != 1 && rating != -1 {
            return Err(ChatError::Invalid("rating は 1 か -1 です".into()));
        }
        if let Some(c) = category {
            if rating == 1 {
                return Err(ChatError::Invalid("👍 に理由は付けられません".into()));
            }
            if !FEEDBACK_CATEGORIES.contains(&c) {
                return Err(ChatError::Invalid(format!("不明な理由です: {c}")));
            }
        }
        self.require_thread(
            ctx,
            thread_id,
            Relation::Viewer,
            "thread.feedback",
            trace_id,
        )
        .await?;
        // 対象の回答（このスレッド・このテナントの assistant）と、それを作った run のモデル。
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT m.org, \
                    (SELECT r.model FROM generation_run r \
                      WHERE r.message_id = m.id ORDER BY r.created_at DESC LIMIT 1) \
             FROM message m \
             WHERE m.id = $1 AND m.thread_id = $2 AND m.tenant_id = $3 AND m.role = 'assistant'",
        )
        .bind(message_id)
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let (org, model) = row.ok_or(ChatError::NotFound)?;
        sqlx::query(
            "INSERT INTO message_feedback \
             (tenant_id, org, message_id, thread_id, user_id, rating, category, model) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (tenant_id, message_id, user_id) DO UPDATE SET \
               rating = EXCLUDED.rating, category = EXCLUDED.category, \
               model = EXCLUDED.model, updated_at = now()",
        )
        .bind(&ctx.tenant_id)
        .bind(&org)
        .bind(message_id)
        .bind(thread_id)
        .bind(&ctx.principal.id)
        .bind(rating)
        .bind(category)
        .bind(model)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// 自分の評価を取り消す（無ければ何もしない）。
    pub async fn clear_feedback(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Viewer,
            "thread.feedback",
            trace_id,
        )
        .await?;
        sqlx::query(
            "DELETE FROM message_feedback \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND user_id = $4",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(&ctx.principal.id)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// このスレッドで自分が付けた評価（再訪時にボタンの状態を戻す）。
    pub async fn my_feedback(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<MessageFeedback>, ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Viewer,
            "thread.feedback",
            trace_id,
        )
        .await?;
        let rows: Vec<(Uuid, i16, Option<String>)> = sqlx::query_as(
            "SELECT message_id, rating, category FROM message_feedback \
             WHERE tenant_id = $1 AND thread_id = $2 AND user_id = $3",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(&ctx.principal.id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|(message_id, rating, category)| MessageFeedback {
                message_id,
                rating,
                category,
            })
            .collect())
    }
}
