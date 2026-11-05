//! アーティファクトのバージョン操作（不変追記・楽観ロック・履歴・版取得）。

use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::types::Json;
use storage::audit::AuditAction;
use uuid::Uuid;

use crate::model::{Artifact, ArtifactKind, ArtifactVersion, ConflictReason, VersionMeta};
use crate::store::{validate_body, ArtifactStore};
use crate::{map_db, ArtifactError};

impl ArtifactStore {
    /// 新バージョンを追記する（editor 権限・不変追記・楽観ロック）。
    ///
    /// `expected_version` を渡すと現行バージョンと一致する場合のみ追記する
    /// （dnd/AI 編集の lost-update 防止）。`None` は無条件追記。
    pub async fn append_version(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        body: serde_json::Value,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<ArtifactVersion, ArtifactError> {
        validate_body(&body)?;
        self.require(
            ctx,
            id,
            Relation::Editor,
            AuditAction::ArtifactVersionAppend,
            trace_id,
        )
        .await?;

        let mut tx = self.db.begin().await.map_err(map_db)?;
        // current_version の加算が単一の正（同時追記は行ロックで直列化される）。
        let new_version: Option<i64> = sqlx::query_scalar(
            "UPDATE artifact SET current_version = current_version + 1, updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL \
               AND ($3::bigint IS NULL OR current_version = $3) \
             RETURNING current_version",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(expected_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db)?;
        let Some(new_version) = new_version else {
            tx.rollback().await.map_err(map_db)?;
            // 行が無い（削除済み）か expected_version 不一致かを区別して返す。不一致なら種類と今の最新の版を添える
            // （呼ぶ人は editor を確かめてある・API は「ほかの人が先に保存した」の 409 にする・GAP-41）。
            let current: Option<(String, i64)> = sqlx::query_as(
                "SELECT kind, current_version FROM artifact \
                 WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
            )
            .bind(&ctx.tenant_id)
            .bind(id)
            .fetch_optional(&self.db)
            .await
            .map_err(map_db)?;
            return Err(match current {
                Some((kind, current)) => {
                    let kind = ArtifactKind::parse(&kind)
                        .ok_or_else(|| ArtifactError::Internal(format!("未知の kind: {kind}")))?;
                    ArtifactError::Conflict(ConflictReason::VersionChanged { kind, current })
                }
                None => ArtifactError::NotFound,
            });
        };
        let created_at: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO artifact_version (tenant_id, artifact_id, version, body, created_by) \
             VALUES ($1, $2, $3, $4, $5) RETURNING created_at",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(new_version)
        .bind(Json(&body))
        .bind(&ctx.principal.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        // 監査を**同一 TX**で記録してからコミットする。コミット後に別 TX で監査すると、監査失敗時に
        // バージョンだけ永続化され再試行で想定外の Conflict を招く（CodeRabbit 指摘）。
        storage::audit::record_on(
            &mut tx,
            ctx,
            storage::audit::AuditEntry {
                action: AuditAction::ArtifactVersionAppend,
                object_type: "artifact",
                object_id: &id.to_string(),
                decision: storage::audit::Decision::Allow,
                trace_id,
                metadata: json!({ "version": new_version }),
            },
            storage::audit::Chain::No,
        )
        .await
        .map_err(|e| ArtifactError::Internal(format!("audit: {e}")))?;
        tx.commit().await.map_err(map_db)?;

        Ok(ArtifactVersion {
            artifact_id: id,
            version: new_version,
            body,
            created_by: ctx.principal.id.clone(),
            created_at,
        })
    }

    /// メタデータを取得する（viewer 権限）。
    pub async fn get(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Artifact, ArtifactError> {
        self.require(
            ctx,
            id,
            Relation::Viewer,
            AuditAction::ArtifactGet,
            trace_id,
        )
        .await?;
        self.fetch_live(ctx, id).await
    }

    /// 指定バージョンの本文を取得する（viewer 権限・過去バージョンも不変で取得できる）。
    ///
    /// 下書きと公開を分ける種類（[`ArtifactKind::has_drafts`]・ミニアプリ）では、editor でない人は公開した版しか
    /// 読めない（ほかの版は実在を問わず [`ArtifactError::NotPublished`]・D-16）。editor かの確かめは、公開した版の
    /// ほかを読むときだけ行う。
    ///
    /// [`ArtifactKind::has_drafts`]: crate::ArtifactKind::has_drafts
    pub async fn get_version(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<ArtifactVersion, ArtifactError> {
        self.require(
            ctx,
            id,
            Relation::Viewer,
            AuditAction::ArtifactVersionGet,
            trace_id,
        )
        .await?;
        // 論理削除済みアーティファクトのバージョン本文は読めない（tuple 残存でも body を返さない）。
        #[allow(clippy::type_complexity)]
        let row: Option<(
            String,
            Option<i64>,
            Option<Json<serde_json::Value>>,
            Option<String>,
            Option<DateTime<Utc>>,
        )> = sqlx::query_as(
            "SELECT a.kind, a.published_version, v.body, v.created_by, v.created_at \
             FROM artifact a \
             LEFT JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id AND v.version = $3 \
             WHERE a.tenant_id = $1 AND a.id = $2 AND a.deleted_at IS NULL",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(version)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let (kind, published, body, created_by, created_at) = row.ok_or(ArtifactError::NotFound)?;
        let drafts = ArtifactKind::parse(&kind).is_some_and(ArtifactKind::has_drafts);
        if drafts && published != Some(version) && !self.allows(ctx, id, Relation::Editor).await? {
            return Err(ArtifactError::NotPublished { published });
        }
        let (Some(body), Some(created_by), Some(created_at)) = (body, created_by, created_at)
        else {
            return Err(ArtifactError::NotFound);
        };
        Ok(ArtifactVersion {
            artifact_id: id,
            version,
            body: body.0,
            created_by,
            created_at,
        })
    }

    /// **バンドル経由**の版取得チョークポイント（Phase 6 Task 6.10・ミニアプリ）。
    ///
    /// 認可は **bundle（例: mini_app）への viewer** で行い、部品（component）の直接権限は
    /// 要求しない — ミニアプリの共有 = バンドル本体の ReBAC が正で、部品を個別共有せずに
    /// 共有相手が実行できる（miniapp-platform §7「アプリスコープ ∩ ユーザー ReBAC」。
    /// データアクセス自体はノード実行時に本人 ReBAC で二重ゲートされる）。
    ///
    /// ⚠️ 「component が bundle に**ピンされていること**」の照合は呼び出し側
    /// （`gui::MiniAppStore` 等・body の意味を知る層）の責務。本メソッドを直接公開 API に
    /// 繋がないこと（照合なしでは任意 artifact の読み出し口になる）。
    pub async fn get_version_via_bundle(
        &self,
        ctx: &AuthContext,
        bundle_id: Uuid,
        component_id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<ArtifactVersion, ArtifactError> {
        self.require(
            ctx,
            bundle_id,
            Relation::Viewer,
            AuditAction::ArtifactReadViaBundle,
            trace_id,
        )
        .await?;
        let row: Option<(Json<serde_json::Value>, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT v.body, v.created_by, v.created_at FROM artifact_version v \
             WHERE v.tenant_id = $1 AND v.artifact_id = $2 AND v.version = $3 \
               AND EXISTS (SELECT 1 FROM artifact a \
                           WHERE a.tenant_id = $1 AND a.id = $2 AND a.deleted_at IS NULL)",
        )
        .bind(&ctx.tenant_id)
        .bind(component_id)
        .bind(version)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let (body, created_by, created_at) = row.ok_or(ArtifactError::NotFound)?;
        // バンドル越しの部品読み出しを監査に残す（誰が・どのバンドル経由で・どの部品版を）。
        self.record_audit(
            ctx,
            AuditAction::ArtifactReadViaBundle,
            &component_id.to_string(),
            trace_id,
            json!({ "bundle_id": bundle_id, "version": version }),
        )
        .await?;
        Ok(ArtifactVersion {
            artifact_id: component_id,
            version,
            body: body.0,
            created_by,
            created_at,
        })
    }

    /// **参照元にピンされた版**の取得チョークポイント（FSM ↔ テーブルの `fsm_ref`・検証 C-P1-1）。
    ///
    /// アーティファクト自体の viewer は要求しない。認可は**参照元**（例: テーブルの editor）で行い、
    /// 「その版が参照元にピンされていること」の照合と合わせて**呼び出し側の責務**（`data::FsmStore` 等）。
    /// [`Self::get_version_via_bundle`] と同じく、照合なしの公開 API に繋がないこと（任意 artifact の
    /// 読み出し口になる）。種別が `kind` でなければ見つからない扱い。読み出しは監査に残す。
    pub async fn get_version_pinned(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        kind: crate::ArtifactKind,
        holder: &str,
        trace_id: Option<&str>,
    ) -> Result<ArtifactVersion, ArtifactError> {
        let row: Option<(Json<serde_json::Value>, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT v.body, v.created_by, v.created_at FROM artifact_version v \
             WHERE v.tenant_id = $1 AND v.artifact_id = $2 AND v.version = $3 \
               AND EXISTS (SELECT 1 FROM artifact a \
                           WHERE a.tenant_id = $1 AND a.id = $2 AND a.kind = $4 \
                             AND a.deleted_at IS NULL)",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(version)
        .bind(kind.as_str())
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let (body, created_by, created_at) = row.ok_or(ArtifactError::NotFound)?;
        self.record_audit(
            ctx,
            AuditAction::ArtifactReadViaReference,
            &id.to_string(),
            trace_id,
            json!({ "holder": holder, "version": version }),
        )
        .await?;
        Ok(ArtifactVersion {
            artifact_id: id,
            version,
            body: body.0,
            created_by,
            created_at,
        })
    }

    /// バージョン履歴（メタのみ・新しい順）。viewer 権限。
    ///
    /// 下書きと公開を分ける種類（ミニアプリ）では、editor でない人には公開した版だけを返す（まだ公開していなければ
    /// 空・D-16）。
    pub async fn list_versions(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Vec<VersionMeta>, ArtifactError> {
        self.require(
            ctx,
            id,
            Relation::Viewer,
            AuditAction::ArtifactVersionsList,
            trace_id,
        )
        .await?;
        let rows: Vec<(i64, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT v.version, v.created_by, v.created_at FROM artifact_version v \
             WHERE v.tenant_id = $1 AND v.artifact_id = $2 \
               AND EXISTS (SELECT 1 FROM artifact a \
                           WHERE a.tenant_id = $1 AND a.id = $2 AND a.deleted_at IS NULL) \
             ORDER BY v.version DESC",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let meta = self.fetch_live(ctx, id).await?;
        let only = if meta.kind.has_drafts() && !self.allows(ctx, id, Relation::Editor).await? {
            Some(meta.published_version)
        } else {
            None
        };
        Ok(rows
            .into_iter()
            .filter(|(version, _, _)| only.is_none_or(|p| p == Some(*version)))
            .map(|(version, created_by, created_at)| VersionMeta {
                version,
                created_by,
                created_at,
            })
            .collect())
    }
}
