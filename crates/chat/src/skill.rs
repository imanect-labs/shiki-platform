//! skill のチャット適用（Task 6.9・呼び出し面①「チャット開始時の初期コンテキスト適用」）。
//!
//! run にピンされた skill を**発話ユーザーの権限**で読み（ミニアプリ経由はバンドル権限
//! チョークポイント）、system 指示文・few-shot・許可ツール（**縮小のみ**）・モデル既定・
//! 知識スコープを生成へ反映する。**承認ポリシ（破壊系の明示許可・Task 3.9）には一切
//! 触れない** — skill はそれを無効化できない。
//!
//! fail-closed: ピンがあるのに読めない（共有剥奪・削除・未配線）場合は run を失敗させる
//! （skill 無しで黙って生成しない）。

use std::sync::Arc;

use agent_core::{AgentOptions, Tool};
use authz::AuthContext;
use llm_gateway::{Message as LlmMessage, Role as LlmRole};
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use uuid::Uuid;

use crate::store::ClaimedRun;
use crate::ChatError;

/// 適用対象として解決済みの skill。
#[derive(Debug)]
pub(crate) struct AppliedSkill {
    pub(crate) id: Uuid,
    pub(crate) version: i64,
    pub(crate) name: String,
    pub(crate) body: gui::SkillBody,
}

impl AppliedSkill {
    /// run のピンから skill を解決する（発話ユーザー権限・fail-closed）。
    pub(crate) async fn load(
        ctx: &AuthContext,
        artifacts: Option<&Arc<artifact::ArtifactStore>>,
        run: &ClaimedRun,
        trace_id: Option<&str>,
    ) -> Result<Option<AppliedSkill>, ChatError> {
        let Some((skill_id, skill_version)) = run.skill_id.zip(run.skill_version) else {
            return Ok(None);
        };
        let Some(artifacts) = artifacts else {
            return Err(ChatError::Unavailable(
                "skill が指定されていますが artifact ストアが未配線です".into(),
            ));
        };
        // ミニアプリ経由のセッションはバンドル権限で読む（部品の個別共有は不要・Task 6.10）。
        // 単体 skill は本人の viewer で読む。剥奪・削除は Forbidden/NotFound = run 失敗。
        let (meta_name, version_body) =
            if let Some((bundle_id, _)) = run.mini_app_id.zip(run.mini_app_version) {
                let v = artifacts
                    .get_version_via_bundle(ctx, bundle_id, skill_id, skill_version, trace_id)
                    .await
                    .map_err(map_skill_err)?;
                (format!("skill:{skill_id}"), v)
            } else {
                let meta = artifacts
                    .get(ctx, skill_id, trace_id)
                    .await
                    .map_err(map_skill_err)?;
                if meta.kind != artifact::ArtifactKind::Skill {
                    return Err(ChatError::Invalid(
                        "ピンされた参照が skill ではありません".into(),
                    ));
                }
                let v = artifacts
                    .get_version(ctx, skill_id, skill_version, trace_id)
                    .await
                    .map_err(map_skill_err)?;
                (meta.name, v)
            };
        let body = gui::validate_skill_body(&version_body.body).map_err(|errors| {
            ChatError::Internal(format!("保存済み skill body が不正です: {errors:?}"))
        })?;
        Ok(Some(AppliedSkill {
            id: skill_id,
            version: skill_version,
            name: meta_name,
            body,
        }))
    }

    /// 知識スコープ（doc_search / 古典 RAG 注入の絞り込み・Task 6.8）。
    pub(crate) fn search_scope(&self) -> Option<rag::SearchScope> {
        self.body
            .knowledge_scope
            .as_ref()
            .map(|s| rag::SearchScope {
                folders: s.folders.clone(),
                files: s.files.clone(),
            })
    }

    /// system プロンプトへ SKILL.md 指示文を追記する。
    pub(crate) fn apply_system(&self, system: &mut String) {
        system.push_str("\n\n# Skill: ");
        system.push_str(&self.name);
        system.push('\n');
        system.push_str(&self.body.instructions);
    }

    /// few-shot を履歴の先頭に user/assistant 対で注入する。
    pub(crate) fn apply_few_shot(&self, history: &mut Vec<LlmMessage>) {
        let mut prefix = Vec::with_capacity(self.body.few_shot.len() * 2);
        for ex in &self.body.few_shot {
            prefix.push(LlmMessage::text(LlmRole::User, ex.user.clone()));
            prefix.push(LlmMessage::text(LlmRole::Assistant, ex.assistant.clone()));
        }
        history.splice(0..0, prefix);
    }

    /// モデル/パラメータ既定を反映する（指定があるものだけ上書き）。
    pub(crate) fn apply_model_defaults(&self, opts: &mut AgentOptions) {
        let Some(model) = &self.body.model else {
            return;
        };
        if let Some(m) = &model.model {
            opts.model = Some(m.clone());
        }
        if model.temperature.is_some() {
            opts.temperature = model.temperature;
        }
        if let Some(mt) = model.max_tokens {
            opts.max_tokens = Some(mt);
        }
    }

    /// 許可ツール集合で提示ツールを**縮小**する（None は全提示のまま・Task 6.9）。
    ///
    /// ⚠️ `opts.approval` には触れない — 破壊系ツールの明示許可（Task 3.9）は skill で
    /// 無効化できない（許可集合に含めても承認要求は依然発生する）。
    pub(crate) fn filter_tools(&self, tools: &mut Vec<Arc<dyn Tool>>) {
        if let Some(allowed) = &self.body.allowed_tools {
            tools.retain(|t| allowed.iter().any(|name| name.as_str() == t.name()));
        }
    }

