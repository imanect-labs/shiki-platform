//! `ChatStore`: thread の skill / ミニアプリのバージョンピン操作（Task 6.7/6.10・#344 で複数化）。
//!
//! ピンの意味は「最初からロード済みのスキル」（順序付き・複数可・`thread_skill_pin` テーブル）。
//! 途中適用は skill ツール（カタログ引き・[`crate::skill_tool`]）が担う。post 時に
//! `generation_run.skill_pins`（jsonb スナップショット）へコピーされる（[`super::runs`]）。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Relation};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use uuid::Uuid;

use crate::model::SkillPin;

impl ChatStore {
    /// スレッドに skill（複数可）/ ミニアプリのバージョンピンを設定する（作成直後・owner のみ・
    /// Task 6.7/6.10・#344 で複数化）。
    ///
    /// 参照の存在・kind・viewer 検証は API 層（SkillStore/MiniAppStore）が**設定者の権限**で
    /// 済ませてから呼ぶこと。ピンは再現性のため version 込みで固定される。
    pub async fn set_thread_pins(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        skills: &[SkillPin],
        mini_app: Option<(Uuid, i64)>,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(ctx, thread_id, Relation::Owner, "thread.set_pins", trace_id)
            .await?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let updated = sqlx::query(
            "UPDATE thread SET mini_app_id = $3, mini_app_version = $4, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .bind(mini_app.map(|(id, _)| id))
        .bind(mini_app.map(|(_, v)| v))
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            tx.rollback().await.map_err(map_db)?;
            return Err(ChatError::NotFound);
        }
        replace_skill_pins(&mut tx, thread_id, &ctx.tenant_id, skills).await?;
        tx.commit().await.map_err(map_db)?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "thread.set_pins",
                    object_type: "thread",
                    object_id: &thread_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({
                        "skills": skills,
                        "mini_app": mini_app.map(|(id, v)| json!({ "artifact_id": id, "version": v })),
                    }),
                },
            )
            .await
            .map_err(map_storage)?;
        Ok(())
    }

    /// スレッドの skill ピン集合を置き換える（owner のみ・途中変更 API・#344）。
    ///
    /// ミニアプリ経由のセッションはバンドル定義のピンが正なので置換を拒否する
    /// （バンドルの skill ピンが個別指定より優先、という Task 6.10 の意味を壊さない）。
    /// 参照の存在・kind・viewer 検証は API 層が**設定者の権限**で済ませてから呼ぶこと。
    pub async fn set_thread_skills(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        skills: &[SkillPin],
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Owner,
            "thread.set_skills",
            trace_id,
        )
        .await?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let mini_app: Option<Option<Uuid>> = sqlx::query_scalar(
            "SELECT mini_app_id FROM thread \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        match mini_app {
            None => {
                tx.rollback().await.map_err(map_db)?;
                return Err(ChatError::NotFound);
            }
            Some(Some(_)) => {
                tx.rollback().await.map_err(map_db)?;
                return Err(ChatError::Invalid(
                    "ミニアプリ経由のスレッドはバンドル定義の skill ピンが正のため変更できません"
                        .into(),
                ));
            }
            Some(None) => {}
        }
        replace_skill_pins(&mut tx, thread_id, &ctx.tenant_id, skills).await?;
        sqlx::query("UPDATE thread SET updated_at = now() WHERE id = $1 AND tenant_id = $2")
            .bind(thread_id)
            .bind(&ctx.tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: "thread.set_skills",
                    object_type: "thread",
                    object_id: &thread_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({ "skills": skills }),
                },
            )
            .await
            .map_err(map_storage)?;
        Ok(())
    }
}

/// thread の skill ピン集合を置き換える（TX 内・delete→insert・position は指定順・#344）。
///
/// 同一 skill の重複は API 層が排除してから呼ぶ（PK (thread_id, skill_id) の最終防衛あり）。
async fn replace_skill_pins(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    thread_id: Uuid,
    tenant_id: &str,
    skills: &[SkillPin],
) -> Result<(), ChatError> {
    sqlx::query("DELETE FROM thread_skill_pin WHERE thread_id = $1 AND tenant_id = $2")
        .bind(thread_id)
        .bind(tenant_id)
        .execute(&mut **tx)
        .await
        .map_err(map_db)?;
    for (i, pin) in skills.iter().enumerate() {
        sqlx::query(
            "INSERT INTO thread_skill_pin (thread_id, tenant_id, skill_id, skill_version, position) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .bind(pin.skill_id)
        .bind(pin.skill_version)
        .bind(i32::try_from(i).unwrap_or(i32::MAX))
        .execute(&mut **tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                ChatError::Invalid("同じ skill を複数回ピンできません".into())
            }
            _ => map_db(e),
        })?;
    }
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> ChatError {
    ChatError::Internal(format!("db: {e}"))
}

#[allow(clippy::needless_pass_by_value)]
fn map_storage(e: storage::StorageError) -> ChatError {
    ChatError::Internal(format!("audit: {e}"))
}
