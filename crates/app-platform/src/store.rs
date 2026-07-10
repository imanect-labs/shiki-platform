//! ミニアプリ（mini_app_code）の保存/publish（Task 9.1 / 9.13a）。
//!
//! マニフェストを `artifact(kind=mini_app_code)` として保存する（6.1 枠・不変バージョン・
//! ReBAC 共有・監査＝A/B 同一経路）。保存時に語彙照合検証（[`crate::validate`]）を通す。
//! publish はマニフェストの現行バージョンをレジストリへ不変登録する。

use std::sync::Arc;

use artifact::{ArtifactKind, ArtifactStore, NewArtifact};
use authz::AuthContext;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::manifest::MiniAppManifest;
use crate::registry::{NewRegistryEntry, Registry, RegistryEntry};
use crate::validate::validate_manifest;
use crate::{map_artifact, AppPlatformError};

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
    /// artifact の owner のみが publish できる（`get` の viewer で読めることに加え、
    /// レジストリは owner 前提。呼び出し側 API が owner を要求する）。
    pub async fn publish(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        signature: Option<&[u8]>,
        trace_id: Option<&str>,
    ) -> Result<RegistryEntry, AppPlatformError> {
        let (ver, manifest) = self.get(ctx, id, version, trace_id).await?;
        let digest = manifest_digest(&manifest)?;
        self.registry
            .publish(
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
            .await
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
    match manifest.trust_tier {
        crate::manifest::TrustTier::FirstParty => "first_party",
        crate::manifest::TrustTier::InHouse => "in_house",
        crate::manifest::TrustTier::Marketplace => "marketplace",
    }
}
