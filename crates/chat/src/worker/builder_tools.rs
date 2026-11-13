//! 作成系の道具（D-10 の閉ループ）と、それに結び付く予算の拡張・チェックポイントの保存の条件（T03）。
//!
//! - [`EXTEND_ON`]: 初めて呼んだら、通常の会話の予算を [`super::AppBuilderConfig`] の値まで 1 回だけ広げる
//!   （D-53 の HD-4 (c)・D-204）。広げたかどうかは agent-core が毎ステップ履歴から導く。
//! - [`CHECKPOINT_ON`]: 呼んだステップの終わりから、run のチェックポイントを保存し始める（R1）。429 などで
//!   run を頭からやり直すと、作成や試運転が二重に走る。境界から再開すれば、済んだ呼び出しは走らない。
//!
//! どちらも語彙（[`ToolName`]）から作る。名前の文字列を手で持たない（codegen が正）。

use std::sync::Arc;

use agent_core::{AgentOptions, BudgetExtension, Tool, ToolName};

use super::AppBuilderConfig;

/// 初めて呼んだら予算を 1 回だけ広げる道具。`miniapp_create` は T08 が足す。
pub(super) const EXTEND_ON: &[ToolName] = &[ToolName::EmitWorkflow, ToolName::DataTableCreate];

/// 呼んだステップの終わりからチェックポイントを保存し始める道具。[`EXTEND_ON`] に試運転（`try_workflow`）を
/// 足したもの（作成をしない試運転も、二重に走らせない）と、テーブルの改訂（T05）。`data_view_save`・
/// `miniapp_update` は後の担当が足す。
pub(super) const CHECKPOINT_ON: &[ToolName] = &[
    ToolName::EmitWorkflow,
    ToolName::DataTableCreate,
    ToolName::TryWorkflow,
    ToolName::DataTableUpdate,
];

/// `name` の道具を呼んだら、チェックポイントの保存を始めるか。
pub(super) fn starts_checkpoints(name: &str) -> bool {
    CHECKPOINT_ON.iter().any(|t| t.as_str() == name)
}

/// 通常の会話（Chat プロファイル）に予算の拡張を付ける（HD-4 (c)・D-204）。
///
/// 自律は元から大きい予算で回るので付けない。effort と system は途中で変えない（キャッシュと一貫性・7p-9）。
/// `offered` は提示の最終形（数えるのはそこにある道具だけ）。
pub(super) fn extend_on_builder_tools(
    opts: &mut AgentOptions,
    config: &AppBuilderConfig,
    offered: &[Arc<dyn Tool>],
) {
    if !opts.profile.is_autonomous() {
        opts.budget_extension = budget_extension(config, offered);
    }
}

/// 通常の会話に付ける予算の拡張。数えるのは、この run に提示した [`EXTEND_ON`] の道具だけ（提示していない
/// 名前の呼び出しでは広げない）。1 つも提示していなければ `None`。
pub(super) fn budget_extension(
    config: &AppBuilderConfig,
    offered: &[Arc<dyn Tool>],
) -> Option<BudgetExtension> {
    let on_tools: Vec<ToolName> = EXTEND_ON
        .iter()
        .copied()
        .filter(|t| offered.iter().any(|o| o.name() == t.as_str()))
        .collect();
    (!on_tools.is_empty()).then_some(BudgetExtension {
        on_tools,
        max_steps: config.max_steps,
        max_fresh_tokens: config.max_fresh_tokens,
        max_cost_usd_micros: config.max_cost_usd_micros,
        max_response_tokens: config.max_response_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{ToolError, ToolOutcome};
    use authz::AuthContext;

    struct Named(&'static str);

    #[async_trait::async_trait]
    impl Tool for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "テストの道具"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        async fn call(
            &self,
            _ctx: &AuthContext,
            _input: serde_json::Value,
            _trace_id: Option<&str>,
        ) -> Result<ToolOutcome, ToolError> {
            Ok(ToolOutcome::ok(""))
        }
    }

    /// 保存を始める道具は、広げる道具をすべて含み、試運転を足したもの。
    #[test]
    fn checkpoints_start_on_every_extending_tool_and_on_trials() {
        for t in EXTEND_ON {
            assert!(CHECKPOINT_ON.contains(t), "{t:?}");
            assert!(starts_checkpoints(t.as_str()));
        }
        assert!(starts_checkpoints("try_workflow"));
        // テーブルの改訂は広げないが、二重に直さないよう境界から保存する（T05）。
        assert!(starts_checkpoints("data_table_update"));
        assert!(!EXTEND_ON.contains(&ToolName::DataTableUpdate));
        assert!(EXTEND_ON.contains(&ToolName::DataTableCreate));
        assert!(
            !EXTEND_ON.contains(&ToolName::TryWorkflow),
            "試運転では広げない"
        );
        for plain in [
            "doc_search",
            "read_workflow",
            "list_workflows",
            "tool_search",
            "",
        ] {
            assert!(!starts_checkpoints(plain), "{plain}");
        }
    }

    /// 通常の会話にだけ付ける（自律は元から大きい予算で回る）。
    #[test]
    fn only_plain_chat_runs_get_the_extension() {
        let config = AppBuilderConfig::default();
        let builder: Vec<Arc<dyn Tool>> = vec![Arc::new(Named("emit_workflow"))];
        let mut chat = AgentOptions::chat(6);
        extend_on_builder_tools(&mut chat, &config, &builder);
        assert!(chat.budget_extension.is_some());
        let mut autonomous = AgentOptions::autonomous(50, None, 2_500_000, 8_000_000);
        extend_on_builder_tools(&mut autonomous, &config, &builder);
        assert!(autonomous.budget_extension.is_none());
        // 思考強度と system は変えない。
        assert_eq!(chat.effort, None);
        assert_eq!(chat.system, None);
    }

    /// 提示した作成系の道具だけを数える。1 つも提示していなければ広げない。値は設定どおり。
    #[test]
    fn extension_counts_only_offered_builder_tools() {
        let config = AppBuilderConfig::default();
        let plain: Vec<Arc<dyn Tool>> = vec![Arc::new(Named("doc_search"))];
        assert_eq!(budget_extension(&config, &plain), None);
        assert_eq!(budget_extension(&config, &[]), None);
        let builder: Vec<Arc<dyn Tool>> = vec![
            Arc::new(Named("doc_search")),
            Arc::new(Named("emit_workflow")),
            Arc::new(Named("try_workflow")),
        ];
        assert_eq!(
            budget_extension(&config, &builder),
            Some(BudgetExtension {
                on_tools: vec![ToolName::EmitWorkflow],
                max_steps: 24,
                max_fresh_tokens: 600_000,
                max_cost_usd_micros: 2_000_000,
                max_response_tokens: 32_768,
            })
        );
    }
}
