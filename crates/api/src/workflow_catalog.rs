//! ワークフロー検証カタログの単一構築点（Task 10.13）。
//!
//! カタログ（発話ユーザーの secret 名→許可ホスト・使えるモデル一覧）は
//! **保存 API と AI 編集ツール（emit_workflow）が同一実装を共有する**。ここが分裂すると
//! 「エディタでは通るが AI 経由では落ちる」乖離が生まれるため、構築ロジックはこの 1 箇所に置く。

use std::sync::Arc;

use workflow_engine::Catalog;

/// llm.invoke の model を照合する集合の材料。
///
/// gateway があれば**組織の許可リストから本人専用のモデルを除いた chat モデル**（BUG-11・BUG-12・FEAT-1）。
/// 実行時は gateway が同じ設定（[`llm_gateway::LlmGateway::settings_without_personal`]）でモデルを
/// 解決するので、保存時もそこへ揃える（管理者が無効にしたモデル・opt-in 前のモデルを保存で弾く）。
/// ChatGPT プランのモデルは連携した本人の枠で、ワークフローは誰が起動しても使わないため、本人であっても除く。
/// gateway が無い構成（LLM 未構成）では従来どおりデプロイ設定のモデル id を使う（空なら照合しない）。
///
/// ai.decide のモデルも同じ材料から、判定用のモデル（`modality = "decision"`）だけを照合する（FEAT-1）。種類の
/// 振り分けはここ（[`WorkflowModels::check`]）だけで行い、保存 API と AI 編集（emit_workflow）が同じ結果になる。
#[derive(Clone)]
pub enum WorkflowModels {
    Gateway(llm_gateway::LlmGateway),
    /// デプロイ設定のモデル（id と種類）。
    Configured(Vec<(String, llm_gateway::Modality)>),
}

/// 保存時のモデル照合の材料（[`Catalog::models`] と [`Catalog::model_lock`] に入れる）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowModelCheck {
    /// llm.invoke に書けるモデル id（カタログ順）。`None` は照合しない（LLM 未構成）。
    /// gateway があれば、許可が 0 件でも `Some(空)`（明示したモデルを全部拒否する・BUG-37）。
    pub ids: Option<Vec<String>>,
    /// 組織がモデルの選択を禁じているときの既定（`auto` かモデル ID）。
    pub lock: Option<String>,
    /// 組織の既定が Auto か（[`Catalog::default_is_auto`]・ノードの思考の深さの警告・D-75）。LLM 未構成は false。
    pub default_is_auto: bool,
    /// ai.decide に書ける判定用のモデル id（カタログ順・[`Catalog::decision_models`]）。`None` は照合しない
    /// （LLM 未構成）。gateway があれば、組織が許可し、元にする chat モデルも許可されているものだけ
    /// （`/llm/models?purpose=workflow&modality=decision` と同じ集合）。
    pub decision_ids: Option<Vec<String>>,
}

impl WorkflowModels {
    /// gateway があればそれを、無ければデプロイ設定の id を材料にする。
    #[must_use]
    pub fn new(
        gateway: Option<llm_gateway::LlmGateway>,
        configured: Vec<(String, llm_gateway::Modality)>,
    ) -> Self {
        match gateway {
            Some(gw) => WorkflowModels::Gateway(gw),
            None => WorkflowModels::Configured(configured),
        }
    }

    /// 保存時の照合の材料（呼び出し主体の組織の設定・本人専用を除く）。
    pub async fn check(&self, ctx: &authz::AuthContext) -> Result<WorkflowModelCheck, String> {
        match self {
            WorkflowModels::Configured(models) => {
                let of = |kind: llm_gateway::Modality| -> Vec<String> {
                    models
                        .iter()
                        .filter(|(_, m)| *m == kind)
                        .map(|(id, _)| id.clone())
                        .collect()
                };
                // 何も設定していなければ照合しない（従来どおり）。
                let configured = !models.is_empty();
                Ok(WorkflowModelCheck {
                    ids: configured.then(|| of(llm_gateway::Modality::Chat)),
                    lock: None,
                    default_is_auto: false,
                    decision_ids: configured.then(|| of(llm_gateway::Modality::Decision)),
                })
            }
            WorkflowModels::Gateway(gw) => {
                let settings = gw
                    .settings_without_personal(ctx)
                    .await
                    .map_err(|e| format!("組織の AI 設定: {e}"))?;
                Ok(WorkflowModelCheck {
                    // llm.invoke に書けるのは chat モデルだけ（判定用のモデルは生成に使えない・FEAT-1）。
                    ids: Some(
                        settings
                            .enabled_chat_models(gw.catalog())
                            .into_iter()
                            .map(|m| m.id.clone())
                            .collect(),
                    ),
                    lock: (!settings.allow_user_choice).then_some(settings.default_model.clone()),
                    // 実行時の解決（`resolve_workflow_choice`）と同じ設定で、省略したモデルが Auto で動くか。
                    default_is_auto: settings.default_model == llm_gateway::AUTO_MODEL,
                    // ai.decide に書けるのは判定用のモデルだけ（元の chat モデルが無効なものは除く・FEAT-1）。
                    decision_ids: Some(
                        settings
                            .enabled_decision_models(gw.catalog())
                            .into_iter()
                            .map(|m| m.id.clone())
                            .collect(),
                    ),
                })
            }
        }
    }

