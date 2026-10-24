//! ミニアプリの保存・解決（Task 6.10）。
//!
//! 保存時は**作成者の権限**で全ピン（skill/workflow）の存在・kind・viewer を検証する。画面の
//! ボタンの実行先がバンドルの別名に限られることは body の検証（[`validate_miniapp_body`]）が見る。
//! 解決（実行）時は**ミニアプリ本体の viewer**のみを要求し、部品はバンドル権限
//! チョークポイント（[`ArtifactStore::get_version_via_bundle`]）で読む — 共有相手は
//! 部品を個別共有されなくても実行できる（miniapp-platform §7）。

use std::sync::Arc;
use storage::audit::AuditAction;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use authz::AuthContext;
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::miniapp::{validate_miniapp_body, ComponentPin, MiniAppBody};
use crate::skill::{validate_skill_body, SkillBody};
use crate::store::GuiError;
use crate::validate::GuiValidationError;

/// 解決済みミニアプリ（実行面が使う一式・全て検証済み）。
#[derive(Debug, Clone)]
pub struct ResolvedMiniApp {
    pub id: Uuid,
    pub version: i64,
    /// 検証済みの本文（画面のボタンの実行先は `workflows` の別名に限る）。
    pub body: MiniAppBody,
    /// skill 本文（ピンがある場合）。
    pub skill: Option<SkillBody>,
}

/// ミニアプリの保存/解決。
#[derive(Clone)]
pub struct MiniAppStore {
    pub(crate) artifacts: Arc<ArtifactStore>,
    pub(crate) audit: AuditRecorder,
    /// 画面のボタンの起動の記録（`miniapp_action_run`）。
    pub(crate) db: PgPool,
}

impl MiniAppStore {
    pub fn new(artifacts: Arc<ArtifactStore>, db: PgPool) -> Self {
        MiniAppStore {
            artifacts,
            audit: AuditRecorder::new(db.clone()),
            db,
        }
    }

    /// 新しいミニアプリを保存する（構造検証＋ピン解決 → artifact version 1）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        name: &str,
        raw_body: &serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<(Uuid, MiniAppBody), GuiError> {
        let body = self.validate_pins(ctx, raw_body, trace_id).await?;
        let artifact = self
            .artifacts
            .create(
                ctx,
                NewArtifact {
                    kind: ArtifactKind::MiniApp,
                    name: name.to_string(),
                    body: raw_body.clone(),
                },
                trace_id,
            )
            .await?;
        Ok((artifact.id, body))
    }

    /// 既存ミニアプリに新バージョンを追記する。
    pub async fn update(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        raw_body: &serde_json::Value,
        expected_version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<(i64, MiniAppBody), GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::MiniApp {
            return Err(GuiError::Validation(vec![GuiValidationError::new(
                "miniapp.kind_mismatch",
                "このアーティファクトは mini_app ではありません",
            )]));
        }
        let body = self.validate_pins(ctx, raw_body, trace_id).await?;
        let version = self
            .artifacts
            .append_version(ctx, id, raw_body.clone(), expected_version, trace_id)
            .await?;
        Ok((version.version, body))
    }

    /// ミニアプリを解決する（本体 viewer → 部品はバンドル権限で読み込み・再検証）。
    ///
    /// `version` 省略時は current。戻りは全て検証済み（実行面はこの結果のみを信用する）。
    pub async fn resolve(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        version: Option<i64>,
        trace_id: Option<&str>,
    ) -> Result<ResolvedMiniApp, GuiError> {
        let meta = self.artifacts.get(ctx, id, trace_id).await?;
        if meta.kind != ArtifactKind::MiniApp {
            return Err(GuiError::Artifact(ArtifactError::NotFound));
        }
        let version = version.unwrap_or(meta.current_version);
        let v = self
            .artifacts
            .get_version(ctx, id, version, trace_id)
            .await?;
        // 描画前再検証: 保存済みでも壊れた本文（旧形式を含む）を実行面へ流さない。
        let body = validate_miniapp_body(&v.body).map_err(GuiError::Validation)?;

        let skill = match &body.skill {
            Some(pin) => {
                let s = self
                    .artifacts
                    .get_version_via_bundle(ctx, id, pin.artifact_id, pin.version, trace_id)
                    .await?;
                Some(validate_skill_body(&s.body).map_err(GuiError::Validation)?)
            }
            None => None,
        };

        // 解決を監査に残す（誰が・どの版を・どのピン集合で実行面へ持ち出したか・Task 6.12）。
        let entry = AuditEntry {
            action: AuditAction::MiniappResolve,
            object_type: "artifact",
            object_id: &id.to_string(),
            decision: Decision::Allow,
            trace_id,
            metadata: json!({
                "version": version,
                "skill": body.skill,
                "workflows": body.workflows,
            }),
        };
        if let Err(e) = self.audit.record(ctx, entry).await {
            tracing::warn!(error = %e, "miniapp.resolve の監査記録に失敗");
        }

        Ok(ResolvedMiniApp {
            id,
            version,
            body,
            skill,
        })
    }

    /// 保存時のピン解決（**作成者の権限**で存在・kind・viewer を検証する）。
    async fn validate_pins(
        &self,
        ctx: &AuthContext,
        raw_body: &serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<MiniAppBody, GuiError> {
        let body = validate_miniapp_body(raw_body).map_err(GuiError::Validation)?;
        let mut errors = Vec::new();
        if let Some(pin) = &body.skill {
            if let Err(e) = self
                .check_pin(ctx, pin, ArtifactKind::Skill, trace_id)
                .await
            {
                errors.push(e.at("skill"));
            }
        }
        for (i, wf) in body.workflows.iter().enumerate() {
            let pin = ComponentPin {
                artifact_id: wf.artifact_id,
                version: wf.version,
            };
            if let Err(e) = self
                .check_pin(ctx, &pin, ArtifactKind::Workflow, trace_id)
                .await
            {
                errors.push(e.at(format!("workflows[{i}]")));
            }
        }
        if errors.is_empty() {
            Ok(body)
        } else {
            Err(GuiError::Validation(errors))
        }
    }

    /// 1 ピンの存在・kind・viewer 検証（作成者権限・存在秘匿の同一メッセージ）。
    async fn check_pin(
        &self,
        ctx: &AuthContext,
        pin: &ComponentPin,
        kind: ArtifactKind,
        trace_id: Option<&str>,
    ) -> Result<serde_json::Value, GuiValidationError> {
        let unresolved = || {
            GuiValidationError::new(
                "miniapp.pin_unresolved",
                format!(
                    "参照 {}（kind={}）が見つからないか権限がありません",
                    pin.artifact_id,
                    kind.as_str()
                ),
            )
        };
        let meta = self
            .artifacts
            .get(ctx, pin.artifact_id, trace_id)
            .await
            .map_err(|e| match e {
                ArtifactError::NotFound | ArtifactError::Forbidden => unresolved(),
                other => GuiValidationError::new(
                    "miniapp.pin_resolve_error",
                    format!("参照解決に失敗しました: {other}"),
                ),
            })?;
        if meta.kind != kind {
            return Err(unresolved());
        }
        let v = self
            .artifacts
            .get_version(ctx, pin.artifact_id, pin.version, trace_id)
            .await
            .map_err(|e| match e {
                ArtifactError::NotFound | ArtifactError::Forbidden => unresolved(),
                other => GuiValidationError::new(
                    "miniapp.pin_resolve_error",
                    format!("参照解決に失敗しました: {other}"),
                ),
            })?;
        Ok(v.body)
    }
}
