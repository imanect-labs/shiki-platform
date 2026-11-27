//! バンドル経由で部品の版をまとめて読む（GAP-37・アプリを使う人の権限の確かめ）。

use std::collections::HashMap;

use authz::{AuthContext, Relation};
use serde_json::json;
use sqlx::types::Json;
use storage::audit::AuditAction;
use uuid::Uuid;

use crate::model::ArtifactKind;
use crate::store::ArtifactStore;
use crate::{map_db, ArtifactError};

/// 1 回にまとめて読める部品の版の数の上限。
pub const MAX_BUNDLE_COMPONENTS: usize = 64;

impl ArtifactStore {
    /// **バンドル経由**で、部品の版（`(部品の id, 版)`）をまとめて読む（[`Self::get_version_via_bundle`] のまとめ版）。
    ///
    /// 認可はバンドル本体の viewer の 1 問だけで、部品の直接の権限は求めない。戻りは読めた部品の版の種類と本文で、
    /// 消えた部品・無い版は入れない（呼び出し側が「使えない部品」として扱う）。監査は 1 行（`artifact.read_via_bundle`・
    /// 対象はバンドル、読んだ部品の版と `purpose` を metadata に残す）。
    ///
    /// ⚠️ 部品がバンドルに**ピンされていること**の照合は呼び出し側（`gui` の部品を知る層）の責務。本メソッドを直接
    /// 公開 API に繋がないこと（照合なしでは任意 artifact の読み出し口になる）。
    pub async fn get_versions_via_bundle(
        &self,
        ctx: &AuthContext,
        bundle_id: Uuid,
        pins: &[(Uuid, i64)],
        purpose: &str,
        trace_id: Option<&str>,
    ) -> Result<HashMap<(Uuid, i64), (ArtifactKind, serde_json::Value)>, ArtifactError> {
        if pins.len() > MAX_BUNDLE_COMPONENTS {
            return Err(ArtifactError::Invalid(format!(
                "部品は 1 回に {MAX_BUNDLE_COMPONENTS} 件までしか読めません"
            )));
        }
        self.require(
            ctx,
            bundle_id,
            Relation::Viewer,
            AuditAction::ArtifactReadViaBundle,
            trace_id,
        )
        .await?;
        if pins.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<Uuid> = pins.iter().map(|(id, _)| *id).collect();
        let versions: Vec<i64> = pins.iter().map(|(_, v)| *v).collect();
        let rows: Vec<(Uuid, i64, String, Json<serde_json::Value>)> = sqlx::query_as(
            "SELECT v.artifact_id, v.version, a.kind, v.body FROM artifact_version v \
             JOIN artifact a ON a.tenant_id = v.tenant_id AND a.id = v.artifact_id \
                            AND a.deleted_at IS NULL \
             JOIN UNNEST($2::uuid[], $3::bigint[]) AS p(id, version) \
               ON p.id = v.artifact_id AND p.version = v.version \
             WHERE v.tenant_id = $1",
        )
        .bind(ctx.tenant_id())
        .bind(&ids)
        .bind(&versions)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let read: Vec<serde_json::Value> = rows
            .iter()
            .map(|(id, version, _, _)| json!({ "id": id, "version": version }))
            .collect();
        self.record_audit(
            ctx,
            AuditAction::ArtifactReadViaBundle,
            &bundle_id.to_string(),
            trace_id,
            json!({ "components": read, "purpose": purpose }),
        )
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, version, kind, body)| {
                ArtifactKind::parse(&kind).map(|k| ((id, version), (k, body.0)))
            })
            .collect())
    }
}
