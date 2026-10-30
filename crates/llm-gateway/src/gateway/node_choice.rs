//! ノード（ワークフローの `llm.invoke`・instructions だけの skill・ミニアプリの AI 能力）のモデル解決と、
//! その見え方の組織の設定（BUG-12・D-30）。
//!
//! チャットは呼び出した本人から見た設定（[`LlmGateway::settings_for`]）で解決するが、ノードは
//! **本人専用のモデル（ChatGPT プラン）を、呼び出し主体によらず外した設定**で解決する。
//! - ワークフローは誰が起動しても同じモデルで動かす。schedule / event の実行主体はワークフローで、
//!   本人専用のモデルはそもそも使えない（連携者が対話トリガで起動したときだけ通る、を作らない）。
//! - ミニアプリの AI はアプリの外のコードが呼ぶ。単価 0 のプラン枠では日次予算が効かない。
//!
//! 保存時の照合（api の `WorkflowModels`）と、モデル欄の一覧（`GET /llm/models?purpose=workflow`）も
//! 同じ設定を使う。規則そのものは [`crate::tenant::resolve_node_choice`] にある。

use authz::AuthContext;

use super::chatgpt_gate::without_models;
use super::LlmGateway;
use crate::model::Effort;
use crate::tenant::{self, ChoiceError, ResolvedChoice, TenantLlmSettings};

impl LlmGateway {
    /// 本人専用のモデルを外した組織の AI 設定（既定がそれなら Auto）。保存はしない。
    pub async fn settings_without_personal(
        &self,
        ctx: &AuthContext,
    ) -> Result<TenantLlmSettings, sqlx::Error> {
        let catalog = &self.inner.config.catalog;
        let settings = self.inner.tenants.load(ctx).await?.reconciled(catalog);
        let personal: Vec<&str> = catalog
            .models
            .iter()
            .filter(|m| self.is_personal_model(m))
            .map(|m| m.id.as_str())
            .collect();
        Ok(without_models(catalog, settings, &personal))
    }

    /// ノードのモデル指定を解決する（`model` が `None` か空文字なら組織の既定・使えないモデルは `Err`）。
    ///
    /// 設定は [`Self::settings_without_personal`]。呼び出し側は解決後の `model` を `stream` に渡し、
    /// 会計・出力・監査にも同じ値を残す（`stream` の許可チェックは最終防衛線として残る）。
    pub async fn resolve_node_choice(
        &self,
        ctx: &AuthContext,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> Result<Result<ResolvedChoice, ChoiceError>, sqlx::Error> {
        let settings = self.settings_without_personal(ctx).await?;
        Ok(tenant::resolve_node_choice(
            &self.inner.config.catalog,
            &self.inner.config.auto,
            &settings,
            |e| self.supports_effort(e),
            model,
            effort,
        ))
    }

    /// ワークフローの `llm.invoke` のモデルと思考の深さを解決する（ノードの思考の深さの規則つき・D-75）。ミニアプリの
    /// AI の能力も、モデルを省略した呼び出しと明示した呼び出しの両方をこれで解決する（アプリの指定した思考の深さに同じ
    /// 規則を当て・BUG-116、選択を禁じた組織では既定と違うモデルを断る・D-117）。
    ///
    /// 設定は [`Self::resolve_node_choice`] と同じ。規則は [`crate::tenant::resolve_workflow_choice`] にある。
    pub async fn resolve_workflow_choice(
        &self,
        ctx: &AuthContext,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> Result<Result<ResolvedChoice, ChoiceError>, sqlx::Error> {
        let settings = self.settings_without_personal(ctx).await?;
        Ok(tenant::resolve_workflow_choice(
            &self.inner.config.catalog,
            &self.inner.config.auto,
            &settings,
            |e| self.supports_effort(e),
            model,
            effort,
        ))
    }
}
