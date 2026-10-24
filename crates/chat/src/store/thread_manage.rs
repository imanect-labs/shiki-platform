//! `ChatStore`: サイドバー履歴の行操作（名前の変更・ピン留め・削除）と自動タイトル。
//!
//! - 改名・ピン留め・削除は owner のみ（自分の会話の整理）。どれも `updated_at` を動かさない
//!   （履歴の並び＝最後に話した順を、整理の操作で崩さない）。
//! - 自動タイトルは生成ワーカーが付ける（[`crate::worker`] の title）。ユーザーの改名が常に勝つよう、
//!   書き込みは `title_pending` の条件付き更新にする。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Relation};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use uuid::Uuid;

use super::threads::{map_db, map_storage, ThreadRow, THREAD_COLUMNS};
use crate::model::Thread;

/// ユーザーが付けるタイトルの上限（文字）。サイドバーでは 1 行に切り詰めて出す。
pub const MAX_THREAD_TITLE_CHARS: usize = 200;

/// サイドバーのピン留めの上限件数（一覧の先頭に全件出すため）。
const MAX_PINNED: i64 = 50;

/// 自動タイトルの材料にする最初のユーザー発話の件数・文字数。
const TITLE_SEED_MESSAGES: i64 = 3;
const TITLE_SEED_CHARS: usize = 1500;

impl ChatStore {
    /// スレッドの名前を変える（owner のみ）。以後は自動タイトルで上書きしない。
    pub async fn rename_thread(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        title: &str,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        let title = normalize_user_title(title)?;
        self.require_thread(ctx, thread_id, Relation::Owner, "thread.rename", trace_id)
            .await?;
        let updated = sqlx::query(
            "UPDATE thread SET title = $3, title_pending = false \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .bind(&title)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(ChatError::NotFound);
        }
        self.record_allow(ctx, "thread.rename", thread_id, trace_id, json!({}))
            .await
    }

    /// ピン留めを付け外しする（owner のみ）。
    pub async fn set_thread_pinned(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        pinned: bool,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(ctx, thread_id, Relation::Owner, "thread.pin", trace_id)
            .await?;
        // 付け直しで時刻を更新しない（既にピン留め済みなら元の時刻のまま＝並びが跳ねない）。
        let updated = sqlx::query(
            "UPDATE thread SET pinned_at = CASE WHEN $3 THEN coalesce(pinned_at, now()) END \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .bind(pinned)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(ChatError::NotFound);
        }
        self.record_allow(
            ctx,
            "thread.pin",
            thread_id,
            trace_id,
            json!({ "pinned": pinned }),
        )
        .await
    }

    /// スレッドを削除する（owner のみ・論理削除）。
    ///
    /// 削除後は一覧から消え、共有相手を含めて誰も開けない（[`Self::require_thread`] が削除済みを
    /// NotFound にする）。生成中・順番待ちの run は停止を要求する（ワーカーがキャンセルで確定する）。
    pub async fn delete_thread(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(ctx, thread_id, Relation::Owner, "thread.delete", trace_id)
            .await?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let updated = sqlx::query(
            "UPDATE thread SET deleted_at = now(), pinned_at = NULL \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            tx.rollback().await.map_err(map_db)?;
            return Err(ChatError::NotFound);
        }
        sqlx::query(
            "UPDATE generation_run SET cancel_requested = true, updated_at = now() \
             WHERE thread_id = $1 AND tenant_id = $2 \
               AND status IN ('queued', 'running', 'waiting_approval')",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        self.record_allow(ctx, "thread.delete", thread_id, trace_id, json!({}))
            .await
    }

    /// 自分のピン留めしたスレッド（ピン留めの新しい順・上限 [`MAX_PINNED`] 件）。
    pub async fn list_pinned_threads(&self, ctx: &AuthContext) -> Result<Vec<Thread>, ChatError> {
        let rows: Vec<ThreadRow> = sqlx::query_as(&format!(
            "SELECT {THREAD_COLUMNS} FROM thread \
             WHERE tenant_id = $1 AND org = $2 AND owner = $3 \
               AND deleted_at IS NULL AND pinned_at IS NOT NULL \
             ORDER BY pinned_at DESC, id DESC LIMIT $4",
        ))
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(MAX_PINNED)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(ThreadRow::into_thread).collect())
    }

