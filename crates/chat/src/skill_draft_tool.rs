//! AI によるスキルの下書き作成/修正ツール（save_skill・#919）。
//!
//! save_note / save_slide / save_csv と同型の**下書き確定型**: モデルが出したスキル定義を
//! **保存 API と同一の検証**（[`gui::validate_skill_body`]）に通し、
//! - 通れば `{name, skill}` を skill_draft として外部化する（フロントがスキルエディタへ流し込む）
//! - 通らなければ検証エラー**全件**を is_error の tool_result で返し、モデルに自己修正させる
//!
//! artifact へは保存しない（保存はエディタで人が行い、保存 API の検証と認可を通る）。
//! スキルは能力を増やさない（許可ツールは誘導・知識スコープは絞り込みのみ）ため承認ゲートは不要。

use std::fmt::Write as _;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use serde_json::{json, Map, Value};

/// スキル名の最大文字数（artifact の参照名の上限と同じ）。
const MAX_NAME_CHARS: usize = 128;

/// 入力のうち SkillBody へそのまま渡すフィールド（`command`・`references` は AI に書かせない。
/// エディタが既存値を保つ）。
const BODY_FIELDS: &[&str] = &[
    "description",
    "instructions",
    "knowledge_scope",
    "allowed_tools",
    "model",
    "few_shot",
    "scripts",
];

pub struct SaveSkillTool;

impl SaveSkillTool {
    pub fn new() -> Self {
        SaveSkillTool
    }
}

impl Default for SaveSkillTool {
    fn default() -> Self {
        Self::new()
    }
}

/// 検証エラー全件をモデルが自己修正しやすい形に整形する。
fn format_errors(errors: &[gui::GuiValidationError]) -> String {
    let mut out = format!(
        "スキルの検証に失敗しました（{} 件）。全件を直して save_skill を呼び直してください。\n",
        errors.len()
    );
    for e in errors {
        let _ = write!(out, "- [{}] {}", e.code, e.message);
        if let Some(path) = &e.path {
            let _ = write!(out, "（{path}）");
        }
        out.push('\n');
    }
    out
}

