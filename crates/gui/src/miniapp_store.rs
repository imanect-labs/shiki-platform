//! ミニアプリの保存・解決（Task 6.10）。
//!
//! 保存時は**作成者の権限**で全ピン（skill/ui_spec/workflow）の存在・kind・viewer を検証し、
//! UI スペック内の workflow 束縛がバンドルのピン集合に含まれることを照合する。
//! 解決（実行）時は**ミニアプリ本体の viewer**のみを要求し、部品はバンドル権限
//! チョークポイント（[`ArtifactStore::get_version_via_bundle`]）で読む — 共有相手は
//! 部品を個別共有されなくても実行できる（miniapp-platform §7）。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore, NewArtifact};
use authz::AuthContext;
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::action::ActionBinding;
use crate::miniapp::{validate_miniapp_body, ComponentPin, MiniAppBody};
use crate::skill::{validate_skill_body, SkillBody};
use crate::spec::UiSpecDoc;
use crate::store::GuiError;
use crate::validate::{validate_spec, GuiValidationError};

/// 解決済みミニアプリ（実行面が使う一式・全て検証済み）。
#[derive(Debug, Clone)]
pub struct ResolvedMiniApp {
    pub id: Uuid,
    pub version: i64,
    pub body: MiniAppBody,
    /// 検証済み UI スペック（描画・アクション束縛照合の正）。
    pub ui_spec: UiSpecDoc,
    /// UI スペックの生 JSON（レスポンス用）。
    pub ui_spec_json: serde_json::Value,
    /// skill 本文（ピンがある場合）。
    pub skill: Option<SkillBody>,
}

/// ミニアプリの保存/解決。
#[derive(Clone)]
pub struct MiniAppStore {
    artifacts: Arc<ArtifactStore>,
    audit: AuditRecorder,
}

impl MiniAppStore {
    pub fn new(artifacts: Arc<ArtifactStore>, db: PgPool) -> Self {
        MiniAppStore {
            artifacts,
            audit: AuditRecorder::new(db),
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
        let body = validate_miniapp_body(&v.body).map_err(GuiError::Validation)?;

        // 部品はバンドル権限チョークポイントで読む（部品の個別共有は不要）。
        let ui = self
            .artifacts
            .get_version_via_bundle(
                ctx,
                id,
                body.ui_spec.artifact_id,
                body.ui_spec.version,
                trace_id,
            )
            .await?;
        // 描画前再検証（Task 6.3 の「描画取得路」）: 保存済みでも壊れた部品を実行面へ流さない。
        let ui_spec = validate_spec(&ui.body).map_err(GuiError::Validation)?;
        Self::check_bindings_subset(&ui_spec, &body)?;

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
            action: "miniapp.resolve",
            object_type: "artifact",
            object_id: &id.to_string(),
            decision: Decision::Allow,
            trace_id,
            metadata: json!({
                "version": version,
                "ui_spec": body.ui_spec,
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
            ui_spec,
            ui_spec_json: ui.body,
            skill,
        })
    }

    /// UI スペック内の workflow 束縛がバンドルのピン集合 ⊆ であることを照合する
    /// （バンドル外のワークフローへ UI から到達できない）。
    fn check_bindings_subset(ui_spec: &UiSpecDoc, body: &MiniAppBody) -> Result<(), GuiError> {
        let mut errors = Vec::new();
        for binding in &ui_spec.actions {
            if let ActionBinding::Workflow(b) = binding {
                let pinned = b.workflow.artifact_id.zip(b.workflow.version);
                let in_bundle = pinned.is_some_and(|(id, ver)| {
                    body.workflows
                        .iter()
                        .any(|w| w.artifact_id == id && w.version == ver)
                });
                if !in_bundle {
                    errors.push(GuiValidationError::new(
                        "miniapp.binding_not_bundled",
                        format!(
                            "UI スペックの workflow 束縛 '{}' がバンドルのピン集合にありません",
                            b.id
                        ),
                    ));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(GuiError::Validation(errors))
        }
    }

    /// 保存時のピン解決（**作成者の権限**で存在・kind・viewer を検証し、束縛 ⊆ ピンを照合）。
    async fn validate_pins(
        &self,
        ctx: &AuthContext,
        raw_body: &serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<MiniAppBody, GuiError> {
        let body = validate_miniapp_body(raw_body).map_err(GuiError::Validation)?;
        let mut errors = Vec::new();

        let ui = self
            .check_pin(ctx, &body.ui_spec, ArtifactKind::UiSpec, trace_id)
            .await
            .map_err(|e| errors.push(e.at("ui_spec")))
            .ok();
        if let Some(ui_body) = ui {
            match validate_spec(&ui_body) {
                Ok(ui_spec) => {
                    if let Err(GuiError::Validation(mut e)) =
                        Self::check_bindings_subset(&ui_spec, &body)
                    {
                        errors.append(&mut e);
                    }
                }
                Err(mut e) => errors.append(&mut e),
            }
        }
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
