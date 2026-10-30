//! ユーザー設定（設定 → チャット／表示・#952）。
//!
//! 本人がアカウント単位で持つ好み。保存は [`crate::ChatStore::put_preferences`]（1 人 1 行の jsonb）。
//! 型と検証はここに閉じ、DB は形を縛らない（欠けた項目は既定値で読む）。
//!
//! どれも画面側の好みで、サーバは認可・承認・ツールの提示に使わない。既定の承認モードもホームの
//! エージェントのモードにある承認モードの選択欄の初期値にすぎず、スレッドの承認モードは作成者が
//! その選択欄で明示して送ったときだけ変わる（スレッドは従来どおり承認必須で作る）。全自動（bypass）は
//! org キャップで禁止できるので、既定にするときも保存時に検査して明示エラーにする。既定のモデルも、
//! 送信時は従来どおり API が検証する。

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{AutonomousMode, ChatError};

/// 既定のモデル ID の上限（文字数）。
pub const MAX_MODEL_ID_CHARS: usize = 200;

/// 入力欄の送信キー。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubmitKey {
    /// Enter で送信（Shift+Enter で改行）。
    #[default]
    Enter,
    /// Ctrl+Enter（Mac は ⌘+Enter）で送信。Enter は改行。
    ModEnter,
}

/// 入力欄の書き方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputFormat {
    /// Markdown をその場で見た目に変えて書く（`**太字**` が太字になる等）。送る本文は Markdown。
    #[default]
    Markdown,
    /// 記法を変換しない素のテキスト。
    Plain,
}

/// 日時の表示形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimeFormat {
    /// 「2026/06/25 10:00」。
    #[default]
    Absolute,
    /// 「3 分前」（1 週間以上前は日時）。
    Relative,
}

/// 新しいチャットの既定のモデル（`None` は Auto）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultModel {
    /// モデル ID（論理名）。`None` は「組織の既定のモデルのまま、思考の深さだけ変える」。
    pub model: Option<String>,
    pub effort: llm_gateway::Effort,
}

/// 本人のユーザー設定（未登録なら既定値）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UserPreferences {
    #[serde(default)]
    pub submit_key: SubmitKey,
    #[serde(default)]
    pub input_format: InputFormat,
    #[serde(default)]
    pub default_model: Option<DefaultModel>,
    /// 新しいチャットで Web 検索を選んだ状態にする。
    #[serde(default)]
    pub web_search_by_default: bool,
    /// 新しいスレッドの承認モード。
    #[serde(default)]
    pub default_autonomous_mode: AutonomousMode,
    /// 思考過程を開いた状態で表示する。
    #[serde(default)]
    pub expand_reasoning: bool,
    #[serde(default)]
    pub time_format: TimeFormat,
}

