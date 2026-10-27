//! ノード（ワークフローの `llm.invoke`・instructions だけの skill・ミニアプリの AI 能力）の
//! モデル指定の解決（純粋関数・BUG-12）。
//!
//! 省略はチャットと同じ規則（[`resolve_choice`]）で組織の既定（Auto を含む）に従う。チャットと違うのは
//! 明示したモデルの扱いだけで、次のとおりにする。
//! - 使えないモデル（カタログ外・組織で無効・本人専用で外したもの・判定用のモデル）は既定へ戻さず拒否する
//!   （fail-closed・D-30）。
//! - `auto` という文字列はモデルとして扱わない（IR のモデル欄に Auto は書けない・保存時検証 V3 と揃える）。
//! - 思考強度は呼び出し側の指定だけを送る（組織の `default_effort` を足さない。IR に effort が無いので、
//!   明示モデルのワークフローは今までどおり思考強度なしで動く）。
//! - 選択の禁止（`allow_user_choice=false`）はチャットと同じく、既定と同じモデル以外を拒否する。

use super::{
    resolve_choice, ChoiceError, ModelChoice, ResolvedChoice, TenantLlmSettings, AUTO_MODEL,
};
use crate::config::{AutoConfig, ModelCatalog, ModelEntry};
use crate::model::Effort;

/// ノードのモデル指定を組織の設定で解決する。
///
/// `model` が `None` か空文字なら組織の既定。`effort` は呼び出し側の指定（ワークフローは `None`）。
/// 既定が特定のモデルなら `effort` か組織の `default_effort`、Auto なら Auto の振り先と思考強度になる
/// （[`resolve_choice`] と同じ）。思考強度に対応しないモデルでは `effort = None`。
pub fn resolve_node_choice(
    catalog: &ModelCatalog,
    auto: &AutoConfig,
    settings: &TenantLlmSettings,
    supports_effort: impl Fn(&ModelEntry) -> bool,
    model: Option<&str>,
    effort: Option<Effort>,
) -> Result<ResolvedChoice, ChoiceError> {
    let Some(model) = model.filter(|m| !m.is_empty()) else {
        return resolve_choice(
            catalog,
            auto,
            settings,
            supports_effort,
            &ModelChoice {
                model: None,
                effort,
            },
        );
    };
    if !settings.allow_user_choice && model != settings.default_model {
        return Err(ChoiceError::ChoiceDisabled);
    }
    // 判定用のモデル（chat ではない）は生成に使えない（`ai.decide` の判定にだけ使う）。
    if model == AUTO_MODEL || !settings.is_chat_enabled(catalog, model) {
        return Err(ChoiceError::ModelNotAllowed(model.to_string()));
    }
    let effort = effort.filter(|_| catalog.get(model).is_some_and(&supports_effort));
    Ok(ResolvedChoice {
        model: model.to_string(),
        effort,
        auto: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a・b は思考強度に対応し、local は対応しない。
    fn catalog() -> ModelCatalog {
        let m = |id: &str| ModelEntry {
            id: id.into(),
            supports_effort: Some(id != "local"),
            ..Default::default()
        };
        ModelCatalog {
            default_model: "a".into(),
            models: vec![m("a"), m("b"), m("local")],
        }
    }

    fn auto() -> AutoConfig {
        AutoConfig {
            model: "b".into(),
            effort: Effort::Low,
        }
    }

    fn resolve(
        settings: &TenantLlmSettings,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> Result<ResolvedChoice, ChoiceError> {
        resolve_node_choice(
            &catalog(),
            &auto(),
            settings,
            |e| e.supports_effort.unwrap_or(false),
            model,
            effort,
        )
    }

    fn settings(default_model: &str) -> TenantLlmSettings {
        TenantLlmSettings {
            default_model: default_model.into(),
            default_effort: Effort::High,
            ..TenantLlmSettings::default()
        }
    }

    #[test]
    fn omitted_or_empty_follows_the_organization_default() {
        // 既定が特定のモデル: そのモデルと組織の思考強度。
        for model in [None, Some("")] {
            let r = resolve(&settings("local"), model, None).unwrap();
            assert_eq!((r.model.as_str(), r.effort, r.auto), ("local", None, false));
            let r = resolve(&settings("a"), model, None).unwrap();
            assert_eq!(
                (r.model.as_str(), r.effort, r.auto),
                ("a", Some(Effort::High), false)
            );
        }
        // 既定が Auto: 振り先（組織の上書き → デプロイ設定）と Auto の思考強度（呼び出し側の指定は使わない）。
        let r = resolve(&settings(AUTO_MODEL), None, Some(Effort::High)).unwrap();
        assert_eq!(
            (r.model.as_str(), r.effort, r.auto),
            ("b", Some(Effort::Low), true)
        );
        let mut overridden = settings(AUTO_MODEL);
        overridden.auto_model = Some("local".into());
        let r = resolve(&overridden, None, None).unwrap();
        assert_eq!((r.model.as_str(), r.auto), ("local", true));
    }

    #[test]
    fn explicit_models_are_kept_without_the_default_effort() {
        let r = resolve(&settings(AUTO_MODEL), Some("a"), None).unwrap();
        assert_eq!((r.model.as_str(), r.effort, r.auto), ("a", None, false));
        // 呼び出し側が指定した思考強度は、対応するモデルにだけ送る。
        let r = resolve(&settings(AUTO_MODEL), Some("a"), Some(Effort::Medium)).unwrap();
        assert_eq!(r.effort, Some(Effort::Medium));
        let r = resolve(&settings(AUTO_MODEL), Some("local"), Some(Effort::Medium)).unwrap();
        assert_eq!(r.effort, None);
    }

    #[test]
    fn unusable_explicit_models_fail_closed() {
        let mut s = settings("a");
        s.allowed_models = Some(vec!["a".into()]);
        // 組織で無効・カタログ外・Auto の文字列は、既定へ戻さずに拒否する。
        for m in ["b", "ghost", AUTO_MODEL] {
            assert_eq!(
                resolve(&s, Some(m), None),
                Err(ChoiceError::ModelNotAllowed(m.into())),
                "{m}"
            );
        }
    }

    #[test]
    fn a_locked_organization_only_accepts_its_default() {
        let mut s = settings("a");
        s.allow_user_choice = false;
        assert_eq!(
            resolve(&s, Some("b"), None),
            Err(ChoiceError::ChoiceDisabled)
        );
        assert_eq!(resolve(&s, Some("a"), None).unwrap().model, "a");
        assert_eq!(resolve(&s, None, None).unwrap().model, "a");
        // 既定が Auto の組織では、どのモデルも明示できない（Auto の文字列も IR には書けない）。
        let mut auto_locked = settings(AUTO_MODEL);
        auto_locked.allow_user_choice = false;
        assert_eq!(
            resolve(&auto_locked, Some("a"), None),
            Err(ChoiceError::ChoiceDisabled)
        );
        assert_eq!(
            resolve(&auto_locked, Some(AUTO_MODEL), None),
            Err(ChoiceError::ModelNotAllowed(AUTO_MODEL.into()))
        );
        assert_eq!(resolve(&auto_locked, None, None).unwrap().model, "b");
    }

    #[test]
    fn no_usable_model_is_reported() {
        let mut s = settings(AUTO_MODEL);
        s.allowed_models = Some(Vec::new());
        assert_eq!(resolve(&s, None, None), Err(ChoiceError::NoModel));
    }
}
