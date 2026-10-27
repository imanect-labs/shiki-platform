//! ```ui ブロック（Yui）の種類の早見と、種類ごとの書き方を返す `ui_reference` ツール。
//!
//! 説明は `@imanect/yui/guide` から生成した `yui_reference.json`（`cd web && pnpm gen:yui`）を
//! 読む。shiki 側で種類の説明を手書きしない（手書きの説明は 5 種類のまま追随が止まり、
//! 円グラフを「描けない」と答えていた）。system プロンプトには早見と共通の決まりだけを置き、
//! 書き方はモデルが図を書く前にこのツールで読む（y-ui の評価の方式 H・I）。

use std::collections::BTreeMap;
use std::sync::LazyLock;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use serde::Deserialize;

/// 生成物（`web/scripts/gen-yui-reference.mjs`）。
const REFERENCE_JSON: &str = include_str!("yui_reference.json");

/// 1 回に読める種類の数（早見から選んだ数種類を 1 往復で読めれば足りる）。
const MAX_KINDS: usize = 5;

/// `@imanect/yui/guide` から生成した説明。
#[derive(Debug, Default, Deserialize)]
pub(super) struct YuiReference {
    /// 生成元の `@imanect/yui` の版（web の依存と一致することをテストで確かめる）。
    pub(super) yui_version: String,
    /// どの種類にも当てはまる書き方の決まり。
    pub(super) rules: String,
    /// 種類の早見（ask・plan は載せない）。
    pub(super) index: String,
    /// 種類ごとの書き方（全種類）。
    pub(super) kinds: BTreeMap<String, String>,
}

/// 読めなければ空で続ける（早見の無いプロンプトとツールの誤りになるだけで、生成は止めない）。
/// 生成物が読めることは単体テストが保証する。
pub(super) static REFERENCE: LazyLock<YuiReference> = LazyLock::new(
    || match serde_json::from_str::<YuiReference>(REFERENCE_JSON) {
        Ok(reference) => {
            tracing::debug!(yui = %reference.yui_version, "```ui の説明を読み込んだ");
            reference
        }
        Err(e) => {
            tracing::error!(error = %e, "yui_reference.json を読めない（cd web && pnpm gen:yui で作り直す）");
            YuiReference::default()
        }
    },
);

