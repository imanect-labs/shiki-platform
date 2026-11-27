//! ミニアプリ（mini_app_code）の保存/publish（Task 9.1 / 9.13a）。
//!
//! マニフェストを `artifact(kind=mini_app_code)` として保存する（6.1 枠・不変バージョン・
//! ReBAC 共有・監査＝A/B 同一経路）。保存時に語彙照合検証（[`crate::validate`]）を通す。
//! publish はマニフェストの現行バージョンをレジストリへ不変登録する。

use std::sync::Arc;

use artifact::{ArtifactKind, ArtifactStore, NewArtifact};
use authz::{AuthContext, Relation};
use sha2::{Digest, Sha256};
use storage::audit::{record_on, AuditAction, AuditEntry, Chain, Decision};
use uuid::Uuid;

use crate::manifest::MiniAppManifest;
use crate::registry::{NewRegistryEntry, Registry, RegistryEntry};
use crate::validate::validate_manifest;
use crate::{map_artifact, map_db, AppPlatformError};

/// ミニアプリの保存/publish チョークポイント。
#[derive(Clone)]
pub struct MiniAppCodeStore {
    artifacts: Arc<ArtifactStore>,
    registry: Registry,
}

impl MiniAppCodeStore {
    pub fn new(artifacts: Arc<ArtifactStore>, registry: Registry) -> Self {
        MiniAppCodeStore {
            artifacts,
            registry,
        }
    }

    /// マニフェストを保存する（検証 → artifact version 1）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        manifest: &MiniAppManifest,
        trace_id: Option<&str>,
    ) -> Result<Uuid, AppPlatformError> {
        validate_manifest(manifest)?;
        let raw = serde_json::to_value(manifest)
            .map_err(|e| AppPlatformError::Invalid(format!("manifest: {e}")))?;
        let a = self
            .artifacts
            .create(
                ctx,
                NewArtifact {
                    kind: ArtifactKind::MiniAppCode,
                    name: manifest.name.clone(),
                    body: raw,
                },
                trace_id,
            )
            .await
            .map_err(map_artifact)?;
        Ok(a.id)
    }

    /// 新バージョンを追記する（検証 → 不変追記）。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        manifest: &MiniAppManifest,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<i64, AppPlatformError> {
        self.ensure_kind(ctx, id, trace_id).await?;
        validate_manifest(manifest)?;
        let raw = serde_json::to_value(manifest)
            .map_err(|e| AppPlatformError::Invalid(format!("manifest: {e}")))?;
        let v = self
            .artifacts
            .append_version(ctx, id, raw, expected_version, trace_id)
            .await
            .map_err(map_artifact)?;
        Ok(v.version)
    }

    /// 指定バージョン（省略時最新）のマニフェストを取得する（viewer）。
    pub async fn get(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, MiniAppManifest), AppPlatformError> {
        let meta = self
            .artifacts
            .get(ctx, id, trace_id)
            .await
            .map_err(map_artifact)?;
        if meta.kind != ArtifactKind::MiniAppCode {
            return Err(AppPlatformError::NotFound);
        }
        let ver = version.unwrap_or(meta.current_version);
        let v = self
            .artifacts
            .get_version(ctx, id, ver, trace_id)
            .await
            .map_err(map_artifact)?;
        let manifest: MiniAppManifest = serde_json::from_value(v.body)
            .map_err(|e| AppPlatformError::Internal(format!("manifest 破損: {e}")))?;
        Ok((v.version, manifest))
    }

    /// マニフェストの指定バージョンをレジストリへ不変 publish する。
    ///
    /// artifact の owner のみが publish できる（読める viewer・editor は publish できない）。共有で viewer になった
    /// 人が name@version を先に公開すると、unique 制約で持ち主がその版を出せなくなるため、最初に owner を確かめる
    /// （足りなければ `app.registry.publish` の deny を監査に残して 403・GAP-69a）。登録と、`app.registry.publish`
    /// の監査（チェーンにつなぐ・対象は `miniapp` のアプリの id）は同じ Tx で書く。
    pub async fn publish(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        signature: Option<&[u8]>,
        trace_id: Option<&str>,
    ) -> Result<RegistryEntry, AppPlatformError> {
        self.artifacts
            .require_relation(
                ctx,
                id,
                Relation::Owner,
                AuditAction::AppRegistryPublish,
                trace_id,
            )
            .await
            .map_err(map_artifact)?;
        let (ver, manifest) = self.get(ctx, id, version, trace_id).await?;
        let digest = manifest_digest(&manifest)?;
        let mut tx = self.registry.begin().await?;
        let entry = self
            .registry
            .publish_on(
                &mut tx,
                ctx,
                NewRegistryEntry {
                    artifact_kind: ArtifactKind::MiniAppCode.as_str(),
                    name: &manifest.name,
                    version: &manifest.version,
                    artifact_id: id,
                    artifact_version: ver,
                    manifest_digest: &digest,
                    trust_tier: trust_tier_str(&manifest),
                    signature,
                },
            )
            .await?;
        record_on(
            &mut tx,
            ctx,
            AuditEntry {
                action: AuditAction::AppRegistryPublish,
                object_type: "miniapp",
                object_id: &id.to_string(),
                decision: Decision::Allow,
                trace_id,
                metadata: serde_json::json!({
                    "name": entry.name,
                    "version": entry.version,
                    "artifact_id": id,
                    "artifact_version": ver,
                }),
            },
            Chain::Yes,
        )
        .await
        .map_err(|e| AppPlatformError::Internal(format!("audit: {e}")))?;
        tx.commit().await.map_err(map_db)?;
        Ok(entry)
    }

    async fn ensure_kind(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), AppPlatformError> {
        let meta = self
            .artifacts
            .get(ctx, id, trace_id)
            .await
            .map_err(map_artifact)?;
        if meta.kind != ArtifactKind::MiniAppCode {
            return Err(AppPlatformError::Invalid(
                "このアーティファクトは mini_app_code ではありません".into(),
            ));
        }
        Ok(())
    }
}