    /// 自動タイトルの材料（タイトルが仮のスレッドの、最初のユーザー発話の本文）。
    ///
    /// 仮でない（生成済み・改名済み）、削除済み、または文字の発話がまだ無いなら `None`。
    /// 生成ワーカー内部用（run の発話ユーザーで claim 済みの thread に対して呼ぶ）。
    pub async fn title_seed(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
    ) -> Result<Option<String>, ChatError> {
        let rows: Vec<(sqlx::types::Json<Vec<crate::model::ContentBlock>>,)> = sqlx::query_as(
            "SELECT m.content FROM message m \
             JOIN thread t ON t.id = m.thread_id AND t.tenant_id = m.tenant_id \
             WHERE m.thread_id = $1 AND m.tenant_id = $2 AND m.role = 'user' \
               AND t.title_pending AND t.deleted_at IS NULL \
             ORDER BY m.created_at, m.id LIMIT $3",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .bind(TITLE_SEED_MESSAGES)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut seed = String::new();
        for (content,) in rows {
            for block in content.0 {
                if let crate::model::ContentBlock::Text { text } = block {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    if !seed.is_empty() {
                        seed.push_str("\n\n");
                    }
                    seed.push_str(text);
                }
            }
        }
        let seed: String = seed.chars().take(TITLE_SEED_CHARS).collect();
        Ok((!seed.trim().is_empty()).then_some(seed))
    }

    /// 生成したタイトルを書く（タイトルが仮のままのときだけ）。書けたら true。
    ///
    /// 生成中にユーザーが改名していれば `title_pending = false` なので何もしない（改名が勝つ）。
    pub async fn set_generated_title(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
        title: &str,
    ) -> Result<bool, ChatError> {
        let updated = sqlx::query(
            "UPDATE thread SET title = $3, title_pending = false \
             WHERE id = $1 AND tenant_id = $2 AND title_pending AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .bind(title)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(updated.rows_affected() > 0)
    }

    async fn record_allow(
        &self,
        ctx: &AuthContext,
        action: &str,
        thread_id: Uuid,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), ChatError> {
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "thread",
                    object_id: &thread_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata,
                },
            )
            .await
            .map_err(map_storage)
    }
}

/// ユーザーが付けるタイトルを整える（改行・連続空白は 1 つの空白に。空・長すぎは拒否）。
fn normalize_user_title(title: &str) -> Result<String, ChatError> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err(ChatError::Invalid("タイトルを入力してください".into()));
    }
    if title.chars().count() > MAX_THREAD_TITLE_CHARS {
        return Err(ChatError::Invalid(format!(
            "タイトルは {MAX_THREAD_TITLE_CHARS} 文字以内にしてください"
        )));
    }
    Ok(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_title_collapses_whitespace() {
        assert_eq!(
            normalize_user_title("  週次\n 報告の\t下書き ").unwrap(),
            "週次 報告の 下書き"
        );
    }

    #[test]
    fn user_title_rejects_empty_and_too_long() {
        assert!(matches!(
            normalize_user_title(" \n "),
            Err(ChatError::Invalid(_))
        ));
        let long = "あ".repeat(MAX_THREAD_TITLE_CHARS + 1);
        assert!(matches!(
            normalize_user_title(&long),
            Err(ChatError::Invalid(_))
        ));
        let max = "あ".repeat(MAX_THREAD_TITLE_CHARS);
        assert_eq!(normalize_user_title(&max).unwrap(), max);
    }
}
