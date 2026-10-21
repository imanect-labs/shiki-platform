//! チャットの回答で ```ui ブロック（Yui）の図を書かせる指示（#530）。
//!
//! 図は回答の Markdown の一部として保存・再送され、フロントの `@imanect/yui` が描く。
//! 操作を持つ種類（app・ask・plan）のボタンは、回答の確定時にここでワークフローの束縛を
//! 解決し（[`ChatWorker::yui_bindings`]）、押されたときはサーバーが本文を読み直して検証する
//! （`store::yui_actions`・#604）。

/// 書き方の説明（評価で最も正確だった版をもとにしたもの）。
const GUIDE: &str = include_str!("yui_guide.md");

impl super::ChatWorker {
    /// 本文の図のワークフローのボタン（`-> workflow 名前`）を、**回答した主体**の権限で解決して
    /// current の版に固定する。解決できない名前（不在・権限なし）は束縛を作らない＝押せない。
    pub(super) async fn yui_bindings(
        &self,
        ctx: &authz::AuthContext,
        content: &[crate::model::ContentBlock],
    ) -> Vec<crate::yui_actions::UiWorkflowBinding> {
        let buttons = crate::yui_actions::workflow_buttons(content);
        let Some(resolver) = self
            .workflow_resolver
            .as_ref()
            .filter(|_| !buttons.is_empty())
        else {
            return Vec::new();
        };
        let mut pins: std::collections::HashMap<String, Option<gui::WorkflowPin>> =
            std::collections::HashMap::new();
        let mut out = Vec::new();
        for (action_id, name) in buttons {
            if !pins.contains_key(&name) {
                let pin = resolver.resolve_workflow_name(ctx, &name, None).await;
                pins.insert(name.clone(), pin);
            }
            let Some(Some(pin)) = pins.get(&name) else {
                continue;
            };
            let (Some(workflow_id), Some(version)) = (pin.artifact_id, pin.version) else {
                continue;
            };
            out.push(crate::yui_actions::UiWorkflowBinding {
                action_id,
                workflow_id,
                version,
                workflow_name: pin.name.clone().unwrap_or(name),
            });
        }
        out
    }
}

/// システムプロンプトの末尾に図の書き方を足す。
pub fn with_yui_guide(base: &str) -> String {
    format!("{}\n\n{}", base.trim_end(), GUIDE.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_guide_after_base_prompt() {
        let out = with_yui_guide("あなたはアシスタントです。\n");
        assert!(out.starts_with("あなたはアシスタントです。\n\n# 図の書き方"));
        // 5 種類すべての書き方を含み、撤去した emit_ui には触れない（#604）。
        for kind in ["```ui", "bar", "line", "stat", "flow", "stack"] {
            assert!(out.contains(kind), "{kind} が無い");
        }
        assert!(!out.contains("emit_ui"));
    }
}
