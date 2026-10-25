//! 本文の参照で絞った一覧（例: あるテーブルの保存ビュー・Task 15.5）。`store.rs` から分離。
//!
//! 「自分が owner」∪「viewer の実効集合（共有された分）」を、**本文の参照キーの一致まで SQL で**
//! 絞ってから返す（全件取得→フィルタをしない）。tenant/org の突き合わせは一覧と同じ二重防御。

use authz::{AuthContext, ObjectType, Relation};
use uuid::Uuid;

use crate::model::{Artifact, ArtifactKind};
use crate::store::{ArtifactRow, ArtifactStore};
use crate::{map_db, ArtifactError};

impl ArtifactStore {
    /// `kind` の現行版の本文で `body[body_key] == body_value`（文字列）のもののうち、自分が owner か
    /// viewer のもの（更新日降順・上限 `limit`）。`body_key` はトップレベルのキー名（`[a-z_]+`）。
    pub async fn list_visible_with_body_ref(
        &self,
        ctx: &AuthContext,
        kind: ArtifactKind,
        body_key: &str,
        body_value: &str,
        limit: i64,
    ) -> Result<Vec<Artifact>, ArtifactError> {
        if body_key.is_empty() || !body_key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            return Err(ArtifactError::Invalid("本文の参照キーが不正です".into()));
        }
        let limit = limit.clamp(1, 200);
        let objs = self
            .authz
            .list_objects(&ctx.subject(), Relation::Viewer, ObjectType::Artifact)
            .await
            .map_err(|e| ArtifactError::Internal(e.to_string()))?;
        let mut shared: Vec<Uuid> = Vec::new();
        for o in objs {
            let Some((_, id_part)) = o.split_once(':') else {
                continue;
            };
            if let Some(local) = ctx.ns().strip_object_id(id_part) {
                if let Ok(id) = Uuid::parse_str(local) {
                    shared.push(id);
                }
            }
        }
        let rows: Vec<ArtifactRow> = sqlx::query_as(
            "SELECT a.id, a.kind, a.name, a.owner, a.current_version, a.created_at, a.updated_at \
             FROM artifact a \
             JOIN artifact_version v \
               ON v.tenant_id = a.tenant_id AND v.artifact_id = a.id AND v.version = a.current_version \
             WHERE a.tenant_id = $1 AND a.org = $2 AND a.kind = $3 AND a.deleted_at IS NULL \
               AND (a.owner = $4 OR a.id = ANY($5)) \
               AND v.body->>$6 = $7 \
             ORDER BY a.updated_at DESC, a.id DESC LIMIT $8",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(kind.as_str())
        .bind(&ctx.principal.id)
        .bind(&shared)
        .bind(body_key)
        .bind(body_value)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        rows.into_iter().map(ArtifactRow::into_artifact).collect()
    }
}