    /// llm.invoke に書けるモデル id（カタログ順・照合しない構成では空）。
    pub async fn ids(&self, ctx: &authz::AuthContext) -> Result<Vec<String>, String> {
        Ok(self.check(ctx).await?.ids.unwrap_or_default())
    }

    /// ai.decide に書ける判定用のモデルの id と表示名（カタログ順・照合しない構成では空・D-79）。
    ///
    /// 集合は [`Self::check`] の `decision_ids` と同じ（組織が許可し、元にする chat モデルも許可されているもの）。
    /// 表示名は管理画面・モデル欄と同じ（[`llm_gateway::ModelEntry::label`]）。デプロイ設定だけの構成では表示名を持たない。
    pub async fn decision_models(
        &self,
        ctx: &authz::AuthContext,
    ) -> Result<Vec<chat::DecisionModel>, String> {
        match self {
            WorkflowModels::Configured(_) => Ok(self
                .check(ctx)
                .await?
                .decision_ids
                .unwrap_or_default()
                .into_iter()
                .map(|id| chat::DecisionModel { id, label: None })
                .collect()),
            WorkflowModels::Gateway(gw) => {
                let settings = gw
                    .settings_without_personal(ctx)
                    .await
                    .map_err(|e| format!("組織の AI 設定: {e}"))?;
                Ok(settings
                    .enabled_decision_models(gw.catalog())
                    .into_iter()
                    .map(|m| chat::DecisionModel {
                        id: m.id.clone(),
                        label: Some(m.label().to_string()),
                    })
                    .collect())
            }
        }
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
    // llm.invoke の model 照合（組織が許可したモデルと、選択を禁じた組織の既定）と、ai.decide の判定用のモデル。
    let check = models.check(ctx).await?;
    catalog.models = check.ids;
    catalog.model_lock = check.lock;
    catalog.default_is_auto = check.default_is_auto;
    catalog.decision_models = check.decision_ids;
    Ok(catalog)
}

/// 承認（human.approval）が束縛するテーブルを保存者の見え方で引いて、V4 の材料にする（GAP-08）。
///
/// IR に承認ノードが無ければ `None`（照合しない）。見えないテーブルは入れない（V4 が「見つからないか FSM で
/// 管理されていない」と同じ文言で止める）。FSM の状態は状態の列の選択肢（FSM の保存時にこれと一致することを
/// data が確かめている）。
///
/// 定義は閲覧者向けの絞り込み（[`data::DataStore::get_table_for_viewer`]）を通す。テーブルの owner でない
/// 保存者には読める列だけが入るので、読めない列は無い列と同じ誤り（`ir.unknown_field`）になり、種別
/// （`ir.bad_assignee`）も返らない（無い列と読めない列を区別させない・data-platform §16.1）。状態の列が
/// 読めなければ状態を入れない（`ir.unknown_table` で止め、「使える状態」の一覧も出さない）。状態の列の名前も
/// 同じときだけ入れる（受信箱に出す列に状態の列を選ぶと止める・BUG-67）。
pub async fn approval_tables(
    data: &data::DataStore,
    ctx: &authz::AuthContext,
    ir: &serde_json::Value,
) -> Result<Option<std::collections::BTreeMap<String, workflow_engine::CatalogTable>>, String> {
    let ids: std::collections::BTreeSet<uuid::Uuid> = ir
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|n| {
            n.get("type").and_then(serde_json::Value::as_str)
                == Some(workflow_engine::vocab::NodeType::HumanApproval.as_str())
        })
        .filter_map(|n| n.pointer("/params/record/table")?.as_str())
        .filter_map(|t| uuid::Uuid::parse_str(t).ok())
        .collect();
    let has_approval = ir
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|ns| {
            ns.iter().any(|n| {
                n.get("type").and_then(serde_json::Value::as_str)
                    == Some(workflow_engine::vocab::NodeType::HumanApproval.as_str())
            })
        });
    if !has_approval {
        return Ok(None);
    }
    let mut out = std::collections::BTreeMap::new();
    for id in ids {
        let table = match data.get_table_for_viewer(ctx, id, None).await {
            Ok(t) => t,
            Err(data::DataError::NotFound | data::DataError::Forbidden) => continue,
            Err(e) => return Err(format!("承認のテーブル: {e}")),
        };
        let schema = &table.schema;
        // 状態の列（FSM で管理し、保存者が読めるときだけ・読めなければ定義の絞り込みが列ごと落とす）。
        let status_def = schema
            .fsm_ref
            .as_ref()
            .and(schema.status_field.as_deref())
            .and_then(|k| schema.field(k));
        let states = status_def.map(|f| f.options.clone());
        let status_field = status_def.map(|f| f.key.clone());
        let fields = schema
            .fields
            .iter()
            .map(|f| {
                let ty = serde_json::to_value(f.field_type)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                (f.key.clone(), ty)
            })
            .collect();
        out.insert(
            id.hyphenated().to_string(),
            workflow_engine::CatalogTable {
                states,
                status_field,
                fields,
            },
        );
    }
    Ok(Some(out))
}

