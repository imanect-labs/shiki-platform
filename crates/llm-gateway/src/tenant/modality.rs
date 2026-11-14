//! モデルの種類（chat / decision・FEAT-1）による組織の設定の絞り込み（tenant.rs の行数上限のため分割）。
//!
//! 組織の許可リスト（[`TenantLlmSettings::enabled_models`]）は**種類を問わず全部**のまま持つ。管理者が
//! 何かを保存するたびに許可リストを組み直すので、そこを chat に絞ると、判定用のモデルが黙って許可リスト
//! から消える（判定のノードが突然使えなくなる）。代わりに、呼び出し元ごとに種類で絞る:
//! - チャットの選択・既定・Auto・`resolve_choice`・`/llm/models`（既定）は [`enabled_chat_models`]。
//! - 判定のモデルの一覧（`/llm/models?modality=decision`）は [`enabled_decision_models`]。元にする chat
//!   モデル（`decision_via`）も有効なものだけ（判定のチョークポイントは両方の許可を求める）。
//! - 生成・判定のたびの許可（`permits`）は種類に関係なく許可リストで見る（従来どおり）。
//!
//! [`enabled_chat_models`]: TenantLlmSettings::enabled_chat_models
//! [`enabled_decision_models`]: TenantLlmSettings::enabled_decision_models

use super::{SettingsError, TenantLlmSettings, AUTO_MODEL};
use crate::config::{ModelCatalog, ModelEntry};

impl TenantLlmSettings {
    /// 組織で使える chat モデル（カタログ順）。
    pub fn enabled_chat_models<'a>(&self, catalog: &'a ModelCatalog) -> Vec<&'a ModelEntry> {
        catalog
            .models
            .iter()
            .filter(|m| m.is_chat() && self.allows(m))
            .collect()
    }

    /// 組織で判定に使える判定用のモデル（カタログ順）。元にする chat モデルが無効なものは除く
    /// （判定のたびに両方の許可を確かめるので、選んでも判定できない）。
    pub fn enabled_decision_models<'a>(&self, catalog: &'a ModelCatalog) -> Vec<&'a ModelEntry> {
        catalog
            .models
            .iter()
            .filter(|m| {
                m.is_decision()
                    && self.allows(m)
                    && m.decision_via
                        .as_deref()
                        .is_none_or(|via| self.is_chat_enabled(catalog, via))
            })
            .collect()
    }

    /// 組織で使える chat モデルか（チャット・ノードのモデル指定に書けるか）。
    pub fn is_chat_enabled(&self, catalog: &ModelCatalog, model: &str) -> bool {
        catalog
            .get(model)
            .is_some_and(|e| e.is_chat() && self.allows(e))
    }
}

