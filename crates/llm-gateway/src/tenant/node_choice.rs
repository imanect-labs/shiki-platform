//! ノード（ワークフローの `llm.invoke`・instructions だけの skill・ミニアプリの AI 能力）の
//! モデル指定の解決（純粋関数・BUG-12）。
//!
//! 省略はチャットと同じ規則（[`resolve_choice`]）で組織の既定（Auto を含む）に従う。チャットと違うのは
//! 明示したモデルの扱いだけで、次のとおりにする。
//! - 使えないモデル（カタログ外・組織で無効・本人専用で外したもの・判定用のモデル）は既定へ戻さず拒否する
//!   （fail-closed・D-30）。
//! - `auto` という文字列はモデルとして扱わない（IR のモデル欄に Auto は書けない・保存時検証 V3 と揃える）。
//! - 思考強度は呼び出し側の指定だけを送る（組織の `default_effort` を足さない。effort を書かない明示モデルの
//!   ワークフローは今までどおり思考強度なしで動く）。
//! - 選択の禁止（`allow_user_choice=false`）はチャットと同じく、既定と同じモデル以外を拒否する。
//!
//! ワークフローの `llm.invoke` に書いた思考の深さ（`effort`）は [`resolve_workflow_choice`] が扱う。組織がモデルの
//! 選択を禁じているときと、モデルを省略して組織の既定の Auto で動くときは使わず、組織の設定に従う（チャットと
//! 揃える・D-75。チャットは Auto で利用者の指定を使わず、選択を禁じた組織では選ぶ画面を出さない）。

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

/// ワークフローのノードに書いた思考の深さを使うか（D-75）。
///
/// 組織がモデルの選択を禁じているとき（`allow_user_choice=false`）と、モデルを省略して組織の既定の Auto で
/// 動くときは使わない。保存時の警告（workflow-engine の `ir.effort_ignored`）も同じ規則で出す。
#[must_use]
pub fn node_effort_applies(settings: &TenantLlmSettings, model: Option<&str>) -> bool {
    let omitted = model.is_none_or(str::is_empty);
    settings.allow_user_choice && !(omitted && settings.default_model == AUTO_MODEL)
}

/// ワークフローのノード（`llm.invoke`）のモデルと思考の深さを解決する。
///
/// [`resolve_node_choice`] と同じ規則に、ノードの思考の深さの規則（[`node_effort_applies`]）を足す。使わない
/// ときは書かなかったのと同じに解決する（省略は組織の既定の思考強度か Auto の設定・明示したモデルは思考強度なし）。
pub fn resolve_workflow_choice(
    catalog: &ModelCatalog,
    auto: &AutoConfig,
    settings: &TenantLlmSettings,
    supports_effort: impl Fn(&ModelEntry) -> bool,
    model: Option<&str>,
    effort: Option<Effort>,
) -> Result<ResolvedChoice, ChoiceError> {
    let effort = effort.filter(|_| node_effort_applies(settings, model));
    resolve_node_choice(catalog, auto, settings, supports_effort, model, effort)
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

    fn workflow(
        settings: &TenantLlmSettings,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> Result<ResolvedChoice, ChoiceError> {
        resolve_workflow_choice(
            &catalog(),
            &auto(),
            settings,
            |e| e.supports_effort.unwrap_or(false),
            model,
            effort,
        )
    }

    /// ノードの思考の深さは、選べる組織で特定のモデルで動くときだけ使う。省略して Auto で動くときと、選択を禁じた
    /// 組織では使わず、組織の設定（Auto の思考強度・既定の思考強度）に従う（チャットと揃える・D-75）。
    #[test]
    fn node_effort_follows_the_chat_rules() {
        let low = Some(Effort::Low);
        // 選べる組織: 明示したモデル・既定が特定のモデルの省略は、ノードの指定を使う。
        let r = workflow(&settings(AUTO_MODEL), Some("a"), low).unwrap();
        assert_eq!((r.model.as_str(), r.effort), ("a", low));
        let r = workflow(&settings("a"), None, low).unwrap();
        assert_eq!((r.model.as_str(), r.effort), ("a", low));
        // 省略して Auto で動く: Auto の思考強度（Low）。ノードに High と書いても使わない。
        let r = workflow(&settings(AUTO_MODEL), Some(""), Some(Effort::High)).unwrap();
        assert_eq!(
            (r.model.as_str(), r.effort, r.auto),
            ("b", Some(Effort::Low), true)
        );
        // 選択を禁じた組織: 省略は組織の既定の思考強度（High）、既定と同じ明示は思考強度なし（書かないのと同じ）。
        let mut locked = settings("a");
        locked.allow_user_choice = false;
        let r = workflow(&locked, None, low).unwrap();
        assert_eq!((r.model.as_str(), r.effort), ("a", Some(Effort::High)));
        let r = workflow(&locked, Some("a"), low).unwrap();
        assert_eq!((r.model.as_str(), r.effort), ("a", None));
        assert_eq!(
            workflow(&locked, Some("b"), low),
            Err(ChoiceError::ChoiceDisabled)
        );
        let mut auto_locked = settings(AUTO_MODEL);
        auto_locked.allow_user_choice = false;
        let r = workflow(&auto_locked, None, Some(Effort::High)).unwrap();
        assert_eq!((r.model.as_str(), r.effort), ("b", Some(Effort::Low)));

        // 規則そのもの（保存時の警告と同じ）。
        assert!(node_effort_applies(&settings(AUTO_MODEL), Some("a")));
        assert!(node_effort_applies(&settings("a"), None));
        assert!(!node_effort_applies(&settings(AUTO_MODEL), None));
        assert!(!node_effort_applies(&settings(AUTO_MODEL), Some("")));
        assert!(!node_effort_applies(&locked, Some("a")));
        assert!(!node_effort_applies(&locked, None));
    }

    #[test]
    fn no_usable_model_is_reported() {
        let mut s = settings(AUTO_MODEL);
        s.allowed_models = Some(Vec::new());
        assert_eq!(resolve(&s, None, None), Err(ChoiceError::NoModel));
    }
}
