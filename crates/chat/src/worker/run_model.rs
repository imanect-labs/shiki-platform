//! run のモデル指定（チャットで選んだモデル・思考強度・#553）と skill のモデル既定の合成。
//!
//! 優先順位（投入時に API がテナント設定で解決・検証済みの値が run に載っている）:
//! 1. ユーザーがモデルを**選んだ** run（`model_auto = false`）はそのモデルと思考強度で生成する。
//!    skill のピンより優先する（利用者の明示的な選択を黙って差し替えない）。
//! 2. Auto の run は skill がモデルを指定していればそれを使う（Auto＝「任せる」なので、
//!    skill 作者の指定が勝つ）。このとき Auto の思考強度は別モデル向けなので載せない。
//! 3. それ以外は Auto の振り先（run に解決済み）。
//! 4. run にモデルが無い（このカラム以前の run）は従来どおり（skill → 設定 → 既定）。

use llm_gateway::Effort;

/// 合成結果（`model` が `None` なら gateway の既定モデル）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EffectiveModel {
    pub model: Option<String>,
    pub effort: Option<Effort>,
}

/// run の指定・skill の既定・従来の既定（設定）からモデルと思考強度を決める。
pub(super) fn effective_model(
    run_model: Option<&str>,
    run_effort: Option<&str>,
    run_auto: bool,
    skill_model: Option<&str>,
    fallback: Option<&str>,
) -> EffectiveModel {
    match run_model {
        Some(m) if !run_auto || skill_model.is_none() => EffectiveModel {
            model: Some(m.to_string()),
            effort: run_effort.and_then(Effort::parse),
        },
        _ => EffectiveModel {
            model: skill_model.or(fallback).map(str::to_string),
            effort: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_choice_beats_skill_pin() {
        let e = effective_model(
            Some("opus"),
            Some("high"),
            false,
            Some("skill-m"),
            Some("cfg"),
        );
        assert_eq!(e.model.as_deref(), Some("opus"));
        assert_eq!(e.effort, Some(Effort::High));
    }

    #[test]
    fn auto_defers_to_skill_pin_without_auto_effort() {
        let e = effective_model(Some("gpt-6-luna"), Some("low"), true, Some("skill-m"), None);
        assert_eq!(e.model.as_deref(), Some("skill-m"));
        assert_eq!(e.effort, None);
        let e = effective_model(Some("gpt-6-luna"), Some("low"), true, None, Some("cfg"));
        assert_eq!(e.model.as_deref(), Some("gpt-6-luna"));
        assert_eq!(e.effort, Some(Effort::Low));
    }

    #[test]
    fn legacy_run_keeps_previous_behaviour() {
        let e = effective_model(None, None, true, None, Some("cfg"));
        assert_eq!(e.model.as_deref(), Some("cfg"));
        let e = effective_model(None, None, true, Some("skill-m"), Some("cfg"));
        assert_eq!(e.model.as_deref(), Some("skill-m"));
        assert_eq!(effective_model(None, None, true, None, None).model, None);
    }
}
