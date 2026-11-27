//! ワークフローを編集する人（保存する人）から見たモデルの設定（D-169・D-185）。
//!
//! ワークフローのモデルは、誰が起動しても本人専用のモデルを外した組織の設定
//! （[`LlmGateway::settings_without_personal`]）で解決し、実行の主体（schedule・event ではワークフロー）には
//! 利用者ごとのプロバイダの制限（`provider_allowed_for`・#1014）を掛けない。そのままでは、制限された人が使えない
//! プロバイダのモデルを書いて保存し、自動実行で制限を回り込める。そこで、編集画面のモデルの欄
//! （`GET /llm/models?purpose=workflow`）と保存時の照合（api の `WorkflowModels`）では、**編集する人の制限**を通す。
//! 実行時の解決（[`LlmGateway::resolve_workflow_choice`]）は変えない。
//!
//! - 書けるモデル: 実行時の設定の許可リストから、この人に許されないプロバイダのモデルを外したもの（判定はチャットの
//!   一覧 [`LlmGateway::settings_for`] と同じ `provider_allowed_for`）。判定用のモデルは、元にする chat モデルが
//!   外れたものも外す（判定のチョークポイントは両方のプロバイダを確かめる）。
//! - 既定・Auto の振り先・選択の禁止: 実行時の設定のまま。モデルを省いたノードは実行時にこれで動くので、チャットの
//!   ように既定を Auto へ差し替えると、欄の「組織の既定に従う（現在: …）」が実際の解決とずれる。
//! - モデルを省いたノード: 実行時に動くモデル（[`WorkflowEditorSettings::default_target`]）がこの人に使えなければ、
//!   保存時に断る（D-185・[`WorkflowEditorSettings::default_restricted`]）。チャットは既定を Auto へ差し替え、Auto を
//!   使えるモデルに絞るので、制限された人の生成が制限されたプロバイダへ届くことは無い。ワークフローは実行時に
//!   編集した人を知らないので、同じ結果を保存時に断って作る。
//!
//! 制限の無い人（規則の無い人・人でない主体）には、実行時の設定そのものになる。

use std::collections::BTreeSet;

use authz::AuthContext;

use super::chatgpt_gate::without_models;
use super::LlmGateway;
use crate::config::{AutoConfig, ModelCatalog, ModelEntry};
use crate::tenant::{self, TenantLlmSettings};

/// ワークフローを編集する人から見たモデルの設定（[`LlmGateway::settings_for_workflow_editor`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowEditorSettings {
    /// 実行時の解決と同じ設定（本人専用のモデルを外した組織の設定）。
    runtime: TenantLlmSettings,
    /// 書けるモデルの絞り込み（実行時の許可リストから、この人に許されないプロバイダのモデルを外したもの）。
    /// 既定は Auto に差し替わっていることがあるので、既定・Auto には使わない。
    editable: TenantLlmSettings,
    /// 組織では使えるが、この人には使えないモデル（chat・判定用・カタログ順）。
    restricted: Vec<String>,
    /// モデルを省いたノードが今の設定で動くモデル（使えるモデルが無ければ `None`）。
    default_target: Option<String>,
}

impl WorkflowEditorSettings {
    /// 組む（純粋関数・`allowed` はこの人に許すモデルか）。
    pub(super) fn build(
        catalog: &ModelCatalog,
        auto: &AutoConfig,
        runtime: TenantLlmSettings,
        allowed: impl Fn(&ModelEntry) -> bool,
        supports_effort: impl Fn(&ModelEntry) -> bool,
    ) -> Self {
        let hidden: Vec<&str> = catalog
            .models
            .iter()
            .filter(|m| !allowed(m))
            .map(|m| m.id.as_str())
            .collect();
        let editable = without_models(catalog, runtime.clone(), &hidden);
        let usable = |s: &TenantLlmSettings| -> BTreeSet<String> {
            s.enabled_chat_models(catalog)
                .into_iter()
                .chain(s.enabled_decision_models(catalog))
                .map(|m| m.id.clone())
                .collect()
        };
        let (org, mine) = (usable(&runtime), usable(&editable));
        let restricted = catalog
            .models
            .iter()
            .filter(|m| org.contains(&m.id) && !mine.contains(&m.id))
            .map(|m| m.id.clone())
            .collect();
        let default_target =
            tenant::resolve_workflow_choice(catalog, auto, &runtime, supports_effort, None, None)
                .ok()
                .map(|c| c.model);
        WorkflowEditorSettings {
            runtime,
            editable,
            restricted,
            default_target,
        }
    }

    /// 実行時の解決と同じ設定（既定・Auto の振り先・選択の禁止はこれで読む）。
    #[must_use]
    pub fn runtime(&self) -> &TenantLlmSettings {
        &self.runtime
    }