/// chat ワーカー（emit_workflow）へ注入するカタログ源。保存 API と同じ材料を持つ。
pub struct ApiWorkflowCatalogSource {
    secrets: Option<Arc<secrets::SecretStore>>,
    skill_installs: Option<Arc<app_platform::SkillInstallService>>,
    models: WorkflowModels,
    /// AI エージェントの web.fetch が読んでよいサイトの天井（保存 API と同じ設定・BUG-6）。
    web_ceiling: Option<Vec<String>>,
    /// 承認（human.approval）が束縛するテーブルを引く（保存 API の `build_catalog_for` と同じ・GAP-08）。
    data: Option<Arc<data::DataStore>>,
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
            web_ceiling: None,
            data: None,
        }
    }

    /// 承認のテーブルを照合する（保存 API と同じ DataStore を渡す・V4・GAP-08）。
    #[must_use]
    pub fn with_data(mut self, data: Arc<data::DataStore>) -> Self {
        self.data = Some(data);
        self
    }

    /// web.fetch の天井を照合する（`workflow.agent.web_allowlist`・保存 API と同じ値を渡す）。
    #[must_use]
    pub fn with_web_ceiling(mut self, hosts: Vec<String>) -> Self {
        self.web_ceiling = Some(hosts);
        self
    }
}

#[async_trait::async_trait]
impl chat::WorkflowCatalogSource for ApiWorkflowCatalogSource {
    async fn catalog(&self, ctx: &authz::AuthContext) -> Result<Catalog, String> {
        let mut catalog = build_catalog_from(
            self.secrets.as_deref(),
            self.skill_installs.as_deref(),
            &self.models,
            ctx,
        )
        .await
        .map_err(|e| format!("カタログ構築に失敗: {e}"))?;
        catalog.web_ceiling.clone_from(&self.web_ceiling);
        Ok(catalog)
    }

    /// 説明文用のモデル一覧（secret・skill の一覧は引かない・照合と同じ集合）。
    async fn models(&self, ctx: &authz::AuthContext) -> Result<Vec<String>, String> {
        self.models.ids(ctx).await
    }

    /// 説明文用の判定用のモデルの一覧（id と表示名・照合と同じ集合・D-79）。
    async fn decision_models(
        &self,
        ctx: &authz::AuthContext,
    ) -> Result<Vec<chat::DecisionModel>, String> {
        self.models.decision_models(ctx).await
    }

    /// 保存 API（`build_catalog_for`）と同じく、IR の承認が束縛するテーブルを保存者の権限で引いて足す。
    async fn catalog_for(
        &self,
        ctx: &authz::AuthContext,
        ir: &serde_json::Value,
    ) -> Result<Catalog, String> {
        let mut catalog = chat::WorkflowCatalogSource::catalog(self, ctx).await?;
        if let Some(data) = &self.data {
            catalog.tables = approval_tables(data, ctx, ir).await?;
        }
        Ok(catalog)
    }
}
