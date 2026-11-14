use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use shiki_bug_report::{BugReportService, Category, HelpRun, ReportContent};
use std::sync::Arc;
use uuid::Uuid;

pub(super) struct BugReportDraftTool {
    pub service: Arc<BugReportService>,
    pub binding: HelpRun,
    pub case_id: Uuid,
}
#[async_trait::async_trait]
impl Tool for BugReportDraftTool {
    fn name(&self) -> &str {
        ToolName::BugReportDraft.as_str()
    }
    fn description(&self) -> &'static str {
        "本人がヘルプ案内と報告希望を確認した問題の下書きだけを作る。症状・試した手順・期待動作を短く要約する。会話全文や社内文書を含めない。送信は本人が確認画面で行う。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{
            "title":{"type":"string","minLength":1,"maxLength":200},
            "symptom":{"type":"string","minLength":1,"maxLength":2000},
            "steps_tried":{"type":"string","maxLength":2000},
            "expected_behavior":{"type":"string","maxLength":2000}},
            "required":["title","symptom","steps_tried","expected_behavior"],"additionalProperties":false})
    }
    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            title: String,
            symptom: String,
            steps_tried: String,
            expected_behavior: String,
        }
        let input: Input = serde_json::from_value(input)
            .map_err(|_| ToolError::Invalid("要約の項目が不正です".into()))?;
        // 提示後に本人が解決へ変更した場合や、枝・権限・run が変化した場合も再検査する。
        if self
            .service
            .available_help_case(ctx, &self.binding)
            .await
            .ok()
            != Some(self.case_id)
        {
            return Err(ToolError::Invalid(
                "ヘルプ案内と本人の確認が必要です".into(),
            ));
        }
        let report = self.service.create_ai_draft(ctx, &self.binding, self.case_id, ReportContent {
            category: Category::Bug, title: input.title, symptom: input.symptom,
            steps_tried: input.steps_tried, expected_behavior: input.expected_behavior,
        }, trace.unwrap_or("bug-report-ai")).await.map_err(|_| ToolError::Invalid("下書きを作成できません。ヘルプ案内と確認状態、組織の共有設定を確認してください".into()))?;
        let mut out = ToolOutcome::ok("本人用の報告下書きを作成しました。確認画面で内容を確認して送信してください。まだ送信していません。");
        out.bug_report_draft = Some((report.id.to_string(), report.rev));
        Ok(out)
    }
}
