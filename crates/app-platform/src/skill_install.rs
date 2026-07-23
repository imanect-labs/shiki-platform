//! skill の publish / 同意インストール（#344 Task 10.11・スキルストア）。
//!
//! **Phase 9 レジストリ（[`Registry`]・kind='skill'）を流用する**（新しい配布機構は作らない・
//! miniapp-platform.md §4）。インストールは**ユーザー単位**（human 決定・#344）:
//!
//! - **publish** = skill artifact の owner（human 確定判断）。署名対象は skill body の
//!   正規化 JSON digest（[`crate::value_digest`]・バンドル改竄は digest 不一致で検知）。
//! - **install** = 本人の明示行為（カタログ掲載）。信頼ティアで検証が分岐する:
//!   - `first_party`: 有効な信頼鍵の署名必須（[`crate::sign::verify_digest_signature`]）。
//!     検証成功時に本人へ artifact の viewer タプルを付与する（個別共有なしで読める＝
//!     「first-party 署名により管理者の個別同意なしで利用可能」・10.15）。
//!   - `in_house`: 本人が既に viewer で読める skill のみ（共有＝閲覧可、インストール＝
//!     カタログ掲載、の分離。読めない skill はインストールできない＝fail-closed）。
//!   - `marketplace`/未知: 拒否（審査トラック未実装・fail-closed）。
//!
//! スキルは能力を増やさない: インストールが付与するのは**読取（viewer）とカタログ掲載**のみ。
//! 実行時の実効権限は常に 実行主体 ReBAC ∩ スキル宣言（認可は artifact チョークポイント）。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Relation};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::sign::verify_digest_signature;
use crate::store::value_digest;
use crate::trusted_key::TrustedKeyStore;
use crate::{map_db, AppPlatformError, NewRegistryEntry, Registry, RegistryEntry};

/// レジストリ上の skill の artifact_kind（registry_entry.artifact_kind）。
const SKILL_KIND: &str = "skill";

/// インストール済み skill 1 件（カタログ・一覧 API 用）。
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
pub struct SkillInstallation {
    pub name: String,
    pub registry_version: String,
    pub skill_id: Uuid,
    pub skill_version: i64,
    pub trust_tier: String,
    pub created_at: DateTime<Utc>,
}

/// カタログ用の要約（name + description のみ・本文は載せない）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InstalledSkillSummary {
    pub name: String,
    pub skill_id: Uuid,
    pub skill_version: i64,
    pub description: String,
    pub trust_tier: String,
}

/// skill の publish / 同意インストールのチョークポイント。
#[derive(Clone)]
pub struct SkillInstallService {
    db: PgPool,
    registry: Registry,
    keys: TrustedKeyStore,
    artifacts: Arc<artifact::ArtifactStore>,
    authz: Arc<dyn AuthzClient>,
    audit: AuditRecorder,
}

