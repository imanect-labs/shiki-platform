//! ワークフロー検証カタログの単一構築点（Task 10.13）。
//!
//! カタログ（発話ユーザーの secret 名→許可ホスト・設定済みモデル一覧）は
//! **保存 API と AI 編集ツール（emit_workflow）が同一実装を共有する**。ここが分裂すると
//! 「エディタでは通るが AI 経由では落ちる」乖離が生まれるため、構築ロジックはこの 1 箇所に置く。

use std::sync::Arc;

use workflow_engine::Catalog;

/// カタログを組む（secrets 未配線ならモデルのみ）。
pub async fn build_catalog_from(
    secrets: Option<&secrets::SecretStore>,
    models: &[String],
    ctx: &authz::AuthContext,
) -> Result<Catalog, secrets::SecretError> {
    let mut catalog = Catalog::default();
    // secret の参照名→許可ホスト（V4 の宛先束縛事前照合に使う）。
    if let Some(secrets) = secrets {
        for meta in secrets.list_mine(ctx).await? {
            catalog.secrets.insert(meta.name, meta.allowed_hosts);
        }
    }
    // モデルカタログ（llm.invoke の model 照合）。
    catalog.models = models.to_vec();
    Ok(catalog)
}

/// chat ワーカー（emit_workflow）へ注入するカタログ源。保存 API と同じ材料を持つ。
pub struct ApiWorkflowCatalogSource {
    secrets: Option<Arc<secrets::SecretStore>>,
    models: Vec<String>,
}

impl ApiWorkflowCatalogSource {
    #[must_use]
    pub fn new(secrets: Option<Arc<secrets::SecretStore>>, models: Vec<String>) -> Self {
        ApiWorkflowCatalogSource { secrets, models }
    }
}

#[async_trait::async_trait]
impl chat::WorkflowCatalogSource for ApiWorkflowCatalogSource {
    async fn catalog(&self, ctx: &authz::AuthContext) -> Result<Catalog, String> {
        build_catalog_from(self.secrets.as_deref(), &self.models, ctx)
            .await
            .map_err(|e| format!("カタログ構築に失敗: {e}"))
    }
}