/// ツールを渡さない経路（classic）の system に足す、早見に載せた種類すべての書き方。
///
/// system の決まりは「ui_reference で読む」と言うので、ここで「読めないので次に全部載せる」と
/// 上書きする。ask・plan は早見に無い（門が指示する）ので載せない。
pub(super) fn classic_kind_guides() -> String {
    let reference = &*REFERENCE;
    let guides = reference
        .kinds
        .iter()
        .filter(|(kind, _)| reference.index.contains(&format!("`{kind}`")))
        .map(|(_, guide)| guide.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    format!(
        "\n\n# 種類ごとの書き方\n\
         この会話では ui_reference を使えないので、早見の種類の書き方を次に載せる（読みに行かずにこれに従う）。\n\n{guides}"
    )
}

/// 種類ごとの書き方を返すツール（読み取りのみ・外部への入出力なし）。
pub(super) struct UiReferenceTool;

#[derive(Debug, Deserialize)]
struct Input {
    kinds: Vec<String>,
}

/// 指定した種類の書き方をつなげる。知らない種類は、使える種類の一覧を添えて知らせる。
fn lookup(reference: &YuiReference, kinds: &[String]) -> String {
    let mut seen: Vec<String> = Vec::new();
    for kind in kinds {
        let kind = kind.trim().to_lowercase();
        if !seen.contains(&kind) {
            seen.push(kind);
        }
    }
    seen.iter()
        .map(|k| match reference.kinds.get(k) {
            Some(guide) => guide.clone(),
            None => format!(
                "不明な種類: {k}（使える種類: {}）",
                reference
                    .kinds
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[async_trait::async_trait]
impl Tool for UiReferenceTool {
    fn name(&self) -> &str {
        ToolName::UiReference.as_str()
    }
    fn description(&self) -> &'static str {
        "```ui ブロックの種類ごとの書き方（用途・書き方・オプション・上限・使い分け・作例）を返す。\
         図を書く前に、使う種類を指定して呼ぶ。"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "kinds": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": MAX_KINDS,
                    "description": "読む種類（早見の英字の名前。例: [\"pie\", \"calc\"]）"
                }
            },
            "required": ["kinds"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        _ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: Input = serde_json::from_value(input)
            .map_err(|e| ToolError::Invalid(format!("入力が不正です: {e}")))?;
        if input.kinds.is_empty() || input.kinds.len() > MAX_KINDS {
            return Err(ToolError::Invalid(format!(
                "kinds には種類の名前を 1〜{MAX_KINDS} 個渡してください"
            )));
        }
        Ok(ToolOutcome::ok(lookup(&REFERENCE, &input.kinds)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_reference_covers_every_kind() {
        let r = &*REFERENCE;
        assert!(!r.yui_version.is_empty(), "yui_reference.json が読めない");
        assert!(r.rules.contains("1 行目は「種類 タイトル」"));
        for kind in [
            "bar", "line", "pie", "stat", "flow", "stack", "calc", "app", "map",
        ] {
            assert!(r.kinds.contains_key(kind), "{kind} の書き方が無い");
            assert!(
                r.index.contains(&format!("`{kind}`")),
                "{kind} が早見に無い"
            );
        }
        // 質問・計画のカードは門が指示するときだけ使うので、早見に載せない（書き方は読める）。
        assert!(!r.index.contains("`ask`") && !r.index.contains("`plan`"));
        assert!(r.kinds.contains_key("ask") && r.kinds.contains_key("plan"));
    }

    /// web が描く `@imanect/yui` と、説明の生成元の版が一致する（上げたら pnpm gen:yui）。
    #[test]
    fn yui_reference_matches_web_dependency() {
        let pkg: serde_json::Value =
            serde_json::from_str(include_str!("../../../../web/package.json")).unwrap();
        let dep = pkg["dependencies"]["@imanect/yui"].as_str().unwrap();
        assert_eq!(
            dep, REFERENCE.yui_version,
            "web/package.json の @imanect/yui を変えたら `cd web && pnpm gen:yui` で説明を作り直す"
        );
    }

    /// classic（ツール無し）には、早見の種類すべての書き方を載せ、ask・plan は載せない。
    #[test]
    fn classic_kind_guides_cover_index_kinds_only() {
        let out = classic_kind_guides();
        assert!(out.starts_with("\n\n# 種類ごとの書き方\n"));
        for kind in ["bar", "pie", "calc", "stack", "compare"] {
            assert!(out.contains(&format!("## {kind} ")), "{kind} が無い");
        }
        assert!(!out.contains("## ask ") && !out.contains("## plan "));
    }

    #[test]
    fn lookup_returns_guides_and_lists_unknown_kinds() {
        let out = lookup(
            &REFERENCE,
            &["pie".into(), " Calc ".into(), "pie".into(), "donut".into()],
        );
        assert!(out.starts_with("## pie 円グラフ"));
        assert_eq!(out.matches("## pie ").count(), 1, "重複は 1 回だけ返す");
        assert!(out.contains("## calc 計算機"));
        assert!(out.contains("```ui\npie "), "作例を含む");
        assert!(out.contains("不明な種類: donut（使える種類: app, ask, bar"));
    }

    #[tokio::test]
    async fn call_validates_kinds_count() {
        let ctx = AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("default".into()),
            },
            "acme".into(),
            "default".into(),
        );
        let ok = UiReferenceTool
            .call(&ctx, serde_json::json!({ "kinds": ["bar"] }), None)
            .await
            .unwrap();
        assert!(!ok.is_error);
        assert!(ok.content.contains("## bar 棒グラフ"));
        for bad in [
            serde_json::json!({ "kinds": [] }),
            serde_json::json!({ "kinds": ["a", "b", "c", "d", "e", "f"] }),
            serde_json::json!({}),
        ] {
            assert!(matches!(
                UiReferenceTool.call(&ctx, bad, None).await,
                Err(ToolError::Invalid(_))
            ));
        }
        assert!(UiReferenceTool.is_read_only());
    }
}
