//! 公開した版の決定（下書きと公開を分ける種類・ミニアプリ・D-16・GAP-36）。`versions.rs` から分離。
//!
//! 保存は下書きの版（`artifact_version` の追記）で、使う人（editor でない viewer）に出すのは公開した版
//! （`artifact.published_version`）だけ（[`ArtifactStore::get_version`]）。公開は editor が版を選んで行い、
//! 前に公開していた版は `previous_published_version` に残す（公開を戻す先）。公開できる版かの中身の確かめ
//! （書き損じた図を止めるなど）は、本文の意味を知る呼び出し側（`gui::MiniAppStore`）が先に行う。

use authz::{AuthContext, Relation};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use storage::audit::AuditAction;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::model::ArtifactKind;
use crate::store::ArtifactStore;
use crate::{map_db, ArtifactError};

/// 公開した版（公開の結果）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Publication {
    /// 公開した版。
    pub published_version: i64,
    /// 前に公開していた版（公開を戻す先・初めての公開なら `null`）。
    pub previous_published_version: Option<i64>,
    pub published_at: DateTime<Utc>,
}

impl ArtifactStore {
    /// `version` を公開した版にする（editor・下書きと公開を分ける種類だけ・D-16）。
    ///
    /// 前の版を公開し直すと公開を戻せる（公開していた版は前に公開していた版に移る）。いま公開している版を
    /// 公開し直しても何も変えない（前に公開していた版を潰さない）。版が無ければ [`ArtifactError::NotFound`]、
    /// 公開を分けない種類は [`ArtifactError::Invalid`]。公開は同じトランザクションで監査（`artifact.publish`）に残す。
    pub async fn publish(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: i64,
        trace_id: Option<&str>,
    ) -> Result<Publication, ArtifactError> {
        self.require(
            ctx,
            id,
            Relation::Editor,
            AuditAction::ArtifactPublish,
            trace_id,
        )
        .await?;
        let mut tx = self.db.begin().await.map_err(map_db)?;
        // 公開を決める行をロックする（同時の公開・保存と直列にし、前に公開していた版を取りこぼさない）。
        #[allow(clippy::type_complexity)]
        let row: Option<(String, i64, Option<i64>, Option<i64>, Option<DateTime<Utc>>)> =
            sqlx::query_as(
                "SELECT kind, current_version, published_version, previous_published_version, \
                        published_at \
                 FROM artifact WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL FOR UPDATE",
            )
            .bind(ctx.tenant_id())
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db)?;
        let (kind, current, published, previous, published_at) =
            row.ok_or(ArtifactError::NotFound)?;
        if !ArtifactKind::parse(&kind).is_some_and(ArtifactKind::has_drafts) {
            return Err(ArtifactError::Invalid(format!(
                "kind={kind} は下書きと公開を分けません"
            )));
        }
        if version < 1 || version > current {
            return Err(ArtifactError::NotFound);
        }
        if let (Some(p), Some(at)) = (published, published_at) {
            if p == version {
                tx.rollback().await.map_err(map_db)?;
                return Ok(Publication {
                    published_version: p,
                    previous_published_version: previous,
                    published_at: at,
                });
            }
        }
        let published_at: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE artifact SET previous_published_version = published_version, \
                    published_version = $3, published_at = now(), published_by = $4, updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 RETURNING published_at",
        )
        .bind(ctx.tenant_id())
        .bind(id)
        .bind(version)
        .bind(&ctx.principal().id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        storage::audit::record_on(
            &mut tx,
            ctx,
            storage::audit::AuditEntry {
                action: AuditAction::ArtifactPublish,
                object_type: "artifact",
                object_id: &id.to_string(),
                decision: storage::audit::Decision::Allow,
                trace_id,
                metadata: json!({ "version": version, "previous_version": published }),
            },
            storage::audit::Chain::No,
        )
        .await
        .map_err(|e| ArtifactError::Internal(format!("audit: {e}")))?;
        tx.commit().await.map_err(map_db)?;
        Ok(Publication {
            published_version: version,
            previous_published_version: published,
            published_at,
        })
    }
}