#[async_trait::async_trait]
impl Tool for SaveSkillTool {
    fn name(&self) -> &str {
        ToolName::SaveSkill.as_str()
    }
    fn description(&self) -> &'static str {
        "スキル（呼び出せる業務知識: 指示文＋説明＋許可ツール等）の下書きを用意する。ユーザーが\
         「〜のスキルを作って」「このスキルを直して」等と依頼したときに使う。呼ぶとスキルエディタが\
         開き（開いていれば内容が置き換わり）、ユーザーが確認・編集してから自分で保存する（この\
         ツールは保存しない）。直す場合は**同じ name で呼び直し**、変える項目だけを渡す\
         （渡さなかった項目はエディタの今の値のまま。既定（全ツール・システム既定のモデル・\
         全範囲）へ戻す項目は null を渡す）。instructions は渡すなら全文を書く（部分の差分は不可）。\
         新しく作るときは description と instructions を必ず渡す。instructions は SKILL.md 本文（Markdown）で、スキルを\
         使う AI への指示として書く。社内のファイル・フォルダを指すときは doc_search 等で得た\
         id を使い `[ファイル名](shiki://drive/file/<file_id>)`・\
         `[フォルダ名](shiki://drive/folder/<folder_id>)` の Markdown リンクで書く（エディタでは\
         パスのラベルとして表示され、スキルを使う AI はこの id を doc_read 等に渡せる）。\
         allowed_tools は使うツール名の宣言（例: doc_search, doc_read, web_search, web_fetch, \
         code_interpreter）で、省略すると全ツール。"
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "スキル名（例: expense-assistant）。同じ name で呼び直すと同じ下書きを更新する"
                },
                "description": {
                    "type": "string",
                    "description": "いつ使うスキルかの説明（1〜2 文。AI がスキルを選ぶ手がかりになる）"
                },
                "instructions": {
                    "type": "string",
                    "description": "SKILL.md 本文（Markdown）。スキルを使う AI への指示"
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "使うツール名の宣言（省略で全ツール。空配列は不可）"
                },
                "knowledge_scope": {
                    "type": "object",
                    "properties": {
                        "folders": { "type": "array", "items": { "type": "string" } },
                        "files": { "type": "array", "items": { "type": "string" } }
                    },
                    "description": "社内文書検索の範囲をこのフォルダ/ファイルの id に限る（省略で本人が読める全範囲）"
                },
                "model": {
                    "type": "object",
                    "properties": {
                        "model": { "type": "string" },
                        "temperature": { "type": "number" },
                        "max_tokens": { "type": "integer" }
                    },
                    "description": "モデル既定（ユーザーが指定したときだけ）"
                },
                "few_shot": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "user": { "type": "string" },
                            "assistant": { "type": "string" }
                        },
                        "required": ["user", "assistant"]
                    },
                    "description": "お手本の対話（任意・最大 8 対）"
                },
                "scripts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "scripts/<名前>.shiki または .sh" },
                            "kind": { "type": "string", "enum": ["shiki", "shell"] },
                            "source": { "type": "string" }
                        },
                        "required": ["path", "kind", "source"]
                    },
                    "description": "同梱するスクリプト（任意・ユーザーが求めたときだけ）"
                }
            },
            "required": ["name"]
        })
    }
    /// 下書きは artifact へ書かない（確定はエディタの保存）。承認ゲートは不要。
    fn requires_confirmation(&self) -> bool {
        false
    }

    async fn call(
        &self,
        _ctx: &AuthContext,
        input: Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let Value::Object(input) = input else {
            return Err(ToolError::Invalid(
                "入力はオブジェクトで指定してください".into(),
            ));
        };
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .map_or("", str::trim);
        if name.is_empty() {
            return Err(ToolError::Invalid(
                "スキル名（name）を指定してください".into(),
            ));
        }
        if name.chars().count() > MAX_NAME_CHARS || name.contains('\0') {
            return Err(ToolError::Invalid(format!(
                "スキル名は {MAX_NAME_CHARS} 文字以内で指定してください"
            )));
        }
        let mut body = Map::new();
        // 渡された項目（null も「既定へ戻す」として含む）。エディタはこれだけを置き換え、
        // 渡されなかった項目は今の値を保つ（直すたびに全体を書かせて取りこぼさせない）。
        let mut fields = Vec::new();
        for key in BODY_FIELDS {
            let Some(v) = input.get(*key) else { continue };
            fields.push(Value::String((*key).to_string()));
            if !v.is_null() {
                body.insert((*key).to_string(), v.clone());
            }
        }
        // 渡さなかった必須の文字列は、検証のためだけに仮の値を置く（エディタは置き換えない）。
        // 渡した上で空なら「必須です」を返す。
        for key in ["description", "instructions"] {
            if !fields.iter().any(|f| f == key) {
                body.insert(key.to_string(), Value::String("-".into()));
            }
        }
        if fields.is_empty() {
            return Err(ToolError::Invalid(
                "直す項目（description・instructions など）を 1 つ以上渡してください".into(),
            ));
        }
        // 保存 API と同じ検証（構造・上限・閉語彙）。通らない下書きはエディタへ流さない。
        let skill = match gui::validate_skill_body(&Value::Object(body)) {
            Ok(skill) => skill,
            Err(errors) => return Ok(ToolOutcome::error(format_errors(&errors))),
        };
        let skill = serde_json::to_value(&skill)
            .map_err(|e| ToolError::Internal(format!("skill の直列化: {e}")))?;
        let mut outcome = ToolOutcome::ok(format!(
            "下書きスキル「{name}」を用意しました。スキルエディタで内容を確認・編集し、\
             保存してください（まだ保存されていません）。"
        ));
        outcome
            .skill_drafts
            .push(json!({ "name": name, "skill": skill, "fields": fields }));
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use authz::{Principal, PrincipalKind};

    fn ctx() -> AuthContext {
        AuthContext::new(
            Principal {
                kind: PrincipalKind::User,
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

    #[tokio::test]
    async fn valid_input_becomes_a_skill_draft() {
        let tool = SaveSkillTool::new();
        assert!(!tool.requires_confirmation(), "下書きは確認ゲート不要");
        let out = tool
            .call(
                &ctx(),
                json!({
                    "name": " expense-assistant ",
                    "description": "経費精算の質問に規程を根拠に答える",
                    "instructions": "規程 [経費規程.pdf](shiki://drive/file/0190) に基づいて答える。",
                    "allowed_tools": ["doc_search", "doc_read"],
                    "command": { "name": "x" },
                    "references": ["00000000-0000-0000-0000-000000000001"]
                }),
                None,
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.skill_drafts.len(), 1);
        let draft = &out.skill_drafts[0];
        assert_eq!(draft["name"], "expense-assistant", "name は trim する");
        assert_eq!(
            draft["skill"]["allowed_tools"],
            json!(["doc_search", "doc_read"])
        );
        assert!(draft["skill"]["instructions"]
            .as_str()
            .unwrap()
            .contains("shiki://drive/file/0190"));
        // AI に書かせない項目は落とす（エディタが既存値を保つ）。
        assert!(draft["skill"]["command"].is_null());
        assert_eq!(draft["skill"]["references"], json!([]));
        // 渡した項目だけがエディタで置き換わる。
        assert_eq!(
            draft["fields"],
            json!(["description", "instructions", "allowed_tools"])
        );
    }

    /// 直すときは変える項目だけを渡せる（渡さない項目はエディタの今の値のまま）。
    /// null は「既定へ戻す」として fields に載る。
    #[tokio::test]
    async fn partial_update_reports_only_the_fields_it_sets() {
        let tool = SaveSkillTool::new();
        let out = tool
            .call(
                &ctx(),
                json!({ "name": "x", "description": "より具体的な説明", "allowed_tools": null }),
                None,
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        let draft = &out.skill_drafts[0];
        assert_eq!(draft["fields"], json!(["description", "allowed_tools"]));
        assert_eq!(draft["skill"]["description"], "より具体的な説明");
        assert!(draft["skill"]["allowed_tools"].is_null());

        // 何も渡さない呼び出しは差し戻す。
        assert!(tool
            .call(&ctx(), json!({ "name": "x" }), None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn invalid_body_returns_all_errors_for_self_correction() {
        let tool = SaveSkillTool::new();
        let out = tool
            .call(
                &ctx(),
                json!({
                    "name": "x", "description": " ", "instructions": "", "allowed_tools": []
                }),
                None,
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.skill_drafts.is_empty(), "通らない下書きは流さない");
        for code in [
            "skill.empty_description",
            "skill.empty_instructions",
            "skill.empty_allowed_tools",
        ] {
            assert!(out.content.contains(code), "{code}: {}", out.content);
        }

        // 未知のツール名は閉語彙照合で弾く。
        let out = tool
            .call(
                &ctx(),
                json!({
                    "name": "x", "description": "d", "instructions": "i",
                    "allowed_tools": ["bogus_tool"]
                }),
                None,
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("allowed_tools"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_or_overlong_name_is_rejected() {
        let tool = SaveSkillTool::new();
        let body = |name: Value| json!({ "name": name, "description": "d", "instructions": "i" });
        assert!(tool.call(&ctx(), body(json!(" ")), None).await.is_err());
        assert!(tool.call(&ctx(), body(Value::Null), None).await.is_err());
        let long = "a".repeat(MAX_NAME_CHARS + 1);
        assert!(tool.call(&ctx(), body(json!(long)), None).await.is_err());
        assert!(tool.call(&ctx(), json!("str"), None).await.is_err());
    }
}
