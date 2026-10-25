//! 承認ゲートの永続化（Task 5.6/5.10）。
//!
//! 破壊系ツールの承認要求は agent-core が SSE で出す。ここでは **run を waiting_approval にし**、
//! **承認/却下を `run_approval` に記録**し（誰が・いつ・どのツール呼び出しを）、待機中ワーカーが
//! 決定を**ポーリングで拾う**ための最小 API を提供する。決定は監査（`agent.approval.decision`）へ流す。

#[allow(clippy::wildcard_imports)]
use super::*;
use storage::audit::AuditAction;

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
        claimed_tool_name: &str,
        approved: bool,
        trace_id: Option<&str>,
    ) -> Result<bool, ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Editor,
            AuditAction::AgentApprovalDecide,
            trace_id,
        )
        .await?;
        // 破壊系ツールは run の **actor 権限**で実行される。承認者 ≠ actor だと、共有 thread の
        // 別編集者が他人の権限での破壊操作を承認できてしまう（confused-deputy）。
        // よって**承認は run の actor 本人に限定**する（起案者が自分の権限使用を確認する）。
        let row: Option<(String, Option<sqlx::types::Json<serde_json::Value>>)> = sqlx::query_as(
            "SELECT actor, pending_approval FROM generation_run \
             WHERE run_id = $1 AND thread_id = $2 AND tenant_id = $3",
        )
        .bind(run_id)
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        let pending = match row {
            None => return Err(ChatError::NotFound),
            Some((a, _)) if a != ctx.principal.id => return Err(ChatError::Forbidden),
            Some((_, p)) => p.map(|j| j.0),
        };
        // 承認待ちのツール呼び出し（承認要求を出す前に run へ永続化済み）と照合する。承認待ちでない run・
        // 別の呼び出しの id への決定は記録しない（記録しても誰も拾わず、run は進まないまま残る・検証 C-P1-3）。
        // 監査・台帳のツール名はクライアントの申告ではなく run 側の値を使う。
        let pending_name = pending
            .as_ref()
            .filter(|p| p.get("tool_call_id").and_then(|v| v.as_str()) == Some(tool_call_id))
            .and_then(|p| {
                p.get("tool_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
        let Some(tool_name) = pending_name else {
            // 先勝ちで既に決定済み（承認待ちが消えた直後の二重送信）は no-op として受ける。
            let decided: Option<String> = sqlx::query_scalar(
                "SELECT decision FROM run_approval WHERE run_id = $1 AND tool_call_id = $2",
            )
            .bind(run_id)
            .bind(tool_call_id)
            .fetch_optional(&self.db)
            .await
            .map_err(|e| map_db(&e))?;
            if decided.is_some() {
                return Ok(false);
            }
            return Err(ChatError::Invalid(
                "承認待ちのツール呼び出しではありません（承認要求の tool_call_id を指定してください）"
                    .into(),
            ));
        };
        // 申告のツール名が承認待ちと違う決定は記録しない（利用者が見た承認要求と別のものを承認したことに
        // なり得る・監査の記録が誤る・3 回目の検証 3-B-4）。
        if claimed_tool_name != tool_name {
            tracing::info!(claimed = claimed_tool_name, actual = %tool_name, "承認の申告ツール名が承認待ちと異なる（400）");
            return Err(ChatError::Invalid(
                "承認待ちのツール呼び出しとツール名が一致しません（承認要求の tool_name を指定してください）"
                    .into(),
            ));
        }
        let tool_name = tool_name.as_str();
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
                    action: AuditAction::AgentApprovalDecision,
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

    /// 承認待ちのツール呼び出しを run に記録する（承認要求の SSE を出す前・fencing 一致時のみ）。
    pub async fn set_pending_approval(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        tool_call_id: &str,
        tool_name: &str,
    ) -> Result<bool, ChatError> {
        let updated = sqlx::query(
            "UPDATE generation_run SET pending_approval = $3, updated_at = now() \
             WHERE run_id = $1 AND fencing_token = $2",
        )
        .bind(run_id)
        .bind(fencing_token)
        .bind(sqlx::types::Json(json!({
            "tool_call_id": tool_call_id,
            "tool_name": tool_name,
            "requested_at": chrono::Utc::now(),
        })))
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(updated.rows_affected() == 1)
    }

    /// 承認待ちのツール呼び出しの記録を消す（決定を拾って再開するとき・fencing 一致時のみ）。
    pub async fn clear_pending_approval(
        &self,
        run_id: Uuid,
        fencing_token: i64,
    ) -> Result<bool, ChatError> {
        let updated = sqlx::query(
            "UPDATE generation_run SET pending_approval = NULL \
             WHERE run_id = $1 AND fencing_token = $2 AND pending_approval IS NOT NULL",
        )
        .bind(run_id)
        .bind(fencing_token)
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(updated.rows_affected() == 1)
    }

    /// 承認待ちのツール呼び出し（`(tool_call_id, tool_name)`・無ければ `None`）。
    pub async fn pending_approval(
        &self,
        run_id: Uuid,
    ) -> Result<Option<(String, String)>, ChatError> {
        let p: Option<Option<sqlx::types::Json<serde_json::Value>>> =
            sqlx::query_scalar("SELECT pending_approval FROM generation_run WHERE run_id = $1")
                .bind(run_id)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| map_db(&e))?;
        Ok(p.flatten().and_then(|j| {
            let id = j.0.get("tool_call_id")?.as_str()?.to_string();
            let name = j.0.get("tool_name")?.as_str()?.to_string();
            Some((id, name))
        }))
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

    /// thread のワークスペース作成先の親フォルダ id を引く（未設定なら `None`＝Drive 直下）。
    /// 「親を選んで配下に新規作成」を選んだときだけ設定される（Phase 6 UX・0030）。
    pub async fn workspace_parent_folder_id(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
    ) -> Result<Option<Uuid>, ChatError> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT workspace_parent_folder_id FROM thread WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))?
        .flatten();
        Ok(id)
    }

    /// スレッド作成時にワークスペースの場所を設定する（Phase 6 UX・0030）。
    ///
    /// `folder_id`（既存フォルダをワークスペースにする）と `parent_folder_id`（配下に新規作成する
    /// 親）はどちらか一方のみ渡す想定（両 None は Drive 直下＝現行挙動）。認可はルート層が本人 ctx で
    /// 検証済み（`require_folder_editor`）。owner 本人のスレッドにのみ設定する。
    pub async fn set_thread_workspace(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
        folder_id: Option<Uuid>,
        parent_folder_id: Option<Uuid>,
    ) -> Result<(), ChatError> {
        sqlx::query(
            "UPDATE thread SET workspace_folder_id = $3, workspace_parent_folder_id = $4 \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .bind(folder_id)
        .bind(parent_folder_id)
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(())
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

impl ChatStore {
    /// スレッドの回答の本文のうち、```ui ブロックを含みうる text ブロック（実行前フェーズの門・#400）。
    ///
    /// 戻り値は `(メッセージ id, 本文)` を会話の順に並べたもの。ask・plan の有無は呼び出し側が
    /// `yui::extract_action_blocks` で読む（ボタンの経路と同じく、text ブロックごとに読む）。
    /// フェンスの無いブロックは jsonb の段階で落とす（全件取得 → フィルタをしない）。
    pub async fn fenced_answer_texts(
        &self,
        thread_id: uuid::Uuid,
        tenant_id: &str,
    ) -> Result<Vec<(uuid::Uuid, String)>, crate::ChatError> {
        sqlx::query_as(
            "SELECT m.id, b.value->>'text' \
             FROM message m, jsonb_array_elements(m.content) WITH ORDINALITY AS b(value, ord) \
             WHERE m.thread_id = $1 AND m.tenant_id = $2 AND m.role = 'assistant' \
               AND b.value->>'type' = 'text' AND b.value->>'text' ~ '(```|~~~)' \
             ORDER BY m.created_at, m.id, b.ord",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_all(&self.db)
        .await
        .map_err(|e| crate::ChatError::Internal(format!("回答の本文の取得に失敗: {e}")))
    }
}
