//! 非同期の検証・解決層（Task 6.3/6.5）。
//!
//! 同期検証（[`validate_spec`](crate::validate::validate_spec)）に加え、workflow 束縛の
//! **存在・権限・バージョンピン**を発話ユーザーの `AuthContext` で解決する（アンビエント
//! 権限なし・存在秘匿）。検証拒否は監査（`ui_spec.validate` / Deny）に残す（Task 6.12）。
//! 保存路（UiSpecStore）・発話路（EmitUiTool）・解決路（ミニアプリ）の全てがここを通る。

use std::sync::Arc;

use artifact::{ArtifactError, ArtifactKind, ArtifactStore};
use authz::AuthContext;
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};

use crate::action::ActionBinding;
use crate::spec::UiSpecDoc;
use crate::validate::{validate_spec, GuiValidationError};

/// 検証・解決済みスペック（永続化・配信してよい唯一の形）。
#[derive(Debug, Clone)]
pub struct ResolvedSpec {
    /// 型付き文書（workflow 束縛は artifact_id/version が焼き込み済み）。
    pub doc: UiSpecDoc,
    /// 直列化 JSON（artifact 本文・generative_ui ブロックにそのまま入れる）。
    pub json: serde_json::Value,
}

/// スペック検証・解決の単一実装（保存・発話・解決の全経路が共有する）。
#[derive(Clone)]
pub struct SpecValidator {
    artifacts: Arc<ArtifactStore>,
    audit: AuditRecorder,
}

impl SpecValidator {
    pub fn new(artifacts: Arc<ArtifactStore>, db: PgPool) -> Self {
        SpecValidator {
            artifacts,
            audit: AuditRecorder::new(db),
        }
    }

    /// 生 JSON を検証・解決する。失敗は全件エラー＋監査 Deny。
    ///
    /// `source` は監査用の経路識別（"save" / "emit" / "miniapp.resolve" 等）。
    pub async fn validate(
        &self,
        ctx: &AuthContext,
        raw: &serde_json::Value,
        source: &str,
        trace_id: Option<&str>,
    ) -> Result<ResolvedSpec, Vec<GuiValidationError>> {
        let result = self.validate_inner(ctx, raw, trace_id).await;
        if let Err(errors) = &result {
            self.record_deny(ctx, source, errors, trace_id).await;
        }
        result
    }

    async fn validate_inner(
        &self,
        ctx: &AuthContext,
        raw: &serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ResolvedSpec, Vec<GuiValidationError>> {
        let mut doc = validate_spec(raw)?;
        let mut errors = Vec::new();
        for (i, binding) in doc.actions.iter_mut().enumerate() {
            if let ActionBinding::Workflow(b) = binding {
                let path = format!("actions[{i}]");
                match self.resolve_workflow(ctx, b, trace_id).await {
                    Ok(()) => {}
                    Err(e) => errors.push(e.at(path)),
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        // 解決済み文書を直列化し直す（ピンの焼き込みを本文へ反映）。
        match serde_json::to_value(&doc) {
            Ok(json) => Ok(ResolvedSpec { doc, json }),
            Err(e) => Err(vec![GuiValidationError::new(
                "gui.schema_violation",
                format!("直列化に失敗しました: {e}"),
            )]),
        }
    }

    /// workflow 束縛を発話ユーザーの viewer 権限で解決し、version をピンする。
    async fn resolve_workflow(
        &self,
        ctx: &AuthContext,
        binding: &mut crate::action::WorkflowBinding,
        trace_id: Option<&str>,
    ) -> Result<(), GuiValidationError> {
        let pin = &mut binding.workflow;
        // 参照解決（id 優先・無ければ名前）。権限なし/不在は同一メッセージ（存在秘匿）。
        let unresolved = |detail: &str| {
            GuiValidationError::new(
                "gui.action_workflow_unresolved",
                format!("参照ワークフロー（{detail}）が見つからないか権限がありません"),
            )
        };
        let meta = match (pin.artifact_id, pin.name.as_deref()) {
            (Some(id), _) => self
                .artifacts
                .get(ctx, id, trace_id)
                .await
                .map_err(|e| map_unresolved(e, &unresolved(&id.to_string())))?,
            (None, Some(name)) => self
                .artifacts
                .get_by_name(ctx, ArtifactKind::Workflow, name.trim(), trace_id)
                .await
                .map_err(|e| map_unresolved(e, &unresolved(name)))?,
            (None, None) => {
                // validate_spec が拒否済みだが防御的に扱う。
                return Err(unresolved("未指定"));
            }
        };
        if meta.kind != ArtifactKind::Workflow {
            // id 指定で他種 artifact を掴んだ場合も存在秘匿（種類は漏らさない）。
            return Err(unresolved(&meta.id.to_string()));
        }
        let version = match pin.version {
            Some(v) => {
                // 指定版の存在を確認する（不変版の直接取得）。
                self.artifacts
                    .get_version(ctx, meta.id, v, trace_id)
                    .await
                    .map_err(|e| map_unresolved(e, &unresolved(&format!("{}@v{v}", meta.id))))?;
                v
            }
            None => meta.current_version,
        };
        pin.name = Some(meta.name);
        pin.artifact_id = Some(meta.id);
        pin.version = Some(version);
        Ok(())
    }

    /// 検証拒否の監査（Task 6.12: セキュリティ事象の追跡可能性）。
    async fn record_deny(
        &self,
        ctx: &AuthContext,
        source: &str,
        errors: &[GuiValidationError],
        trace_id: Option<&str>,
    ) {
        let entry = AuditEntry {
            action: "ui_spec.validate",
            object_type: "ui_spec",
            object_id: source,
            decision: Decision::Deny,
            trace_id,
            metadata: json!({
                "source": source,
                "errors": errors,
            }),
        };
        if let Err(e) = self.audit.record(ctx, entry).await {
            tracing::warn!(error = %e, "ui_spec.validate の監査記録に失敗");
        }
    }
}

/// artifact 層のエラーを「見つからないか権限がない」へ写す（内部エラーのみ区別）。
fn map_unresolved(e: ArtifactError, unresolved: &GuiValidationError) -> GuiValidationError {
    match e {
        ArtifactError::NotFound | ArtifactError::Forbidden => unresolved.clone(),
        other => GuiValidationError::new(
            "gui.workflow_resolve_error",
            format!("参照解決に失敗しました: {other}"),
        ),
    }
}