impl SkillInstallService {
    pub fn new(
        db: PgPool,
        registry: Registry,
        keys: TrustedKeyStore,
        artifacts: Arc<artifact::ArtifactStore>,
        authz: Arc<dyn AuthzClient>,
    ) -> Self {
        let audit = AuditRecorder::new(db.clone());
        SkillInstallService {
            db,
            registry,
            keys,
            artifacts,
            authz,
            audit,
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// skill をレジストリへ不変 publish する（artifact owner のみ・human 確定判断）。
    ///
    /// `version_label` はレジストリの公開バージョン（IR の `skill:<name>@<version>` 語彙）。
    /// 未指定は artifact の current_version 文字列。`signature` は body digest への
    /// ed25519 署名（first-party ティアのみ必須・検証はインストール時）。
    pub async fn publish(
        &self,
        ctx: &AuthContext,
        skill_id: Uuid,
        version_label: Option<&str>,
        trust_tier: &str,
        signature: Option<&[u8]>,
        trace_id: Option<&str>,
    ) -> Result<RegistryEntry, AppPlatformError> {
        // owner 検査＋kind 検査（artifact チョークポイント経由・監査つき）。
        let meta = self
            .artifacts
            .get(ctx, skill_id, trace_id)
            .await
            .map_err(map_artifact)?;
        if meta.kind != artifact::ArtifactKind::Skill {
            return Err(AppPlatformError::Invalid("skill ではありません".into()));
        }
        if meta.owner != ctx.principal.id {
            self.audit_deny(ctx, skill_id, "skill.publish", trace_id)
                .await;
            return Err(AppPlatformError::Forbidden);
        }
        if !matches!(trust_tier, "first_party" | "in_house") {
            return Err(AppPlatformError::Invalid(format!(
                "信頼ティア '{trust_tier}' は publish できません"
            )));
        }
        let version = self
            .artifacts
            .get_version(ctx, skill_id, meta.current_version, trace_id)
            .await
            .map_err(map_artifact)?;
        // 保存時検証を通っている body か防御的に確認（壊れた行をレジストリへ流さない）。
        let body = gui::validate_skill_body(&version.body)
            .map_err(|e| AppPlatformError::Invalid(format!("skill body が不正です: {e:?}")))?;
        // `.shiki` script は publish 時にコンパイル検証する（skill.invoke が実行するため、
        // 壊れた script を配布してから全 run で落とさない・レビュー指摘）。
        compile_shiki_scripts(&body)?;
        let digest = value_digest(&version.body);
        let label = version_label.map_or_else(|| meta.current_version.to_string(), str::to_string);
        // first-party は **publish 時にも**署名を要求・検証する（レジストリ一覧/ストアが
        // 検証前のエントリを「公式」表示してしまうスプーフィングを防ぐ・レビュー指摘。
        // install 時の再検証は維持＝二重）。署名対象は name/version に束縛する（別名 replay 不可）。
        if trust_tier == "first_party" {
            let Some(sig) = signature else {
                return Err(AppPlatformError::Invalid(
                    "first-party publish には署名が必要です".into(),
                ));
            };
            let signing = crate::registry_signing_digest(&meta.name, &label, &digest);
            let keys = self.keys.active_key_bytes(ctx).await?;
            let ok = keys
                .iter()
                .any(|k| verify_digest_signature(&signing, sig, k).is_ok());
            if !ok {
                self.audit_deny(ctx, skill_id, "skill.publish.signature", trace_id)
                    .await;
                return Err(AppPlatformError::Forbidden);
            }
        }
        let entry = self
            .registry
            .publish(
                ctx,
                NewRegistryEntry {
                    artifact_kind: SKILL_KIND,
                    name: &meta.name,
                    version: &label,
                    artifact_id: skill_id,
                    artifact_version: meta.current_version,
                    manifest_digest: &digest,
                    trust_tier,
                    signature,
                },
            )
            .await?;
        self.record(
            ctx,
            "skill.publish",
            &skill_id.to_string(),
            trace_id,
            json!({ "name": meta.name, "version": label, "trust_tier": trust_tier }),
        )
        .await;
        Ok(entry)
    }

    /// skill を本人のカタログへインストールする（ユーザー単位・同意＝明示行為）。
    pub async fn install(
        &self,
        ctx: &AuthContext,
        name: &str,
        version: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<SkillInstallation, AppPlatformError> {
        // ① レジストリ解決（yank 済みは新規インストール不可）。
        let entry = match version {
            Some(v) => self.registry.get(ctx, SKILL_KIND, name, v).await?,
            None => self.registry.latest(ctx, SKILL_KIND, name).await?,
        }
        .ok_or(AppPlatformError::NotFound)?;
        if entry.yanked {
            return Err(AppPlatformError::Conflict(
                "このバージョンは yank されています".into(),
            ));
        }

        // ② 信頼ティア検証（first-party=署名必須／in-house=viewer 必須／その他=拒否）。
        match entry.trust_tier.as_str() {
            "first_party" => {
                let sig = self
                    .registry
                    .signature_of(ctx, SKILL_KIND, &entry.name, &entry.version)
                    .await?
                    .ok_or_else(|| {
                        AppPlatformError::Invalid(
                            "first-party skill は署名付き publish が必要です".into(),
                        )
                    })?;
                // entry の name/version/body-digest から署名対象を**再計算**して検証する
                // （stored digest をそのまま信用せず、別名でのなりすましを弾く・レビュー指摘）。
                let signing = crate::registry_signing_digest(
                    &entry.name,
                    &entry.version,
                    &entry.manifest_digest,
                );
                let keys = self.keys.active_key_bytes(ctx).await?;
                let ok = keys
                    .iter()
                    .any(|k| verify_digest_signature(&signing, &sig, k).is_ok());
                if !ok {
                    self.audit_deny(ctx, entry.artifact_id, "skill.install.signature", trace_id)
                        .await;
                    return Err(AppPlatformError::Forbidden);
                }
                // 署名検証済み: 本人へ viewer を付与（個別共有なしで読める・冪等）。
                let obj = ctx.ns().artifact(&entry.artifact_id.to_string());
                self.authz
                    .write_tuple(&ctx.subject(), Relation::Viewer, &obj)
                    .await
                    .map_err(|e| AppPlatformError::Internal(format!("viewer tuple: {e}")))?;
            }
            "in_house" => {
                // 本人が読める（viewer）ことを artifact チョークポイントで確認（fail-closed）。
                self.artifacts
                    .get(ctx, entry.artifact_id, trace_id)
                    .await
                    .map_err(map_artifact)?;
            }
            other => {
                return Err(AppPlatformError::Invalid(format!(
                    "信頼ティア '{other}' はインストールできません"
                )));
            }
        }

        // ③ installation 行（同名は置換＝バージョン更新・ユーザー単位）。
        let row: SkillInstallation = sqlx::query_as(
            "INSERT INTO skill_installation \
             (tenant_id, org, user_id, name, registry_entry_id, registry_version, \
              skill_id, skill_version, trust_tier) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (tenant_id, user_id, name) DO UPDATE SET \
               registry_entry_id = excluded.registry_entry_id, \
               registry_version = excluded.registry_version, \
               skill_id = excluded.skill_id, \
               skill_version = excluded.skill_version, \
               trust_tier = excluded.trust_tier, \
               created_at = now() \
             RETURNING name, registry_version, skill_id, skill_version, trust_tier, created_at",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .bind(&entry.name)
        .bind(entry.id)
        .bind(&entry.version)
        .bind(entry.artifact_id)
        .bind(entry.artifact_version)
        .bind(&entry.trust_tier)
        .fetch_one(&self.db)
        .await
        .map_err(map_db)?;

        self.record(
            ctx,
            "skill.install",
            &entry.artifact_id.to_string(),
            trace_id,
            json!({ "name": entry.name, "version": entry.version, "trust_tier": entry.trust_tier }),
        )
        .await;
        Ok(row)
    }

    /// インストールを解除する（本人のカタログから外す・artifact 本体には触れない）。
    pub async fn uninstall(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<(), AppPlatformError> {
        let deleted = sqlx::query(
            "DELETE FROM skill_installation \
             WHERE tenant_id = $1 AND user_id = $2 AND name = $3",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .bind(name)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if deleted.rows_affected() == 0 {
            return Err(AppPlatformError::NotFound);
        }
        self.record(
            ctx,
            "skill.uninstall",
            name,
            trace_id,
            json!({ "name": name }),
        )
        .await;
        Ok(())
    }

    /// 本人のインストール済み一覧（一覧 API 用）。
    pub async fn list_installed(
        &self,
        ctx: &AuthContext,
    ) -> Result<Vec<SkillInstallation>, AppPlatformError> {
        let rows: Vec<SkillInstallation> = sqlx::query_as(
            "SELECT name, registry_version, skill_id, skill_version, trust_tier, created_at \
             FROM skill_installation \
             WHERE tenant_id = $1 AND user_id = $2 \
             ORDER BY created_at DESC LIMIT 200",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows)
    }

    /// 本人のインストール済み要約（skill ツールのカタログ用・description 込み・単一クエリ）。
    ///
    /// 並び順は first-party → in-house → 新しい順（信頼ティアを既定表示へ反映・#344）。
    pub async fn list_installed_summaries(
        &self,
        ctx: &AuthContext,
    ) -> Result<Vec<InstalledSkillSummary>, AppPlatformError> {
        let rows: Vec<InstalledSkillSummary> = sqlx::query_as(
            "SELECT i.name, i.skill_id, i.skill_version, i.trust_tier, \
                    coalesce(v.body->>'description', '') AS description \
             FROM skill_installation i \
             JOIN artifact_version v \
               ON v.tenant_id = i.tenant_id AND v.artifact_id = i.skill_id \
              AND v.version = i.skill_version \
             JOIN artifact a \
               ON a.tenant_id = i.tenant_id AND a.id = i.skill_id AND a.deleted_at IS NULL \
             WHERE i.tenant_id = $1 AND i.user_id = $2 \
             ORDER BY (i.trust_tier <> 'first_party'), i.created_at DESC LIMIT 200",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows)
    }

    /// V4 skill 照合用: 保存ユーザーのインストール集合（name → レジストリ version 集合・10.1b）。
    pub async fn installed_versions(
        &self,
        ctx: &AuthContext,
    ) -> Result<
        std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
        AppPlatformError,
    > {
        // 削除済み skill artifact は除外する（保存できても実行時に必ず fail-closed になる
        // IR を保存時に green-light しない・レビュー指摘）。
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT i.name, i.registry_version FROM skill_installation i \
             JOIN artifact a \
               ON a.tenant_id = i.tenant_id AND a.id = i.skill_id AND a.deleted_at IS NULL \
             WHERE i.tenant_id = $1 AND i.user_id = $2",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.principal.id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        let mut out: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for (name, version) in rows {
            out.entry(name).or_default().insert(version);
        }
        Ok(out)
    }

    async fn record(
        &self,
        ctx: &AuthContext,
        action: &'static str,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) {
        let entry = AuditEntry {
            action,
            object_type: "artifact",
            object_id,
            decision: Decision::Allow,
            trace_id,
            metadata,
        };
        if let Err(e) = self.audit.record(ctx, entry).await {
            tracing::warn!(error = %e, action, "skill レジストリの監査記録に失敗");
        }
    }

    async fn audit_deny(
        &self,
        ctx: &AuthContext,
        skill_id: Uuid,
        action: &'static str,
        trace_id: Option<&str>,
    ) {
        let entry = AuditEntry {
            action,
            object_type: "artifact",
            object_id: &skill_id.to_string(),
            decision: Decision::Deny,
            trace_id,
            metadata: json!({}),
        };
        if let Err(e) = self.audit.record(ctx, entry).await {
            tracing::warn!(error = %e, action, "skill レジストリの監査記録に失敗");
        }
    }
}

/// `.shiki` script のコンパイル検証（publish/import 時・skill.invoke の実行前提・#344）。
pub(crate) fn compile_shiki_scripts(body: &gui::SkillBody) -> Result<(), AppPlatformError> {
    for script in &body.scripts {
        if script.kind != gui::ScriptKind::Shiki {
            continue;
        }
        script_runtime::compile::compile(&script.source).map_err(|e| {
            AppPlatformError::Invalid(format!(
                "shiki script '{}' がコンパイルできません: {e}",
                script.path
            ))
        })?;
    }
    Ok(())
}

/// artifact 層のエラーを写す（NotFound/Forbidden は秘匿せずそのまま・fail-closed）。
#[allow(clippy::needless_pass_by_value)]
fn map_artifact(e: artifact::ArtifactError) -> AppPlatformError {
    match e {
        artifact::ArtifactError::Forbidden => AppPlatformError::Forbidden,
        artifact::ArtifactError::NotFound => AppPlatformError::NotFound,
        other => AppPlatformError::Internal(format!("artifact: {other}")),
    }
}
