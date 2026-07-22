//! first-party skill バンドルの署名検証つき import（10.15・#344・500 行規約で分離）。
//!
//! 本体の publish / インストールは [`super::skill_install`]。

use authz::AuthContext;
use serde_json::json;
use uuid::Uuid;

use crate::sign::verify_digest_signature;
use crate::skill_install::{map_artifact, SkillInstallService, SKILL_KIND};
use crate::store::value_digest;
use crate::{AppPlatformError, NewRegistryEntry, RegistryEntry};

impl SkillInstallService {
    /// first-party skill バンドルの署名検証つき import（10.15・#344）。
    ///
    /// リポジトリ同梱の skill body（正規化 JSON digest への ed25519 署名つき）を、
    /// **登録済み信頼鍵で検証してから** artifact 化 → first-party として publish する。
    /// マイグレーションに業務コンテンツを埋めず、エアギャップ配布と同一経路にする
    /// （ミニアプリのオフライン import・`install_ops.rs` と同型）。
    ///
    /// - 検証は import 時と install 時の**二重**（レジストリ行の署名は install 時にも再検証）。
    /// - artifact は importer 所有で作る（同名が既にあれば新バージョン追記）。
    pub async fn import(
        &self,
        ctx: &AuthContext,
        name: &str,
        version: &str,
        body: &serde_json::Value,
        signature: &[u8],
        trace_id: Option<&str>,
    ) -> Result<RegistryEntry, AppPlatformError> {
        // 署名検証（fail-closed・鍵は管理者が登録した信頼鍵のみ）。署名対象は name/version に
        // 束縛する（body だけの署名だと、正当な署名を別名で再 import して公式スキルを
        // スプーフィングできてしまう・レビュー指摘。publish/install と同じ signing digest）。
        let body_typed = gui::validate_skill_body(body)
            .map_err(|e| AppPlatformError::Invalid(format!("skill body が不正です: {e:?}")))?;
        // `.shiki` script はコンパイル検証（skill.invoke が実行するため壊れた script を配布しない）。
        crate::skill_install::compile_shiki_scripts(&body_typed)?;
        let digest = value_digest(body);
        let signing = crate::registry_signing_digest(name, version, &digest);
        let keys = self.keys.active_key_bytes(ctx).await?;
        let ok = keys
            .iter()
            .any(|k| verify_digest_signature(&signing, signature, k).is_ok());
        if !ok {
            self.audit_deny(ctx, Uuid::nil(), "skill.import.signature", trace_id)
                .await;
            return Err(AppPlatformError::Forbidden);
        }

        // 冪等性/二重送信対策: artifact を触る前にレジストリ衝突（同一 name+version）を先に弾く
        // （publish は不変で 409 を返すが、artifact append はその前に走ってしまうと未 publish の
        // バージョンが履歴に残る・レビュー指摘）。
        if self
            .registry
            .get(ctx, SKILL_KIND, name, version)
            .await?
            .is_some()
        {
            return Err(AppPlatformError::Conflict(format!(
                "{name}@{version} は既に import 済みです（不変）"
            )));
        }

        // artifact 化（同名は新バージョン追記・importer 所有）。
        let (skill_id, skill_version) = match self
            .artifacts
            .get_by_name(ctx, artifact::ArtifactKind::Skill, name, trace_id)
            .await
        {
            Ok(meta) => {
                let v = self
                    .artifacts
                    .append_version(ctx, meta.id, body.clone(), None, trace_id)
                    .await
                    .map_err(map_artifact)?;
                (meta.id, v.version)
            }
            Err(artifact::ArtifactError::NotFound) => {
                let meta = self
                    .artifacts
                    .create(
                        ctx,
                        artifact::NewArtifact {
                            kind: artifact::ArtifactKind::Skill,
                            name: name.to_string(),
                            body: body.clone(),
                        },
                        trace_id,
                    )
                    .await
                    .map_err(map_artifact)?;
                (meta.id, meta.current_version)
            }
            Err(e) => return Err(map_artifact(e)),
        };

        let entry = self
            .registry
            .publish(
                ctx,
                NewRegistryEntry {
                    artifact_kind: SKILL_KIND,
                    name,
                    version,
                    artifact_id: skill_id,
                    artifact_version: skill_version,
                    manifest_digest: &digest,
                    trust_tier: "first_party",
                    signature: Some(signature),
                },
            )
            .await?;
        self.record(
            ctx,
            "skill.import",
            &skill_id.to_string(),
            trace_id,
            json!({ "name": name, "version": version }),
        )
        .await;
        Ok(entry)
    }
}
