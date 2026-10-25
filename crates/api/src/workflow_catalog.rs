//! ワークフロー検証カタログの単一構築点（Task 10.13）。
//!
//! カタログ（発話ユーザーの secret 名→許可ホスト・使えるモデル一覧）は
//! **保存 API と AI 編集ツール（emit_workflow）が同一実装を共有する**。ここが分裂すると
//! 「エディタでは通るが AI 経由では落ちる」乖離が生まれるため、構築ロジックはこの 1 箇所に置く。

use std::sync::Arc;

use workflow_engine::Catalog;

/// llm.invoke の model を照合する集合の材料。
///
/// gateway があれば**組織の許可リストから本人専用のモデルを除いたもの**（BUG-11・BUG-12）。
/// 実行時は gateway が同じ設定（[`llm_gateway::LlmGateway::settings_without_personal`]）でモデルを
/// 解決するので、保存時もそこへ揃える（管理者が無効にしたモデル・opt-in 前のモデルを保存で弾く）。
/// ChatGPT プランのモデルは連携した本人の枠で、ワークフローは誰が起動しても使わないため、本人であっても除く。
/// gateway が無い構成（LLM 未構成）では従来どおりデプロイ設定のモデル id を使う（空なら照合しない）。
#[derive(Clone)]
pub enum WorkflowModels {
    Gateway(llm_gateway::LlmGateway),
    Configured(Vec<String>),
}

/// 保存時のモデル照合の材料（[`Catalog::models`] と [`Catalog::model_lock`] に入れる）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowModelCheck {
    /// llm.invoke に書けるモデル id（カタログ順）。`None` は照合しない（LLM 未構成）。
    /// gateway があれば、許可が 0 件でも `Some(空)`（明示したモデルを全部拒否する・BUG-37）。
    pub ids: Option<Vec<String>>,
    /// 組織がモデルの選択を禁じているときの既定（`auto` かモデル ID）。
    pub lock: Option<String>,
}

impl WorkflowModels {
    /// gateway があればそれを、無ければデプロイ設定の id を材料にする。
    #[must_use]
    pub fn new(gateway: Option<llm_gateway::LlmGateway>, configured: Vec<String>) -> Self {
        match gateway {
            Some(gw) => WorkflowModels::Gateway(gw),
            None => WorkflowModels::Configured(configured),
        }
    }

    /// 保存時の照合の材料（呼び出し主体の組織の設定・本人専用を除く）。
    pub async fn check(&self, ctx: &authz::AuthContext) -> Result<WorkflowModelCheck, String> {
        match self {
            WorkflowModels::Configured(ids) => Ok(WorkflowModelCheck {
                ids: (!ids.is_empty()).then(|| ids.clone()),
                lock: None,
            }),
            WorkflowModels::Gateway(gw) => {
                let settings = gw
                    .settings_without_personal(ctx)
                    .await
                    .map_err(|e| format!("組織の AI 設定: {e}"))?;
                Ok(WorkflowModelCheck {
                    ids: Some(
                        settings
                            .enabled_models(gw.catalog())
                            .into_iter()
                            .map(|m| m.id.clone())
                            .collect(),
                    ),
                    lock: (!settings.allow_user_choice).then_some(settings.default_model),
                })
            }
        }
    }

    /// llm.invoke に書けるモデル id（カタログ順・照合しない構成では空）。
    pub async fn ids(&self, ctx: &authz::AuthContext) -> Result<Vec<String>, String> {
        Ok(self.check(ctx).await?.ids.unwrap_or_default())
    }
}

/// カタログを組む（secrets 未配線ならモデルのみ・skill_installs 未配線なら skill 照合は空集合）。
pub async fn build_catalog_from(
    secrets: Option<&secrets::SecretStore>,
    skill_installs: Option<&app_platform::SkillInstallService>,
    models: &WorkflowModels,
    ctx: &authz::AuthContext,
) -> Result<Catalog, String> {
    let mut catalog = Catalog::default();
    // secret の参照名→許可ホスト（V4 の宛先束縛事前照合に使う）。
    if let Some(secrets) = secrets {
        for meta in secrets
            .list_mine(ctx)
            .await
            .map_err(|e| format!("secret 一覧: {e}"))?
        {
            catalog.secrets.insert(meta.name, meta.allowed_hosts);
        }
    }
    // 保存ユーザーのインストール済み skill（V4 skill 照合・10.1b）。
    if let Some(installs) = skill_installs {
        catalog.skills = installs
            .installed_versions(ctx)
            .await
            .map_err(|e| format!("skill インストール一覧: {e}"))?;
    }
    // llm.invoke の model 照合（組織が許可したモデルと、選択を禁じた組織の既定）。
    let check = models.check(ctx).await?;
    catalog.models = check.ids;
    catalog.model_lock = check.lock;
    Ok(catalog)
}

/// chat ワーカー（emit_workflow）へ注入するカタログ源。保存 API と同じ材料を持つ。
pub struct ApiWorkflowCatalogSource {
    secrets: Option<Arc<secrets::SecretStore>>,
    skill_installs: Option<Arc<app_platform::SkillInstallService>>,
    models: WorkflowModels,
}

impl ApiWorkflowCatalogSource {
    #[must_use]
    pub fn new(
        secrets: Option<Arc<secrets::SecretStore>>,
        skill_installs: Option<Arc<app_platform::SkillInstallService>>,
        models: WorkflowModels,
    ) -> Self {
        ApiWorkflowCatalogSource {
            secrets,
            skill_installs,
            models,
        }
    }
}

#[async_trait::async_trait]
impl chat::WorkflowCatalogSource for ApiWorkflowCatalogSource {
    async fn catalog(&self, ctx: &authz::AuthContext) -> Result<Catalog, String> {
        build_catalog_from(
            self.secrets.as_deref(),
            self.skill_installs.as_deref(),
            &self.models,
            ctx,
        )
        .await
        .map_err(|e| format!("カタログ構築に失敗: {e}"))
    }

    /// 説明文用のモデル一覧（secret・skill の一覧は引かない・照合と同じ集合）。
    async fn models(&self, ctx: &authz::AuthContext) -> Result<Vec<String>, String> {
        self.models.ids(ctx).await
    }
}