/// 保存する設定の chat の決まり: chat モデルが 1 つは有効、既定・Auto の振り先は chat モデル。
pub(super) fn check_chat_targets(
    catalog: &ModelCatalog,
    next: &TenantLlmSettings,
) -> Result<(), SettingsError> {
    if next.enabled_chat_models(catalog).is_empty() {
        return Err(SettingsError::NoModelEnabled);
    }
    let decision = |m: &str| catalog.get(m).is_some_and(ModelEntry::is_decision);
    if next.default_model != AUTO_MODEL && decision(&next.default_model) {
        return Err(SettingsError::NotChatModel(next.default_model.clone()));
    }
    if let Some(m) = next.auto_model.as_deref().filter(|m| decision(m)) {
        return Err(SettingsError::NotChatModel(m.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{
        apply_patch, auto_target, resolve_choice, ChoiceError, ModelChoice, SettingsPatch,
    };
    use super::*;
    use crate::config::{AutoConfig, Modality};

    /// chat の sonnet・haiku と、haiku を元にした判定用の decide。
    fn catalog() -> ModelCatalog {
        let chat = |id: &str| ModelEntry {
            id: id.into(),
            ..Default::default()
        };
        ModelCatalog {
            default_model: "sonnet".into(),
            models: vec![
                ModelEntry {
                    id: "decide".into(),
                    modality: Modality::Decision,
                    decision_via: Some("haiku".into()),
                    ..Default::default()
                },
                chat("sonnet"),
                chat("haiku"),
            ],
        }
    }

    fn ids(models: &[&ModelEntry]) -> Vec<String> {
        models.iter().map(|m| m.id.clone()).collect()
    }

    #[test]
    fn the_allowlist_keeps_every_kind_and_callers_filter_by_kind() {
        let c = catalog();
        let s = TenantLlmSettings::default();
        assert_eq!(ids(&s.enabled_models(&c)), ["decide", "sonnet", "haiku"]);
        assert_eq!(ids(&s.enabled_chat_models(&c)), ["sonnet", "haiku"]);
        assert_eq!(ids(&s.enabled_decision_models(&c)), ["decide"]);
        assert!(s.is_chat_enabled(&c, "haiku"));
        assert!(
            !s.is_chat_enabled(&c, "decide"),
            "判定用はチャットで使えない"
        );
        assert!(s.is_enabled(&c, "decide"), "許可そのものは種類を問わない");
        // 元の chat モデル（haiku）を無効にすると、判定用のモデルは許可されていても判定に使えない。
        let no_base = TenantLlmSettings {
            allowed_models: Some(vec!["decide".into(), "sonnet".into()]),
            ..Default::default()
        };
        assert!(no_base.enabled_decision_models(&c).is_empty());
        assert!(no_base.is_enabled(&c, "decide"));
    }

    #[test]
    fn switching_chat_models_keeps_the_decision_model_allowed() {
        let c = catalog();
        // 管理者がチャットのモデルを切り替えて保存する（判定用のモデルには触れない）。
        let patch = SettingsPatch {
            default_model: Some("haiku".into()),
            models: vec![("sonnet".into(), false)],
            ..Default::default()
        };
        let next = apply_patch(&c, &TenantLlmSettings::default(), &patch).unwrap();
        assert_eq!(
            next.allowed_models,
            Some(vec!["decide".to_string(), "haiku".to_string()])
        );
        // もう一度、関係のない保存をしても残る。
        let again = apply_patch(&c, &next, &SettingsPatch::default()).unwrap();
        assert!(again.is_enabled(&c, "decide"));
    }

    #[test]
    fn saving_with_only_decision_models_enabled_is_rejected() {
        let c = catalog();
        let patch = SettingsPatch {
            models: vec![("sonnet".into(), false), ("haiku".into(), false)],
            ..Default::default()
        };
        assert_eq!(
            apply_patch(&c, &TenantLlmSettings::default(), &patch),
            Err(SettingsError::NoModelEnabled)
        );
    }

    #[test]
    fn a_decision_model_cannot_be_the_default_or_the_auto_target() {
        let c = catalog();
        let base = TenantLlmSettings::default();
        for patch in [
            SettingsPatch {
                default_model: Some("decide".into()),
                ..Default::default()
            },
            SettingsPatch {
                auto_model: Some(Some("decide".into())),
                ..Default::default()
            },
        ] {
            assert_eq!(
                apply_patch(&c, &base, &patch),
                Err(SettingsError::NotChatModel("decide".into()))
            );
        }
        // 以前に保存された（読み直すと決定モデルを指す）既定は Auto に戻して読む。
        let stale = TenantLlmSettings {
            default_model: "decide".into(),
            auto_model: Some("decide".into()),
            ..Default::default()
        }
        .reconciled(&c);
        assert_eq!(stale.default_model, AUTO_MODEL);
        assert_eq!(stale.auto_model, None);
    }

    #[test]
    fn auto_and_chat_choices_never_land_on_a_decision_model() {
        let c = catalog();
        // Auto の振り先（デプロイ設定）が判定用のモデルを指していても、chat モデルへ落ちる。
        let auto = AutoConfig {
            model: "decide".into(),
            ..Default::default()
        };
        let s = TenantLlmSettings::default();
        assert_eq!(auto_target(&c, &auto, &s).unwrap().0, "sonnet");
        // 最後の砦（有効な先頭）も chat から選ぶ（カタログの先頭は判定用）。
        let only_haiku = TenantLlmSettings {
            allowed_models: Some(vec!["decide".into(), "haiku".into()]),
            ..Default::default()
        };
        let mut no_default = c.clone();
        no_default.default_model = "ghost".into();
        assert_eq!(
            auto_target(&no_default, &auto, &only_haiku).unwrap().0,
            "haiku"
        );
        let only_decision = TenantLlmSettings {
            allowed_models: Some(vec!["decide".into()]),
            ..Default::default()
        };
        assert_eq!(
            auto_target(&c, &auto, &only_decision),
            Err(ChoiceError::NoModel)
        );
        // チャットで判定用のモデルを選んでも使えない。
        let chosen = resolve_choice(
            &c,
            &auto,
            &s,
            |_| true,
            &ModelChoice {
                model: Some("decide".into()),
                effort: None,
            },
        );
        assert_eq!(chosen, Err(ChoiceError::ModelNotAllowed("decide".into())));
    }
}
