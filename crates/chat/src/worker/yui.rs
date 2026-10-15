//! チャットの回答で ```ui ブロック（Yui）の図を書かせる指示（#530）。
//!
//! 図は回答の Markdown の一部として保存・再送され、フロントの `@imanect/yui` が描く。
//! `emit_ui`（検証済み spec）と違い操作・URL・HTML を持たない表示専用の宣言なので、
//! サーバ側の検証は通さない（design §4.7）。

/// 書き方の説明（評価で最も正確だった版をもとに、`emit_ui` との使い分けを足したもの）。
const GUIDE: &str = include_str!("yui_guide.md");

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
        // 20 種類すべての書き方と、emit_ui との使い分けを含む。
        for kind in [
            "```ui",
            "bar",
            "line",
            "pie",
            "scatter",
            "stat",
            "flow",
            "stack",
            "timeline",
            "steps",
            "tree",
            "cards",
            "progress",
            "funnel",
            "quadrant",
            "gantt",
            "calc",
            "compare",
            "proscons",
            "swot",
            "checklist",
            "emit_ui",
        ] {
            assert!(out.contains(kind), "{kind} が無い");
        }
    }
}