    /// この人が llm.invoke・agent.invoke に書ける chat モデル（カタログ順）。
    #[must_use]
    pub fn chat_models<'a>(&self, catalog: &'a ModelCatalog) -> Vec<&'a ModelEntry> {
        self.editable.enabled_chat_models(catalog)
    }

    /// この人が ai.decide に書ける判定用のモデル（カタログ順・元にする chat モデルも書けるもの）。
    #[must_use]
    pub fn decision_models<'a>(&self, catalog: &'a ModelCatalog) -> Vec<&'a ModelEntry> {
        self.editable.enabled_decision_models(catalog)
    }

    /// 組織では使えるが、この人には使えないモデル（chat・判定用・カタログ順）。制限の無い人は空。
    #[must_use]
    pub fn restricted(&self) -> &[String] {
        &self.restricted
    }

    /// モデルを省いたノードが今の設定で実行時に動くモデル（組織の既定か Auto の振り先・使えるモデルが無ければ
    /// `None`）。
    #[must_use]
    pub fn default_target(&self) -> Option<&str> {
        self.default_target.as_deref()
    }

    /// モデルを省いたノードが実行時に動くモデルが、この人には使えないとき、そのモデル（D-185）。
    #[must_use]
    pub fn default_restricted(&self) -> Option<&str> {
        self.default_target()
            .filter(|m| self.restricted.iter().any(|r| r == m))
    }
}

impl LlmGateway {
    /// ワークフローを編集する人（保存する人）から見たモデルの設定（D-169・保存はしない）。
    ///
    /// 実行時の設定（[`Self::settings_without_personal`]）から、この人に許されないプロバイダのモデルを書けるモデルから
    /// 外す（判定は `provider_allowed_for`・チャットの一覧 [`Self::settings_for`] と同じ）。既定・Auto の振り先は実行時の
    /// まま。
    pub async fn settings_for_workflow_editor(
        &self,
        ctx: &AuthContext,
    ) -> Result<WorkflowEditorSettings, sqlx::Error> {
        let runtime = self.settings_without_personal(ctx).await?;
        Ok(WorkflowEditorSettings::build(
            &self.inner.config.catalog,
            &self.inner.config.auto,
            runtime,
            |m| self.provider_allowed_for(ctx, m.provider_id()),
            |m| self.supports_effort(m),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Modality;
    use crate::tenant::AUTO_MODEL;

    /// open（制限なし）の fast・decide（fast を元にした判定用）と、closed（制限されたプロバイダ）の smart・
    /// judge（smart を元にした判定用）・via（open の判定用だが元は smart）。
    fn catalog() -> ModelCatalog {
        let entry = |id: &str, provider: &str| ModelEntry {
            id: id.into(),
            provider: Some(provider.into()),
            ..Default::default()
        };
        let decision = |id: &str, provider: &str, via: &str| ModelEntry {
            modality: Modality::Decision,
            decision_via: Some(via.into()),
            ..entry(id, provider)
        };
        ModelCatalog {
            default_model: "smart".into(),
            models: vec![
                entry("fast", "open"),
                entry("smart", "closed"),
                decision("decide", "open", "fast"),
                decision("judge", "closed", "smart"),
                decision("via", "open", "smart"),
            ],
        }
    }

    fn view(default: &str, restricted: bool) -> WorkflowEditorSettings {
        let runtime = TenantLlmSettings {
            default_model: default.into(),
            ..TenantLlmSettings::default()
        };
        let auto = AutoConfig {
            model: "smart".into(),
            ..AutoConfig::default()
        };
        WorkflowEditorSettings::build(
            &catalog(),
            &auto,
            runtime,
            |m| !restricted || m.provider_id() == "open",
            |_| true,
        )
    }

    fn ids(models: Vec<&ModelEntry>) -> Vec<&str> {
        models.into_iter().map(|m| m.id.as_str()).collect()
    }

    /// 制限された人は、許されないプロバイダのモデルと、それを元にする判定用のモデルを書けない。既定・Auto の振り先は
    /// 実行時のまま残し、モデルを省いたノードが実行時に動くモデルが使えなければ知らせる。
    #[test]
    fn restricted_editors_lose_closed_models_but_keep_the_runtime_default() {
        let c = catalog();
        let v = view("smart", true);
        assert_eq!(ids(v.chat_models(&c)), ["fast"]);
        assert_eq!(ids(v.decision_models(&c)), ["decide"]);
        assert_eq!(v.restricted(), ["smart", "judge", "via"]);
        assert_eq!(v.runtime().default_model, "smart", "既定は差し替えない");
        assert_eq!(v.default_target(), Some("smart"));
        assert_eq!(v.default_restricted(), Some("smart"));

        // 既定が Auto: 実行時の Auto の振り先（smart）で判断する（この人向けに Auto を絞らない）。
        let auto = view(AUTO_MODEL, true);
        assert_eq!(auto.default_target(), Some("smart"));
        assert_eq!(auto.default_restricted(), Some("smart"));
        // 既定が使えるモデルなら省いてよい。
        assert_eq!(view("fast", true).default_restricted(), None);
    }

    /// 制限の無い人には、実行時の設定そのもの（何も外さない）。
    #[test]
    fn unrestricted_editors_see_the_runtime_settings() {
        let c = catalog();
        let v = view("smart", false);
        assert_eq!(ids(v.chat_models(&c)), ["fast", "smart"]);
        assert_eq!(ids(v.decision_models(&c)), ["decide", "judge", "via"]);
        assert!(v.restricted().is_empty());
        assert_eq!(v.default_target(), Some("smart"));
        assert_eq!(v.default_restricted(), None);
    }
}
