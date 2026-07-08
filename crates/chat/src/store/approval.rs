//! 承認ゲートの永続化（Task 5.6/5.10）。
//!
//! 破壊系ツールの承認要求は agent-core が SSE で出す。ここでは **run を waiting_approval にし**、
//! **承認/却下を `run_approval` に記録**し（誰が・いつ・どのツール呼び出しを）、待機中ワーカーが
//! 決定を**ポーリングで拾う**ための最小 API を提供する。決定は監査（`agent.approval.decision`）へ流す。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Relation};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use uuid::Uuid;

use crate::model::RunStatus;
use crate::ChatError;

/// sqlx エラー → ChatError（他 store ファイルと同型の局所ヘルパ）。
fn map_db(e: &sqlx::Error) -> ChatError {
    ChatError::Internal(format!("db: {e}"))
}

impl ChatStore {
    /// ユーザーの承認/却下を記録する（API 経由・thread editor 権限）。
    ///
    /// `(run_id, tool_call_id)` で 1 決定に潰す（先勝ち・二重承認を拒否）。判定は監査へ記録する。
    /// 戻り `true` = 今回の決定が採用された、`false` = 既に決定済み（competing）。
    #[allow(clippy::too_many_arguments)] // ctx＋thread/run/tool_call/tool_name/approved/trace は本質的。
    pub async fn submit_approval(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        run_id: Uuid,
        tool_call_id: &str,
        tool_name: &str,
        approved: bool,
        trace_id: Option<&str>,
    ) -> Result<bool, ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Editor,
            "agent.approval.decide",
            trace_id,
        )
        .await?;
        // 破壊系ツールは run の **actor 権限**で実行される。承認者 ≠ actor だと、共有 thread の
        // 別編集者が他人の権限での破壊操作を承認できてしまう（confused-deputy）。
        // よって**承認は run の actor 本人に限定**する（起案者が自分の権限使用を確認する）。
        let actor: Option<String> = sqlx::query_scalar(
            "SELECT actor FROM generation_run WHERE run_id = $1 AND thread_id = $2 AND tenant_id = $3",
        )
        .bind(run_id)
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        match actor {
            None => return Err(ChatError::NotFound),
            Some(a) if a != ctx.principal.id => return Err(ChatError::Forbidden),
            Some(_) => {}
        }
        let decision = if approved { "approved" } else { "rejected" };
        let inserted = sqlx::query(
            "INSERT INTO run_approval \
                 (run_id, org, tenant_id, tool_call_id, tool_name, decision, decided_by) \
             SELECT $1, org, tenant_id, $3, $4, $5, $6 FROM generation_run \
             WHERE run_id = $1 AND thread_id = $2 AND tenant_id = $7 \
             ON CONFLICT (run_id, tool_call_id) DO NOTHING",
        )
        .bind(run_id)
        .bind(thread_id)
        .bind(tool_call_id)
        .bind(tool_name)
        .bind(decision)
        .bind(&ctx.principal.id)
        .bind(&ctx.tenant_id)
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        // 承認/却下は誰が・何を許可したかを必ず監査へ（NFR-6・trace_id 相関）。
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "agent.approval.decision",
                    object_type: "generation_run",
                    object_id: &run_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({
                        "tool_call_id": tool_call_id,
                        "tool_name": tool_name,
                        "decision": decision,
                        "accepted": inserted.rows_affected() == 1,
                    }),
                },
            )
            .await
            .map_err(|e| ChatError::Internal(format!("approval audit: {e}")))?;
        Ok(inserted.rows_affected() == 1)
    }

    /// あるツール呼び出しの決定を引く（待機中ワーカーのポーリング用・`Some(approved)`）。
    pub async fn poll_approval(
        &self,
        run_id: Uuid,
        tool_call_id: &str,
    ) -> Result<Option<bool>, ChatError> {
        let d: Option<String> = sqlx::query_scalar(
            "SELECT decision FROM run_approval WHERE run_id = $1 AND tool_call_id = $2",
        )
        .bind(run_id)
        .bind(tool_call_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(d.map(|s| s == "approved"))
    }

    /// run の status を fencing 一致時のみ更新する（waiting_approval ⇄ running）。
    /// 戻り `true` = 更新された、`false` = fencing 不一致（ゾンビ）で no-op。
    pub async fn set_run_status_fenced(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        status: RunStatus,
    ) -> Result<bool, ChatError> {
        let updated = sqlx::query(
            "UPDATE generation_run SET status = $3, updated_at = now() \
             WHERE run_id = $1 AND fencing_token = $2",
        )
        .bind(run_id)
        .bind(fencing_token)
        .bind(status.as_str())
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(updated.rows_affected() == 1)
    }

    /// thread のワークスペースフォルダ id を引く（未設定なら `None`）。
    pub async fn workspace_folder_id(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
    ) -> Result<Option<Uuid>, ChatError> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT workspace_folder_id FROM thread WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))?
        .flatten();
        Ok(id)
    }

    /// thread のワークスペースフォルダ id を **未設定時のみ** 設定する（並行 run の二重設定を防ぐ）。
    /// 戻り値は確定した id（自分の設定 or 既に他が設定した値）。
    pub async fn set_workspace_folder_if_absent(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
        folder_id: Uuid,
    ) -> Result<Uuid, ChatError> {
        let current: Option<Uuid> = sqlx::query_scalar(
            "UPDATE thread SET workspace_folder_id = $3 \
             WHERE id = $1 AND tenant_id = $2 AND workspace_folder_id IS NULL \
             RETURNING workspace_folder_id",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .bind(folder_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))?
        .flatten();
        match current {
            Some(id) => Ok(id),
            // 競合で既に設定済み → 既存値を読み直す。
            None => Ok(self
                .workspace_folder_id(thread_id, tenant_id)
                .await?
                .unwrap_or(folder_id)),
        }
    }

    /// キャンセル要求が立っているか（承認待ちワーカーが待機を打ち切る判定）。
    pub async fn is_cancel_requested(&self, run_id: Uuid) -> Result<bool, ChatError> {
        let c: Option<bool> =
            sqlx::query_scalar("SELECT cancel_requested FROM generation_run WHERE run_id = $1")
                .bind(run_id)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| map_db(&e))?;
        Ok(c.unwrap_or(false))
    }
}