impl UserPreferences {
    /// 保存値（jsonb）から読む。項目ごとに読み、欠けた項目・読めない値（新しい版が足した値を
    /// 古い版が読む等）はその項目だけ既定値にする（ほかの項目は保つ。設定が読めないことで画面を
    /// 止めない。承認モードの既定は承認必須なので、戻しても権限は広がらない）。
    pub fn from_stored(value: serde_json::Value) -> Self {
        let serde_json::Value::Object(map) = value else {
            tracing::warn!("user_preference: 保存値がオブジェクトでないため既定値を使う");
            return UserPreferences::default();
        };
        fn field<T: serde::de::DeserializeOwned + Default>(
            map: &serde_json::Map<String, serde_json::Value>,
            key: &str,
        ) -> T {
            match map.get(key) {
                None => T::default(),
                Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|e| {
                    tracing::warn!(key, error = %e, "user_preference: 読めない値は既定値を使う");
                    T::default()
                }),
            }
        }
        UserPreferences {
            submit_key: field(&map, "submit_key"),
            input_format: field(&map, "input_format"),
            default_model: field(&map, "default_model"),
            web_search_by_default: field(&map, "web_search_by_default"),
            default_autonomous_mode: field(&map, "default_autonomous_mode"),
            expand_reasoning: field(&map, "expand_reasoning"),
            time_format: field(&map, "time_format"),
        }
    }

    /// 保存前の正規化と検査（モデル ID の前後の空白を落とし、空なら「指定なし」にする）。
    pub fn normalized(self) -> Result<Self, ChatError> {
        let default_model = match self.default_model {
            Some(DefaultModel { model, effort }) => {
                let model = match model.as_deref().map(str::trim) {
                    None | Some("") => None,
                    Some(m) if m.chars().count() > MAX_MODEL_ID_CHARS => {
                        return Err(ChatError::Invalid(format!(
                            "モデル ID は {MAX_MODEL_ID_CHARS} 文字以内で指定してください"
                        )))
                    }
                    Some(m) if m.chars().any(char::is_control) => {
                        return Err(ChatError::Invalid(
                            "モデル ID に制御文字は使えません".into(),
                        ))
                    }
                    Some(m) => Some(m.to_string()),
                };
                Some(DefaultModel { model, effort })
            }
            None => None,
        };
        Ok(UserPreferences {
            default_model,
            ..self
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_fields_read_as_defaults() {
        assert_eq!(
            UserPreferences::from_stored(json!({})),
            UserPreferences::default()
        );
        let p = UserPreferences::from_stored(json!({ "submit_key": "mod_enter" }));
        assert_eq!(p.submit_key, SubmitKey::ModEnter);
        assert_eq!(p.default_autonomous_mode, AutonomousMode::RequireApproval);
        assert_eq!(
            p.input_format,
            InputFormat::Markdown,
            "入力欄の既定は Markdown"
        );
    }

    #[test]
    fn unreadable_value_falls_back_to_default_for_that_field_only() {
        let p = UserPreferences::from_stored(json!({
            "submit_key": "mod_enter",
            "input_format": "html",
            "default_autonomous_mode": "yolo",
            "time_format": 3,
            "expand_reasoning": true,
        }));
        assert_eq!(p.submit_key, SubmitKey::ModEnter, "読める項目は保つ");
        assert!(p.expand_reasoning);
        assert_eq!(p.default_autonomous_mode, AutonomousMode::RequireApproval);
        assert_eq!(p.time_format, TimeFormat::Absolute);
        assert_eq!(p.input_format, InputFormat::Markdown);
        assert_eq!(
            UserPreferences::from_stored(json!(["not", "an", "object"])),
            UserPreferences::default()
        );
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let p = UserPreferences::from_stored(
            json!({ "removed_setting": true, "expand_reasoning": true }),
        );
        assert!(p.expand_reasoning);
    }

    #[test]
    fn roundtrip_through_json() {
        let p = UserPreferences {
            submit_key: SubmitKey::ModEnter,
            input_format: InputFormat::Plain,
            default_model: Some(DefaultModel {
                model: Some("claude-sonnet".into()),
                effort: llm_gateway::Effort::High,
            }),
            web_search_by_default: true,
            default_autonomous_mode: AutonomousMode::Auto,
            expand_reasoning: true,
            time_format: TimeFormat::Relative,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["default_model"]["effort"], "high");
        assert_eq!(UserPreferences::from_stored(v), p);
    }

    #[test]
    fn model_id_is_trimmed_and_empty_means_org_default() {
        let p = |model: Option<&str>| UserPreferences {
            default_model: Some(DefaultModel {
                model: model.map(str::to_string),
                effort: llm_gateway::Effort::Low,
            }),
            ..UserPreferences::default()
        };
        let got = p(Some("  gpt-x  ")).normalized().unwrap();
        assert_eq!(got.default_model.unwrap().model.as_deref(), Some("gpt-x"));
        let got = p(Some("   ")).normalized().unwrap();
        assert_eq!(got.default_model.unwrap().model, None);
    }

    #[test]
    fn over_long_or_control_model_id_is_rejected() {
        let p = |model: String| UserPreferences {
            default_model: Some(DefaultModel {
                model: Some(model),
                effort: llm_gateway::Effort::Medium,
            }),
            ..UserPreferences::default()
        };
        assert!(p("m".repeat(MAX_MODEL_ID_CHARS)).normalized().is_ok());
        assert!(matches!(
            p("m".repeat(MAX_MODEL_ID_CHARS + 1)).normalized(),
            Err(ChatError::Invalid(_))
        ));
        assert!(matches!(
            p("a\u{0}b".into()).normalized(),
            Err(ChatError::Invalid(_))
        ));
    }
}
