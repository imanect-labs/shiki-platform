//! 単発 UI アクションの実行台帳（#410）。
//!
//! 「どのメッセージのどの action が実行されたか」を一級の状態として持つ。用途は 2 つ:
//!   ① 実行前に確保して**二重送信を入口で潰す**（[`ChatStore::claim_ui_action`]）
//!   ② メッセージ取得時に同梱してカードの「送信済み」表示の根拠にする
//!      （[`ChatStore::invoked_ui_actions`]）
//!
//! 同じ事実は監査（`ui_action.invoke` の Allow）にも残るが、あれは追記専用の台帳で保持期間も
//! アクセス経路も別物なので、UI の描画根拠にはしない。

use std::collections::HashMap;

use authz::AuthContext;
use uuid::Uuid;

use super::ChatStore;
use crate::ChatError;

impl ChatStore {
    /// 単発アクションを実行前に確保する。既に実行済みなら `false`（実行してはいけない）。
    ///
    /// 競合は主キーで潰す（`on conflict do nothing`）ため、同時押し・二重 POST でも
    /// 実行に進めるのは高々 1 つ。呼び出し元（`ActionDispatcher`）は対象メッセージを
    /// thread viewer 認可つきで引いた後にここへ来る。
    pub async fn claim_ui_action(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        action_id: &str,
    ) -> Result<bool, ChatError> {
        // メッセージの実在（＋テナント一致）を insert 側で確かめる。存在しない id で
        // 台帳だけ作らせない（FK 違反ではなく 0 行として静かに落とす）。
        let inserted = sqlx::query(
            "INSERT INTO ui_action_invocation \
                 (tenant_id, thread_id, message_id, action_id, org, invoked_by) \
             SELECT $1, $2, $3, $4, $5, $6 \
             FROM message m \
             WHERE m.id = $3 AND m.thread_id = $2 AND m.tenant_id = $1 \
             ON CONFLICT DO NOTHING",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .execute(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("ui_action claim: {e}")))?;
        Ok(inserted.rows_affected() > 0)
    }

    /// 確保を解く（実行に失敗したときだけ・押し直せる状態へ戻す）。
    pub async fn release_ui_action(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        action_id: &str,
    ) -> Result<(), ChatError> {
        sqlx::query(
            "DELETE FROM ui_action_invocation \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND action_id = $4",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .execute(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("ui_action release: {e}")))?;
        Ok(())
    }

    /// 実行で生まれた run を記録へ紐づける（監査・調査との突合用）。
    pub async fn attach_ui_action_run(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        action_id: &str,
        run_id: Uuid,
    ) -> Result<(), ChatError> {
        sqlx::query(
            "UPDATE ui_action_invocation SET run_id = $5 \
             WHERE tenant_id = $1 AND thread_id = $2 AND message_id = $3 AND action_id = $4",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .bind(message_id)
        .bind(action_id)
        .bind(run_id)
        .execute(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("ui_action attach_run: {e}")))?;
        Ok(())
    }

    /// スレッド内の実行済み action を message ごとに引く（主キー先頭 2 列でそのまま効く）。
    pub(super) async fn invoked_ui_actions(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
    ) -> Result<HashMap<Uuid, Vec<String>>, ChatError> {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT message_id, action_id FROM ui_action_invocation \
             WHERE tenant_id = $1 AND thread_id = $2 \
             ORDER BY message_id, action_id",
        )
        .bind(&ctx.tenant_id)
        .bind(thread_id)
        .fetch_all(&self.db)
        .await
        .map_err(|e| ChatError::Internal(format!("ui_action list: {e}")))?;
        let mut by_message: HashMap<Uuid, Vec<String>> = HashMap::new();
        for (message_id, action_id) in rows {
            by_message.entry(message_id).or_default().push(action_id);
        }
        Ok(by_message)
    }
}