    /// 適用を監査に残す（Task 6.12: 「適用した skill バージョン」）。
    pub(crate) async fn audit_apply(&self, db: &PgPool, ctx: &AuthContext, run: &ClaimedRun) {
        let recorder = AuditRecorder::new(db.clone());
        let entry = AuditEntry {
            action: "skill.apply",
            object_type: "artifact",
            object_id: &self.id.to_string(),
            decision: Decision::Allow,
            trace_id: run.trace_id.as_deref(),
            metadata: json!({
                "skill_version": self.version,
                "thread_id": run.thread_id,
                "run_id": run.run_id,
                "mini_app_id": run.mini_app_id,
                "allowed_tools": self.body.allowed_tools,
                "knowledge_scope": self.body.knowledge_scope,
            }),
        };
        if let Err(e) = recorder.record(ctx, entry).await {
            tracing::warn!(error = %e, "skill.apply の監査記録に失敗");
        }
    }
}

/// artifact 層のエラーを chat のエラーへ写す（fail-closed の文言つき）。
fn map_skill_err(e: artifact::ArtifactError) -> ChatError {
    match e {
        artifact::ArtifactError::Forbidden => ChatError::Forbidden,
        artifact::ArtifactError::NotFound => ChatError::Invalid(
            "ピンされた skill が見つかりません（削除または共有解除された可能性）".into(),
        ),
        other => ChatError::Internal(format!("skill 読込: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{ToolError, ToolOutcome};

    fn skill(body: serde_json::Value) -> AppliedSkill {
        AppliedSkill {
            id: Uuid::new_v4(),
            version: 1,
            name: "test-skill".into(),
            body: gui::validate_skill_body(&body).unwrap(),
        }
    }

    fn full_body() -> serde_json::Value {
        serde_json::json!({
            "description": "テスト",
            "instructions": "規程に基づき回答する。",
            "allowed_tools": ["doc_search", "shell"],
            "model": { "model": "skill-model", "temperature": 0.3, "max_tokens": 512 },
            "few_shot": [ { "user": "Q", "assistant": "A" } ],
            "knowledge_scope": { "folders": [Uuid::new_v4()], "files": [] }
        })
    }

    /// テスト用の名前だけのツール。
    struct NamedTool(&'static str, bool);

    #[async_trait::async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &'static str {
            "test"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn requires_confirmation(&self) -> bool {
            self.1
        }
        async fn call(
            &self,
            _ctx: &AuthContext,
            _input: serde_json::Value,
            _trace_id: Option<&str>,
        ) -> Result<ToolOutcome, ToolError> {
            Ok(ToolOutcome::ok("ok"))
        }
    }

    fn run_with_pin(skill_id: Option<Uuid>) -> ClaimedRun {
        ClaimedRun {
            run_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            message_id: Uuid::new_v4(),
            tenant_id: "t".into(),
            org: "o".into(),
            actor: "alice".into(),
            agent_mode: false,
            fencing_token: 1,
            cancel_requested: false,
            trace_id: None,
            autonomous: false,
            skill_id,
            skill_version: skill_id.map(|_| 1),
            mini_app_id: None,
            mini_app_version: None,
        }
    }

    fn test_ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t".into()),
            },
            "o".into(),
            "t".into(),
        )
    }

    /// run のピンから load する経路の fail-closed（未配線はエラー・skill 無しで生成しない）。
    #[tokio::test]
    async fn load_fails_closed_when_artifacts_unwired() {
        let ctx = test_ctx();
        let err = AppliedSkill::load(&ctx, None, &run_with_pin(Some(Uuid::new_v4())), None)
            .await
            .expect_err("未配線でピンがあるなら失敗する");
        assert!(matches!(err, ChatError::Unavailable(_)));

        // ピンが無ければ None（従来挙動）。
        assert!(AppliedSkill::load(&ctx, None, &run_with_pin(None), None)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn apply_reflects_system_few_shot_model_and_tools() {
        let s = skill(full_body());
        // system 追記。
        let mut system = "base".to_string();
        s.apply_system(&mut system);
        assert!(system.starts_with("base"));
        assert!(system.contains("規程に基づき回答する。"));

        // few-shot は履歴先頭へ user/assistant 対で入る。
        let mut history = vec![LlmMessage::text(LlmRole::User, "本題".to_string())];
        s.apply_few_shot(&mut history);
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].role, LlmRole::User);
        assert_eq!(history[1].role, LlmRole::Assistant);

        // モデル既定（指定があるものだけ上書き）。
        let mut opts = AgentOptions::chat(4);
        s.apply_model_defaults(&mut opts);
        assert_eq!(opts.model.as_deref(), Some("skill-model"));
        assert_eq!(opts.temperature, Some(0.3));
        assert_eq!(opts.max_tokens, Some(512));

        // 許可ツールで縮小（emit_ui は許可外なので消える・doc_search は残る）。
        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(NamedTool("doc_search", false)),
            Arc::new(NamedTool("emit_ui", false)),
            Arc::new(NamedTool("shell", true)),
        ];
        s.filter_tools(&mut tools);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["doc_search", "shell"]);
    }

    /// skill は承認ポリシに触れない（破壊系の明示許可を無効化できない・Task 6.9）。
    #[test]
    fn apply_never_relaxes_approval_policy() {
        let s = skill(full_body());
        let mut opts = AgentOptions::chat(4);
        assert!(!opts.approval.is_pre_authorized("shell"));
        s.apply_model_defaults(&mut opts);
        // allowed_tools に shell を含めても、承認ポリシは deny_all のまま。
        assert!(
            !opts.approval.is_pre_authorized("shell"),
            "skill 適用で破壊系ツールが事前許可に化けない"
        );
    }
}
