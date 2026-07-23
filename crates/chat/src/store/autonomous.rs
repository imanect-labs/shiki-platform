//! `ChatStore`: 自律 run の承認モード（#350）とチェックポイント永続化（#351）。
//!
//! - モードは thread 単位（実行中トグル可）。設定は editor 権限＋org キャップ検査＋監査。
//!   実行時の実効モード決定（設定者と actor の一致・クランプ）は [`crate::autonomous`] の純関数。
//! - チェックポイントは durable run 行（`generation_run.checkpoint`）へ **fencing 一致時のみ**
//!   書く（ゾンビ書込拒否）。端末確定（finalize）で NULL に落とす。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Relation};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use uuid::Uuid;

use crate::autonomous::AutonomousMode;

/// sqlx エラー → ChatError（他 store ファイルと同型の局所ヘルパ）。
fn map_db(e: &sqlx::Error) -> ChatError {
    ChatError::Internal(format!("db: {e}"))
}

impl ChatStore {
    /// thread の現在の承認モードと最終設定者（principal.id）を引く。
    pub async fn thread_autonomous_mode(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
    ) -> Result<(AutonomousMode, Option<String>), ChatError> {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT autonomous_mode, autonomous_mode_set_by FROM thread \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        let (mode, set_by) = row.ok_or(ChatError::NotFound)?;
        // CHECK 制約で閉じている（乖離時は既定＝承認必須へ・fail-closed）。
        Ok((AutonomousMode::parse(&mode).unwrap_or_default(), set_by))
    }

    /// org（tenant）で全自動（bypass）が許可されているか。未登録テナント（開発環境）は
    /// 行が無い＝許可（列既定 true と同じ扱い）。
    pub async fn autonomous_bypass_allowed(&self, tenant_id: &str) -> Result<bool, ChatError> {
        let allowed: Option<bool> =
            sqlx::query_scalar("SELECT allow_autonomous_bypass FROM tenant WHERE tenant_id = $1")
                .bind(tenant_id)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| map_db(&e))?;
        Ok(allowed.unwrap_or(true))
    }

    /// thread の承認モードを設定する（editor 権限・実行中トグルの入口・#350）。
    ///
    /// bypass は org キャップ（`tenant.allow_autonomous_bypass`）を検査し、禁止なら**明示エラー**
    /// で弾く（黙って降格しない）。設定者を `autonomous_mode_set_by` に記録し、実行中の緩和は
    /// run の actor 本人による設定のみ有効になる（[`crate::autonomous::effective_mode`]）。
    pub async fn set_autonomous_mode(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        mode: AutonomousMode,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Editor,
            "thread.set_autonomous_mode",
            trace_id,
        )
        .await?;
        if mode == AutonomousMode::Bypass && !self.autonomous_bypass_allowed(&ctx.tenant_id).await?
        {
            return Err(ChatError::Invalid(
                "全自動（bypass）モードは組織ポリシで禁止されています".into(),
            ));
        }
        let updated = sqlx::query(
            "UPDATE thread SET autonomous_mode = $3, autonomous_mode_set_by = $4 \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .bind(mode.as_str())
        .bind(&ctx.principal.id)
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        if updated.rows_affected() == 0 {
            return Err(ChatError::NotFound);
        }
        // 誰がどのモードへ切り替えたかは必ず監査へ（承認緩和の追跡・NFR-6）。
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "thread.set_autonomous_mode",
                    object_type: "thread",
                    object_id: &thread_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "mode": mode.as_str() }),
                },
            )
            .await
            .map_err(|e| ChatError::Internal(format!("audit: {e}")))?;
        Ok(())
    }

    /// ステップ境界のチェックポイントを durable run 行へ保存する（fencing 一致時のみ・#351）。
    ///
    /// 戻り `true` = 保存した、`false` = fencing 不一致 or 非走行状態（リース喪失＝呼び出し側は
    /// 停止すべき）。承認待ち（waiting_approval）中もリースと同様に書ける。
    pub async fn save_checkpoint(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        checkpoint: &serde_json::Value,
    ) -> Result<bool, ChatError> {
        let updated = sqlx::query(
            "UPDATE generation_run SET checkpoint = $3, updated_at = now() \
             WHERE run_id = $1 AND fencing_token = $2 \
               AND status IN ('running', 'waiting_approval')",
        )
        .bind(run_id)
        .bind(fencing_token)
        .bind(sqlx::types::Json(checkpoint))
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(updated.rows_affected() == 1)
    }

    /// takeover 時、チェックポイント境界より後に残る**破棄 attempt のイベント**を削除する（#351）。
    ///
    /// 境界より後の行はチェックポイントに含まれず当該ステップごと再生成されるため、残すと
    /// SSE replay が部分出力と再生成分を二重に流す。fencing 一致（現リース保持者）時のみ削除する
    /// （append-only の原則は「生き残った attempt の真実」を保つためのもので、これはその維持）。
    pub async fn prune_events_after(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        after_seq: i64,
    ) -> Result<u64, ChatError> {
        let deleted = sqlx::query(
            "DELETE FROM generation_event \
             WHERE run_id = $1 AND seq > $3 \
               AND EXISTS (SELECT 1 FROM generation_run \
                           WHERE run_id = $1 AND fencing_token = $2)",
        )
        .bind(run_id)
        .bind(fencing_token)
        .bind(after_seq)
        .execute(&self.db)
        .await
        .map_err(|e| map_db(&e))?;
        Ok(deleted.rows_affected())
    }
}