/// マニフェストの正準 digest（canonical JSON の sha256・改竄検知/署名対象）。
pub fn manifest_digest(manifest: &MiniAppManifest) -> Result<String, AppPlatformError> {
    // serde_json はキー順序を保持しないため、値を BTreeMap ベースへ正準化してから直列化する。
    let value = serde_json::to_value(manifest)
        .map_err(|e| AppPlatformError::Invalid(format!("manifest: {e}")))?;
    let canonical = canonical_json(&value);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

/// レジストリ first-party 署名の対象 digest（name/version に束縛・#344）。
///
/// body digest だけを署名すると、正当に署名された body を**別名/別バージョンで再 publish**して
/// 公式スキルをスプーフィング/スクワッティングできてしまう（レビュー指摘）。署名対象へ
/// レジストリの `name`/`version` を織り込み、publish・import・install の全経路でこの digest を
/// 検証する（replay 不可）。
pub fn registry_signing_digest(name: &str, version: &str, body_digest: &str) -> String {
    value_digest(&serde_json::json!({
        "name": name,
        "version": version,
        "body_digest": body_digest,
    }))
}

/// 任意の JSON 値の正準 digest（canonical JSON の sha256 hex・#344）。
///
/// skill レジストリの publish/署名対象（skill body）で使う。マニフェストは
/// [`manifest_digest`] を使うこと（型付き経路を優先）。
pub fn value_digest(value: &serde_json::Value) -> String {
    let canonical = canonical_json(value);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex::encode(hasher.finalize())
}

/// 正準 JSON（オブジェクトのキーを辞書順にソート・空白なし）。
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical_json(&map[*k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn trust_tier_str(manifest: &MiniAppManifest) -> &'static str {
    manifest.trust_tier.as_str()
}
