//! チャットの回答で ```ui ブロック（Yui）の図を書かせる指示（#530）。
//!
//! 図は回答の Markdown の一部として保存・再送され、フロントの `@imanect/yui` が描く。
//! 操作を持つ種類（app・ask・plan）のボタンは、回答の確定時にここでワークフローの束縛を
//! 解決し（[`ChatWorker::yui_bindings`]）、押されたときはサーバーが本文を読み直して検証する
//! （`store::yui_actions`・#604）。

/// 回答の中で図と UI をどう使うかの shiki 側の決まり（手書き。種類の説明は書かない）。
const HOST_GUIDE: &str = include_str!("yui_guide.md");

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

/// システムプロンプトの末尾に、図と UI の使い方を足す。
///
/// shiki 側の決まり → 共通の決まり → 種類の早見の順。種類ごとの書き方は載せず、モデルが
/// `ui_reference` で読む（早見は常に載せる。無いとどの種類を使うかの判断が落ちる・y-ui の評価）。
pub fn with_yui_guide(base: &str) -> String {
    let reference = &*super::yui_reference::REFERENCE;
    format!(
        "{}\n\n{}\n\n{}\n\n種類の早見:\n{}",
        base.trim_end(),
        HOST_GUIDE.trim_end(),
        reference.rules.trim_end(),
        reference.index.trim_end()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_guide_after_base_prompt() {
        let out = with_yui_guide("あなたはアシスタントです。\n");
        assert!(out.starts_with("あなたはアシスタントです。\n\n# 回答の中の図と UI"));
        // 早見は全種類（ask・plan を除く）を載せ、円グラフも描けると伝わる。
        for kind in ["`bar`", "`pie`", "`calc`", "`app`", "`compare`", "`map`"] {
            assert!(out.contains(kind), "{kind} が無い");
        }
        assert!(out.contains("種類の早見:\nグラフ:"));
        assert!(out.contains("ui_reference"));
        // 種類ごとの書き方（作例）は載せない（ui_reference で読む）。
        assert!(!out.contains("```ui\npie "));
        assert!(!out.contains("emit_ui"));
        assert!(!out.contains("描けない"));
    }
}
