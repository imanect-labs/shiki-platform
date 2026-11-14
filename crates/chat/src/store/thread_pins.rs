//! `ChatStore`: thread の skill / ミニアプリのバージョンピン操作（Task 6.7/6.10・#344 で複数化）。
//!
//! ピンの意味は「最初からロード済みのスキル」（順序付き・複数可・`thread_skill_pin` テーブル）。
//! 途中適用は skill ツール（カタログ引き・[`crate::skill_tool`]）が担う。post 時に
//! `generation_run.skill_pins`（jsonb スナップショット）へコピーされる（[`super::runs`]）。

#[allow(clippy::wildcard_imports)]
use super::*;
use storage::audit::AuditAction;

use authz::{AuthContext, Relation};
use serde_json::json;
use storage::audit::{AuditEntry, Decision};
use uuid::Uuid;

use crate::model::SkillPin;

/// スレッドの対象と目的（[`ChatStore::thread_builder`]）。名前は持たない（注記の名前は run の開始時に読み直す）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadBuilder {
    pub origin: Option<(Uuid, crate::origin::OriginArtifactKind)>,
    pub purpose: Option<crate::origin::ThreadPurpose>,
}

/// [`ChatStore::thread_builder`] が読む列（対象の id・種類・目的）。
type BuilderRow = (Option<Uuid>, Option<String>, Option<String>);

impl ThreadBuilder {
    /// 作成用の会話か（最初から作成用の予算・チェックポイントで動かす）。
    #[must_use]
    pub fn is_builder(&self) -> bool {
        self.origin.is_some() || self.purpose.is_some()
    }
}

impl ChatStore {
    /// スレッドに会話の対象と目的を設定する（作成直後・ワークフローエディタの「AI に依頼」・D-10 T09）。
    ///
    /// 対象の存在・種類と**作成者の viewer** は API 層（ArtifactStore）が確かめてから呼ぶこと（検索フォルダの
    /// 設定 [`Self::set_thread_search_folder`] と同じ分担）。誘導にしか使わないので権限は広げない。
    /// 後からの付け替えはここではしない（owner の画面から別の入口で行う・ツールからは付け替えない）。
    pub async fn set_thread_builder(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
        origin: Option<&crate::origin::OriginArtifact>,
        purpose: Option<crate::origin::ThreadPurpose>,
    ) -> Result<(), ChatError> {
        sqlx::query(
            "UPDATE thread SET origin_artifact_id = $3, origin_artifact_kind = $4, \
             origin_artifact_name = $5, purpose = $6 \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .bind(origin.map(|o| o.id))
        .bind(origin.map(|o| o.kind.as_str()))
        .bind(origin.map(|o| o.name.as_str()))
        .bind(purpose.map(crate::origin::ThreadPurpose::as_str))
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// 会話の対象を付け替える（「AI と作る」の会話で作ったアプリを、その会話の対象にする・D-10 T11）。
    ///
    /// **owner のみ**（共有された会話の編集者は付け替えられない）。対象の存在・種類と owner の viewer は API 層が
    /// 確かめてから呼ぶこと。文書の会話（`origin_note_id` を持つ）は付け替えない（どちらの画面の会話か 1 つに決める・
    /// 0159 の CHECK）。目的（purpose）はそのまま残す。
    pub async fn set_thread_origin_artifact(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        origin: &crate::origin::OriginArtifact,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Owner,
            AuditAction::ThreadSetOrigin,
            trace_id,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE thread SET origin_artifact_id = $3, origin_artifact_kind = $4, \
             origin_artifact_name = $5, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL AND origin_note_id IS NULL",
        )
        .bind(thread_id)
        .bind(ctx.tenant_id())
        .bind(origin.id)
        .bind(origin.kind.as_str())
        .bind(&origin.name)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(ChatError::Invalid(
                "文書の会話は、ほかの対象に付け替えられません".into(),
            ));
        }
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::ThreadSetOrigin,
                    object_type: "thread",
                    object_id: &thread_id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata: json!({
                        "origin_artifact_id": origin.id.to_string(),
                        "origin_artifact_kind": origin.kind.as_str(),
                    }),
                },
            )
            .await
            .map_err(map_storage)?;
        Ok(())
    }

    /// スレッドの対象と目的（生成ワーカーが注記・道具・予算を決めるのに使う・無ければ `None`）。
    pub async fn thread_builder(
        &self,
        tenant_id: &str,
        thread_id: Uuid,
    ) -> Result<ThreadBuilder, ChatError> {
        let row: Option<BuilderRow> = sqlx::query_as(
            "SELECT origin_artifact_id, origin_artifact_kind, purpose FROM thread \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some((id, kind, purpose)) = row else {
            return Ok(ThreadBuilder::default());
        };
        Ok(ThreadBuilder {
            origin: id.zip(
                kind.as_deref()
                    .and_then(crate::origin::OriginArtifactKind::parse),
            ),
            purpose: purpose
                .as_deref()
                .and_then(crate::origin::ThreadPurpose::parse),
        })
    }

    /// スレッドの検索フォルダを設定する（作成直後・ホームの「社内検索」・#965）。
    ///
    /// フォルダの存在・種別と**作成者の viewer** は API 層（StorageService）が確かめてから呼ぶこと
    /// （ワークスペースの設定 [`Self::set_thread_workspace`] と同じ分担）。絞り込みにしか使わないので
    /// 権限は広げない。
    pub async fn set_thread_search_folder(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
        folder_id: Uuid,
        folder_name: &str,
    ) -> Result<(), ChatError> {
        sqlx::query(
            "UPDATE thread SET search_folder_id = $3, search_folder_name = $4 \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .bind(folder_id)
        .bind(folder_name)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// スレッドの検索フォルダ（生成ワーカーが doc_search の絞り込みに使う・未設定は `None`）。
    pub async fn thread_search_folder(
        &self,
        tenant_id: &str,
        thread_id: Uuid,
    ) -> Result<Option<Uuid>, ChatError> {
        let folder: Option<Option<Uuid>> = sqlx::query_scalar(
            "SELECT search_folder_id FROM thread WHERE id = $1 AND tenant_id = $2",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(folder.flatten())
    }

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
        self.require_thread(
            ctx,
            thread_id,
            Relation::Owner,
            AuditAction::ThreadSetPins,
            trace_id,
        )
        .await?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let updated = sqlx::query(
            "UPDATE thread SET mini_app_id = $3, mini_app_version = $4, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(thread_id)
        .bind(ctx.tenant_id())
        .bind(mini_app.map(|(id, _)| id))
        .bind(mini_app.map(|(_, v)| v))
        .execute(&mut *tx)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            tx.rollback().await.map_err(map_db)?;
            return Err(ChatError::NotFound);
        }
        replace_skill_pins(&mut tx, thread_id, ctx.tenant_id(), skills).await?;
        tx.commit().await.map_err(map_db)?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::ThreadSetPins,
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
            AuditAction::ThreadSetSkills,
            trace_id,
        )
        .await?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let mini_app: Option<Option<Uuid>> = sqlx::query_scalar(
            "SELECT mini_app_id FROM thread \
             WHERE id = $1 AND tenant_id = $2 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(thread_id)
        .bind(ctx.tenant_id())
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
        replace_skill_pins(&mut tx, thread_id, ctx.tenant_id(), skills).await?;
        sqlx::query("UPDATE thread SET updated_at = now() WHERE id = $1 AND tenant_id = $2")
            .bind(thread_id)
            .bind(ctx.tenant_id())
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::ThreadSetSkills,
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
