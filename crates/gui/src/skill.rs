//! skill の body スキーマ＋検証（Task 6.7 / FR-7）。
//!
//! skill = SKILL.md 相当の指示文＋知識スコープ＋許可ツール＋モデル既定＋few-shot＋
//! （任意）script＋（任意）参照資料のバージョン付きアーティファクト（kind=skill）。
//! 本モジュールは**保存時検証**（構造・上限・閉語彙照合）を担い、実行面（チャット適用）は
//! chat 側が [`SkillBody`] を読んで構成する。script は保存のみ（実行は呼び出し面・Stage B）。

use agent_core::ToolName;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::validate::GuiValidationError;
use crate::vocab::vocab_enum;

/// skill body の上限（防御的リミット）。
pub mod skill_limits {
    /// SKILL.md 本文（指示文）の最大バイト数。
    pub const MAX_INSTRUCTIONS_BYTES: usize = 32 * 1024;
    /// description の最大文字数。
    pub const MAX_DESCRIPTION_CHARS: usize = 1024;
    /// few-shot 対の最大数。
    pub const MAX_FEW_SHOT: usize = 8;
    /// few-shot 1 発話の最大文字数。
    pub const MAX_FEW_SHOT_CHARS: usize = 4000;
    /// script ファイル数上限。
    pub const MAX_SCRIPTS: usize = 16;
    /// script 1 ファイルの最大バイト数。
    pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;
    /// script パスの最大文字数。
    pub const MAX_SCRIPT_PATH_CHARS: usize = 128;
    /// 知識スコープの参照数上限（フォルダ＋ファイル）。
    pub const MAX_SCOPE_REFS: usize = 100;
    /// 参照資料の数上限。
    pub const MAX_REFERENCES: usize = 50;
    /// temperature の上限（0.0..=2.0）。
    pub const MAX_TEMPERATURE: f32 = 2.0;
}

/// skill 本文（artifact kind=skill の body JSONB）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SkillBody {
    /// フロントマターの description 相当（name は artifact.name が正）。
    pub description: String,
    /// SKILL.md 本文（用途・振る舞いを書く指示文。システムプロンプトへ注入される）。
    pub instructions: String,
    /// 知識スコープ（RAG 参照範囲の限定）。None は全可読範囲。
    #[serde(default)]
    pub knowledge_scope: Option<KnowledgeScope>,
    /// 許可ツール（None は全提示）。閉語彙照合・**縮小のみ**（破壊系の明示許可要求は無効化しない）。
    #[serde(default)]
    pub allowed_tools: Option<Vec<ToolName>>,
    /// モデル/パラメータ既定。
    #[serde(default)]
    pub model: Option<ModelDefaults>,
    /// few-shot（user/assistant 対で履歴先頭に注入）。
    #[serde(default)]
    pub few_shot: Vec<FewShotExample>,
    /// script（`.shiki`=script-runtime / `.sh`=agent.invoke サンドボックス）。
    /// **本フェーズでは保存のみ**（実行は呼び出し面・Phase 10 Stage B）。
    #[serde(default)]
    pub scripts: Vec<SkillScript>,
    /// 参照資料（storage node 参照のみ・実体二重持ちなし）。
    #[serde(default)]
    pub references: Vec<Uuid>,
}

/// 知識スコープ（folders は配下全体・files は個別）。両方空の Some は保存時に拒否。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct KnowledgeScope {
    #[serde(default)]
    pub folders: Vec<Uuid>,
    #[serde(default)]
    pub files: Vec<Uuid>,
}

/// モデル/パラメータ既定（llm-gateway 呼び出しへ反映）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ModelDefaults {
    /// 論理モデル名（カタログ照合は llm-gateway 側）。
    #[serde(default)]
    pub model: Option<String>,
    /// 0.0..=2.0。
    #[serde(default)]
    pub temperature: Option<f32>,
    /// 最大トークン。
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// few-shot の 1 対。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FewShotExample {
    pub user: String,
    pub assistant: String,
}

/// skill 同梱 script 1 ファイル（インライン保存＝バージョンと一体で不変・再現性）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SkillScript {
    /// `scripts/<name>.<ext>` 形式の相対パス（`..`・絶対パスは拒否）。
    pub path: String,
    pub kind: ScriptKind,
    /// 本文（実行はしない・保存のみ）。
    pub source: String,
}

vocab_enum! {
    /// script 種別（拡張子と整合・実行面が分岐する）。
    pub enum ScriptKind {
        /// shiki script（`.shiki`・script-runtime で ms 級実行・Stage B）。
        Shiki => "shiki",
        /// shell script（`.sh`・agent.invoke のサンドボックス内で実行・Stage B）。
        Shell => "shell",
    }
}

impl ScriptKind {
    /// 期待する拡張子。
    pub fn extension(self) -> &'static str {
        match self {
            ScriptKind::Shiki => ".shiki",
            ScriptKind::Shell => ".sh",
        }
    }
}

