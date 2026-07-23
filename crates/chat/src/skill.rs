//! skill のチャット適用（Task 6.9 → #344 Task 10.11 で一般化）。
//!
//! ピンされた skill（複数可・順序付き）を**発話ユーザーの権限**で読み（ミニアプリ経由は
//! バンドル権限チョークポイント）、system 指示文・few-shot・モデル既定・知識スコープを生成へ
//! 反映する。`allowed_tools` は「そのスキルが使うツールの宣言＝モデルへの誘導」であり、
//! 提示ツールの縮小はしない（#344 で再定義。決定性はツール実装＋認可＋承認ゲートが担う）。
//! **承認ポリシ（破壊系の明示許可・Task 3.9）には一切触れない** — skill はそれを無効化できない。
//!
//! fail-closed: ピンがあるのに読めない（共有剥奪・削除・未配線）場合は run を失敗させる
//! （skill 無しで黙って生成しない）。途中発動（skill ツール・[`crate::skill_tool`]）も同じ
//! 解決経路（[`AppliedSkill::resolve`]）を通る。

use std::sync::Arc;

use agent_core::AgentOptions;
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
    /// skill 1 件を解決する（発話ユーザー権限・fail-closed）。
    ///
    /// `via_bundle` が Some のとき（ミニアプリ経由のセッション）はバンドル権限で読む
    /// （部品の個別共有は不要・Task 6.10）。単体は本人の viewer で読む。
    /// 剥奪・削除は Forbidden/NotFound = 呼び出し元で失敗（黙って続行しない）。
    pub(crate) async fn resolve(
        ctx: &AuthContext,
        artifacts: &Arc<artifact::ArtifactStore>,
        skill_id: Uuid,
        skill_version: i64,
        via_bundle: Option<Uuid>,
        trace_id: Option<&str>,
    ) -> Result<AppliedSkill, ChatError> {
        let (meta_name, version_body) = if let Some(bundle_id) = via_bundle {
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
        Ok(AppliedSkill {
            id: skill_id,
            version: skill_version,
            name: meta_name,
            body,
        })
    }

    /// run のピン（複数可）を順に解決する（発話ユーザー権限・fail-closed）。
    pub(crate) async fn load_pins(
        ctx: &AuthContext,
        artifacts: Option<&Arc<artifact::ArtifactStore>>,
        run: &ClaimedRun,
        trace_id: Option<&str>,
    ) -> Result<Vec<AppliedSkill>, ChatError> {
        if run.skill_pins.0.is_empty() {
            return Ok(Vec::new());
        }
        let Some(artifacts) = artifacts else {
            return Err(ChatError::Unavailable(
                "skill が指定されていますが artifact ストアが未配線です".into(),
            ));
        };
        let via_bundle = run.mini_app_id.filter(|_| run.mini_app_version.is_some());
        let mut out = Vec::with_capacity(run.skill_pins.0.len());
        for pin in &run.skill_pins.0 {
            out.push(
                AppliedSkill::resolve(
                    ctx,
                    artifacts,
                    pin.skill_id,
                    pin.skill_version,
                    via_bundle,
                    trace_id,
                )
                .await?,
            );
        }
        Ok(out)
    }

    /// system プロンプトへ SKILL.md 指示文（＋ツール誘導）を追記する。
    pub(crate) fn apply_system(&self, system: &mut String) {
        system.push_str("\n\n# Skill: ");
        system.push_str(&self.name);
        system.push('\n');
        system.push_str(&self.body.instructions);
        if let Some(guidance) = self.tool_guidance() {
            system.push_str("\n\n");
            system.push_str(&guidance);
        }
    }

    /// `allowed_tools` を「このスキルが使うツール」の誘導テキストへ写す（#344 で再定義）。
    ///
    /// 旧: 提示ツールの縮小（retain）。途中発動では過去ターンに遡及できないため、宣言＝誘導へ
    /// 一本化した。厳密な隔離が要るスキルはサブエージェント実行（別 issue）。None は誘導なし。
    pub(crate) fn tool_guidance(&self) -> Option<String> {
        let allowed = self.body.allowed_tools.as_ref()?;
        let names: Vec<&str> = allowed.iter().map(|t| t.as_str()).collect();
        Some(format!(
            "このスキルは次のツールを使う想定です: {}",
            names.join(", ")
        ))
    }

    /// skill ツールの観測テキスト（途中発動で instructions を読み込んだ結果・#344）。
    pub(crate) fn loaded_content(&self) -> String {
        let mut out = format!("# Skill: {}\n\n{}", self.name, self.body.instructions);
        if let Some(guidance) = self.tool_guidance() {
            out.push_str("\n\n");
            out.push_str(&guidance);
        }
        if self.body.knowledge_scope.is_some() {
            // doc_search のスコープはツール構築時に固定されるため、途中発動では遡及しない。
            out.push_str(
                "\n\n（注: このスキルの知識スコープは途中読み込みでは検索絞り込みに反映されません。\
                 スレッドにピンすると反映されます。）",
            );
        }
        out
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

    /// モデル/パラメータ既定を反映する（指定があるものだけ上書き・複数ピンは適用順の後勝ち）。
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

    /// 指定ツールが宣言集合に含まれるか（None は全許可・classic の事前検索スキップ用）。
    pub(crate) fn allows(&self, name: &str) -> bool {
        self.body
            .allowed_tools
            .as_ref()
            .is_none_or(|allowed| allowed.iter().any(|t| t.as_str() == name))
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

/// 複数ピンの実効検索スコープ（doc_search の絞り込み・#344 の合成規則）。
///
/// **全ピンが scope を持つ場合のみ** union で絞る。1 つでも scope 無し（=全可読範囲）の
/// スキルがあれば絞らない（束の join・スキルは能力を増やさないが、他スキルの範囲も奪わない）。
pub(crate) fn combined_scope(skills: &[AppliedSkill]) -> Option<rag::SearchScope> {
    if skills.is_empty() {
        return None;
    }
    let mut folders = Vec::new();
    let mut files = Vec::new();
    for s in skills {
        let scope = s.body.knowledge_scope.as_ref()?;
        folders.extend(scope.folders.iter().copied());
        files.extend(scope.files.iter().copied());
    }
    folders.sort_unstable();
    folders.dedup();
    files.sort_unstable();
    files.dedup();
    Some(rag::SearchScope { folders, files })
}

/// 複数ピンの実効モデル既定（classic 経路用・指定があるフィールドだけ適用順の後勝ち）。
pub(crate) fn combined_model_defaults(skills: &[AppliedSkill]) -> Option<gui::ModelDefaults> {
    let mut acc: Option<gui::ModelDefaults> = None;
    for s in skills {
        let Some(m) = &s.body.model else { continue };
        let e = acc.get_or_insert(gui::ModelDefaults {
            model: None,
            temperature: None,
            max_tokens: None,
        });
        if m.model.is_some() {
            e.model.clone_from(&m.model);
        }
        if m.temperature.is_some() {
            e.temperature = m.temperature;
        }
        if m.max_tokens.is_some() {
            e.max_tokens = m.max_tokens;
        }
    }
    acc
}

/// artifact 層のエラーを chat のエラーへ写す（fail-closed の文言つき）。
pub(crate) fn map_skill_err(e: artifact::ArtifactError) -> ChatError {
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
    use crate::model::SkillPin;
    use sqlx::types::Json;

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

    fn run_with_pins(pins: Vec<SkillPin>) -> ClaimedRun {
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
            skill_pins: Json(pins),
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
        let pin = SkillPin {
            skill_id: Uuid::new_v4(),
            skill_version: 1,
        };
        let err = AppliedSkill::load_pins(&ctx, None, &run_with_pins(vec![pin]), None)
            .await
            .expect_err("未配線でピンがあるなら失敗する");
        assert!(matches!(err, ChatError::Unavailable(_)));

        // ピンが無ければ空（従来挙動）。
        assert!(
            AppliedSkill::load_pins(&ctx, None, &run_with_pins(vec![]), None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn apply_reflects_system_few_shot_and_model() {
        let s = skill(full_body());
        // system 追記（instructions＋ツール誘導）。
        let mut system = "base".to_string();
        s.apply_system(&mut system);
        assert!(system.starts_with("base"));
        assert!(system.contains("規程に基づき回答する。"));
        assert!(
            system.contains("doc_search, shell"),
            "allowed_tools は誘導テキストとして system に載る（縮小はしない・#344）"
        );

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

    /// 複数ピンの合成規則: scope は全ピンが持つ時のみ union、model は後勝ち。
    #[test]
    fn combined_scope_and_model_defaults() {
        let f1 = Uuid::new_v4();
        let f2 = Uuid::new_v4();
        let a = skill(serde_json::json!({
            "description": "a", "instructions": "A",
            "model": { "model": "model-a", "temperature": 0.1 },
            "knowledge_scope": { "folders": [f1], "files": [] }
        }));
        let b = skill(serde_json::json!({
            "description": "b", "instructions": "B",
            "model": { "max_tokens": 256 },
            "knowledge_scope": { "folders": [f2], "files": [] }
        }));
        // 両方 scope 持ち → union。
        let scope = combined_scope(&[a, b]).expect("union で絞る");
        assert_eq!(scope.folders.len(), 2);

        // 片方 scope 無し → narrow しない。
        let a = skill(serde_json::json!({
            "description": "a", "instructions": "A",
            "knowledge_scope": { "folders": [f1], "files": [] }
        }));
        let c = skill(serde_json::json!({ "description": "c", "instructions": "C" }));
        assert!(combined_scope(&[a, c]).is_none());

        // model 既定は指定フィールドのみ後勝ち。
        let a = skill(serde_json::json!({
            "description": "a", "instructions": "A",
            "model": { "model": "model-a", "temperature": 0.1 }
        }));
        let b = skill(serde_json::json!({
            "description": "b", "instructions": "B",
            "model": { "max_tokens": 256 }
        }));
        let m = combined_model_defaults(&[a, b]).expect("既定あり");
        assert_eq!(m.model.as_deref(), Some("model-a"));
        assert_eq!(m.temperature, Some(0.1));
        assert_eq!(m.max_tokens, Some(256));
        assert!(combined_model_defaults(&[]).is_none());
    }
}
