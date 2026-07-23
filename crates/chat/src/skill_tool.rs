//! `skill` ツール — スキルのカタログ引き（#344 Task 10.11）。
//!
//! ツール定義の description にインストール済み/所有スキルの **name + description 一覧**を
//! 動的に載せ（本文は載せない）、call で該当 skill を**発話ユーザー権限**で解決して
//! instructions を観測テキストとして返す。既存のエージェントループがそのまま効く
//! （結果を見て次のアクションを選ぶ・1 メッセージ中に何個でも）。
//!
//! - **閉集合照合**: 構築時に確定した name→(id, version) マップからのみ引く（モデルに
//!   UUID/version を渡させない・ハルシネーション境界）。マップ外は観測エラー＋候補提示。
//! - **fail-closed**: 権限剥奪・削除は instructions を返さない（エラー観測のみ）。
//! - **発動記録**: 成功時に `(skill_id, version)` を [`agent_core::ToolOutcome::skill_invocations`]
//!   へ載せ、`AgentEvent::SkillInvoked` → `generation_event` に残す（監査・再現性）。
//! - スキルは能力を増やさない: instructions は助言テキストであり、承認ポリシ・認可には触れない。

use std::collections::HashMap;
use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::skill::AppliedSkill;
use crate::skill_catalog::{merge_entries, render_tool_description, SkillCatalogEntry};
use crate::store::ClaimedRun;
use crate::ChatError;

/// 観測エラーに列挙する候補名の上限。
const MAX_SUGGESTIONS: usize = 10;

/// name→解決材料（閉集合・run 開始時に固定）。
struct Resolvable {
    id: Uuid,
    version: i64,
    pinned: bool,
}

/// skill ツール本体（1 run 分・カタログは run 単位で固定）。
pub(crate) struct SkillTool {
    artifacts: Arc<artifact::ArtifactStore>,
    db: PgPool,
    /// 監査 metadata 用の run 文脈。
    thread_id: Uuid,
    run_id: Uuid,
    /// ミニアプリ経由のセッション（ピン済みスキルはバンドル権限で読む・Task 6.10）。
    mini_app_id: Option<Uuid>,
    by_name: HashMap<String, Resolvable>,
    description: String,
}

impl SkillTool {
    /// カタログからツールを組み立てる（エントリが空なら None＝提示しない）。
    pub(crate) fn build(
        artifacts: Arc<artifact::ArtifactStore>,
        db: PgPool,
        run: &ClaimedRun,
        pinned: Vec<SkillCatalogEntry>,
        source_entries: Vec<SkillCatalogEntry>,
    ) -> Option<SkillTool> {
        let entries = merge_entries(pinned, source_entries);
        if entries.is_empty() {
            return None;
        }
        let description = render_tool_description(&entries);
        let by_name = entries
            .into_iter()
            .map(|e| {
                (
                    e.name,
                    Resolvable {
                        id: e.id,
                        version: e.version,
                        pinned: e.pinned,
                    },
                )
            })
            .collect();
        Some(SkillTool {
            artifacts,
            db,
            thread_id: run.thread_id,
            run_id: run.run_id,
            mini_app_id: run.mini_app_id,
            by_name,
            description,
        })
    }

    /// 発動を監査に残す（`skill.apply` と同型・action は `skill.invoke`）。
    async fn audit_invoke(&self, ctx: &AuthContext, skill: &AppliedSkill, trace_id: Option<&str>) {
        let recorder = AuditRecorder::new(self.db.clone());
        let entry = AuditEntry {
            action: "skill.invoke",
            object_type: "artifact",
            object_id: &skill.id.to_string(),
            decision: Decision::Allow,
            trace_id,
            metadata: json!({
                "skill_version": skill.version,
                "thread_id": self.thread_id,
                "run_id": self.run_id,
                "mini_app_id": self.mini_app_id,
            }),
        };
        if let Err(e) = recorder.record(ctx, entry).await {
            tracing::warn!(error = %e, "skill.invoke の監査記録に失敗");
        }
    }

    /// 観測エラー用の候補名列（名前順・上限つき）。
    fn suggestions(&self) -> String {
        let mut names: Vec<&str> = self.by_name.keys().map(String::as_str).collect();
        names.sort_unstable();
        names.truncate(MAX_SUGGESTIONS);
        names.join(", ")
    }
}

#[async_trait::async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        ToolName::Skill.as_str()
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "読み込むスキル名（ツール説明の一覧にある name）"
                }
            },
            "required": ["name"]
        })
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::Invalid("name（スキル名）が必要です".into()))?;

        // 閉集合照合（構築時に確定したカタログからのみ引く・未知は観測エラー）。
        let Some(entry) = self.by_name.get(name) else {
            return Ok(ToolOutcome::error(format!(
                "スキル '{name}' はカタログにありません。利用可能: {}",
                self.suggestions()
            )));
        };

        // 発話ユーザー権限で解決（ミニアプリ経由のピンはバンドル権限）。fail-closed:
        // 剥奪・削除は instructions を返さない（エラーを観測させて回復させる）。
        let via_bundle = self.mini_app_id.filter(|_| entry.pinned);
        let skill = match AppliedSkill::resolve(
            ctx,
            &self.artifacts,
            entry.id,
            entry.version,
            via_bundle,
            trace_id,
        )
        .await
        {
            Ok(skill) => skill,
            Err(ChatError::Forbidden) => {
                return Ok(ToolOutcome::error(format!(
                    "スキル '{name}' を読む権限がありません（共有解除された可能性）"
                )));
            }
            Err(ChatError::Invalid(msg)) => return Ok(ToolOutcome::error(msg)),
            Err(e) => return Err(ToolError::Unavailable(format!("skill 読込: {e}"))),
        };

        self.audit_invoke(ctx, &skill, trace_id).await;

        let mut outcome = ToolOutcome::ok(skill.loaded_content());
        // 発動記録（run イベント→generation_event・「何をいつ適用したか」の完全な列）。
        outcome.skill_invocations = vec![json!({
            "skill_id": skill.id,
            "skill_version": skill.version,
            "name": skill.name,
        })];
        Ok(outcome)
    }
}