/// skill body を検証する（同期・純粋・全件収集）。
#[allow(clippy::too_many_lines)] // フィールド別の上限検証の列挙（分割すると対応が読みにくい）。
pub fn validate_skill_body(raw: &serde_json::Value) -> Result<SkillBody, Vec<GuiValidationError>> {
    let body: SkillBody = match serde_path_to_error::deserialize(raw.clone()) {
        Ok(body) => body,
        Err(e) => {
            let path = e.path().to_string();
            let err = GuiValidationError::new("skill.schema_violation", e.inner().to_string());
            return Err(vec![if path.is_empty() || path == "." {
                err
            } else {
                err.at(path)
            }]);
        }
    };
    let mut errors = Vec::new();
    if body.description.trim().is_empty() {
        errors.push(GuiValidationError::new(
            "skill.empty_description",
            "description は必須です",
        ));
    }
    if body.description.chars().count() > skill_limits::MAX_DESCRIPTION_CHARS {
        errors.push(
            GuiValidationError::new("skill.too_long", "description が長すぎます").at("description"),
        );
    }
    if body.instructions.trim().is_empty() {
        errors.push(GuiValidationError::new(
            "skill.empty_instructions",
            "instructions（SKILL.md 本文）は必須です",
        ));
    }
    if body.instructions.len() > skill_limits::MAX_INSTRUCTIONS_BYTES {
        errors.push(
            GuiValidationError::new(
                "skill.too_long",
                format!(
                    "instructions が大きすぎます（最大 {} bytes）",
                    skill_limits::MAX_INSTRUCTIONS_BYTES
                ),
            )
            .at("instructions"),
        );
    }
    if let Some(scope) = &body.knowledge_scope {
        if scope.folders.is_empty() && scope.files.is_empty() {
            errors.push(
                GuiValidationError::new(
                    "skill.empty_scope",
                    "knowledge_scope は folders/files のいずれかを 1 件以上指定してください\
                     （制限しない場合は knowledge_scope 自体を省略）",
                )
                .at("knowledge_scope"),
            );
        }
        if scope.folders.len() + scope.files.len() > skill_limits::MAX_SCOPE_REFS {
            errors.push(
                GuiValidationError::new(
                    "skill.too_many_refs",
                    format!("knowledge_scope は最大 {} 件", skill_limits::MAX_SCOPE_REFS),
                )
                .at("knowledge_scope"),
            );
        }
    }
    if let Some(tools) = &body.allowed_tools {
        if tools.is_empty() {
            errors.push(
                GuiValidationError::new(
                    "skill.empty_allowed_tools",
                    "allowed_tools は 1 件以上指定してください（制限しない場合は省略）",
                )
                .at("allowed_tools"),
            );
        }
    }
    if let Some(model) = &body.model {
        if let Some(t) = model.temperature {
            if !(0.0..=skill_limits::MAX_TEMPERATURE).contains(&t) {
                errors.push(
                    GuiValidationError::new("skill.invalid_temperature", "temperature は 0.0〜2.0")
                        .at("model.temperature"),
                );
            }
        }
    }
    if body.few_shot.len() > skill_limits::MAX_FEW_SHOT {
        errors.push(
            GuiValidationError::new(
                "skill.too_many_few_shot",
                format!("few_shot は最大 {} 対", skill_limits::MAX_FEW_SHOT),
            )
            .at("few_shot"),
        );
    }
    for (i, ex) in body.few_shot.iter().enumerate() {
        if ex.user.chars().count() > skill_limits::MAX_FEW_SHOT_CHARS
            || ex.assistant.chars().count() > skill_limits::MAX_FEW_SHOT_CHARS
        {
            errors.push(
                GuiValidationError::new("skill.too_long", "few_shot の発話が長すぎます")
                    .at(format!("few_shot[{i}]")),
            );
        }
    }
    validate_scripts(&body.scripts, &mut errors);
    if body.references.len() > skill_limits::MAX_REFERENCES {
        errors.push(
            GuiValidationError::new(
                "skill.too_many_refs",
                format!("references は最大 {} 件", skill_limits::MAX_REFERENCES),
            )
            .at("references"),
        );
    }
    if errors.is_empty() {
        Ok(body)
    } else {
        Err(errors)
    }
}

/// script の検証（パス安全性・拡張子整合・サイズ・一意性）。
fn validate_scripts(scripts: &[SkillScript], errors: &mut Vec<GuiValidationError>) {
    if scripts.len() > skill_limits::MAX_SCRIPTS {
        errors.push(
            GuiValidationError::new(
                "skill.too_many_scripts",
                format!("scripts は最大 {} 件", skill_limits::MAX_SCRIPTS),
            )
            .at("scripts"),
        );
    }
    let mut seen = std::collections::HashSet::new();
    for (i, script) in scripts.iter().enumerate() {
        let path = format!("scripts[{i}]");
        let p = &script.path;
        let safe = !p.is_empty()
            && p.chars().count() <= skill_limits::MAX_SCRIPT_PATH_CHARS
            && !p.starts_with('/')
            && !p.contains("..")
            && !p.contains('\\')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'));
        if !safe {
            errors.push(
                GuiValidationError::new(
                    "skill.invalid_script_path",
                    format!("script パス '{p}' が不正です（相対パス・英数と /_-. のみ）"),
                )
                .at(&path),
            );
        } else if !p.ends_with(script.kind.extension()) {
            errors.push(
                GuiValidationError::new(
                    "skill.script_kind_mismatch",
                    format!(
                        "kind={} の script は拡張子 {} が必要です（{p}）",
                        script.kind.as_str(),
                        script.kind.extension()
                    ),
                )
                .at(&path),
            );
        }
        if !seen.insert(p.clone()) {
            errors.push(
                GuiValidationError::new(
                    "skill.duplicate_script_path",
                    format!("script パス '{p}' が重複しています"),
                )
                .at(&path),
            );
        }
        if script.source.len() > skill_limits::MAX_SCRIPT_BYTES {
            errors.push(
                GuiValidationError::new(
                    "skill.too_long",
                    format!(
                        "script が大きすぎます（最大 {} bytes）",
                        skill_limits::MAX_SCRIPT_BYTES
                    ),
                )
                .at(&path),
            );
        }
    }
}
