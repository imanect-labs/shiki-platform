//! `ChatStore::post_message` — 発話投入の Transactional Outbox（Task 3.11・design §4.4.1）。
//!
//! message 保存＋run 行＋jobq enqueue を**単一 Postgres TX**で行い、202 で run_id を即返す
//! （同期実行しない）。claim/リース/fencing 等の run 操作は [`super::runs`]。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Relation};
use serde_json::json;
use sqlx::types::Json;
use uuid::Uuid;

use super::runs::{map_db, CHAT_GENERATION_QUEUE};
use crate::model::{Attachment, ContentBlock};

/// `post_message` の結果（202 で返す）。
#[derive(Debug, Clone)]
pub struct PostResult {
    pub run_id: Uuid,
    pub user_message_id: Uuid,
    pub assistant_message_id: Uuid,
    pub agent_mode: bool,
}

impl ChatStore {
    /// ユーザー発話を投入する（**単一 TX**で user/assistant message 保存＋run 行＋jobq enqueue）。
    ///
    /// editor 権限を要求する（viewer は投稿不可）。同期実行はせず run_id を返す（202・Task 3.5）。
    #[allow(clippy::too_many_arguments)] // 発話の全構成要素（呼び出し元は api 1 箇所＋UI アクション）。
    pub async fn post_message(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        text: &str,
        attachments: &[Attachment],
        // エディタの選択コンテキスト（Task 11.10）。node_id の可読性検証は api 層の責務
        // （storage.get_metadata の viewer 判定・監査つき）。ここでは上限の切り詰めのみ行う。
        selection: Option<crate::model::SelectionContext>,
        agent_mode_override: Option<bool>,
        autonomous: bool,
        trace_id: Option<&str>,
    ) -> Result<PostResult, ChatError> {
        self.require_thread(ctx, thread_id, Relation::Editor, "thread.post", trace_id)
            .await?;
        let text = text.trim();
        if text.is_empty() && attachments.is_empty() {
            return Err(ChatError::Invalid("empty message".into()));
        }

        // 実効エージェントモード（メッセージ上書き or スレッド既定）＋skill/ミニアプリのピン
        // （thread 作成時に固定した版を run へコピーする・0027 の autonomous と同パターン）。
        /// thread の生成材料（agent_mode 既定＋承認モード＋skill/mini_app のピン）。
        type ThreadDefaults = (
            bool,
            String,
            Option<Uuid>,
            Option<i64>,
            Option<Uuid>,
            Option<i64>,
        );
        let thread_row: Option<ThreadDefaults> = sqlx::query_as(
            "SELECT agent_mode, autonomous_mode, skill_id, skill_version, mini_app_id, mini_app_version \
             FROM thread WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let (
            thread_default,
            autonomous_mode,
            skill_id,
            skill_version,
            mini_app_id,
            mini_app_version,
        ) = thread_row.ok_or(ChatError::NotFound)?;
        // 自律プロファイルはエージェントモードを含意する（ツールループが前提）。
        let agent_mode = agent_mode_override.unwrap_or(thread_default) || autonomous;

        // user メッセージ content: 選択コンテキスト＋添付（file_ref）＋text。
        let mut user_content: Vec<ContentBlock> = Vec::new();
        if let Some(selection) = selection {
            user_content.push(ContentBlock::SelectionContext {
                context: selection.clamped(),
            });
        }
        user_content.extend(attachments.iter().map(|a| ContentBlock::FileRef {
            node_id: a.node_id.clone(),
            name: a.name.clone(),
        }));
        if !text.is_empty() {
            user_content.push(ContentBlock::Text {
                text: text.to_string(),
            });
        }

        let mut tx = self.db.begin().await.map_err(map_db)?;
        let user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO message (thread_id, org, tenant_id, role, content, agent_mode) \
             VALUES ($1, $2, $3, 'user', $4, $5) RETURNING id",
        )
        .bind(thread_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(Json(&user_content))
        .bind(agent_mode)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;

        let asst_id: Uuid = sqlx::query_scalar(
            "INSERT INTO message (thread_id, org, tenant_id, role, content, agent_mode, parent_id) \
             VALUES ($1, $2, $3, 'assistant', '[]'::jsonb, $4, $5) RETURNING id",
        )
        .bind(thread_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(agent_mode)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;

        // 承認モードは投入時点の thread 値をスナップショットする（発話者はモードを見て投稿する
        // ＝その水準での実行に同意・実行中の緩和は actor 本人設定のみ有効・#350）。
        let run_id: Uuid = sqlx::query_scalar(
            "INSERT INTO generation_run (message_id, thread_id, org, tenant_id, actor, agent_mode, status, trace_id, autonomous, \
                                         autonomous_mode, skill_id, skill_version, mini_app_id, mini_app_version) \
             VALUES ($1, $2, $3, $4, $5, $6, 'queued', $7, $8, $9, $10, $11, $12, $13) RETURNING run_id",
        )
        .bind(asst_id)
        .bind(thread_id)
        .bind(&ctx.org)
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .bind(agent_mode)
        .bind(trace_id)
        .bind(autonomous)
        .bind(&autonomous_mode)
        .bind(skill_id)
        .bind(skill_version)
        .bind(mini_app_id)
        .bind(mini_app_version)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;

        sqlx::query("UPDATE thread SET updated_at = now() WHERE id = $1 AND tenant_id = $2")
            .bind(thread_id)
            .bind(&ctx.tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;

        // outbox: 同一 TX で jobq へ生成ジョブを enqueue（run_id を payload）。
        jobq::enqueue_on(
            &mut tx,
            jobq::NewJob {
                queue: CHAT_GENERATION_QUEUE,
                tenant_id: &ctx.tenant_id,
                payload: &json!({ "run_id": run_id }),
                trace_id,
                max_attempts: 3,
            },
        )
        .await
        .map_err(|e| ChatError::Internal(format!("enqueue: {e}")))?;

        tx.commit().await.map_err(map_db)?;
        Ok(PostResult {
            run_id,
            user_message_id: user_id,
            assistant_message_id: asst_id,
            agent_mode,
        })
    }
}
