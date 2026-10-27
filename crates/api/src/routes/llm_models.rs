//! `GET /llm/models` — 選べるモデルの一覧（一般ユーザー向け・#553）。
//!
//! テナント管理者が絞った範囲（モデルカタログ ∩ テナントで有効）だけを返す。接続先や単価、
//! プロバイダの実モデル ID は返さない（選ぶのに要らない情報を利用者へ出さない）。
//!
//! `?purpose=workflow` はワークフローの `llm.invoke` のモデル欄向けで、保存時の照合・実行時の解決と同じ
//! 見え方（本人専用のモデルを呼び出し主体によらず除く）で返す（BUG-33・D-46）。既定・Auto の振り先も
//! その見え方で計算するので、モデル欄の「組織の既定に従う（現在: …）」が実際の解決と一致する。
//!
//! 返すのは既定で chat モデルだけ（判定用のモデルはチャット・`llm.invoke` で使えない・FEAT-1）。
//! `?modality=decision` は判定ノード（`ai.decide`）のモデル欄向けで、組織で使える判定用のモデルを返す。

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    routing::get,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::{
    error::ApiError,
    extract::AuthContextExt,
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn llm_model_route_decls() -> Vec<RouteDecl> {
    vec![RouteDecl::new(
        "/llm/models",
        &["GET"],
        AccessPolicy::Session,
        || get(list_models),
    )]
}

/// 思考強度（3 段階）。プロバイダ固有のノブは出さない（design §4.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
}

impl From<llm_gateway::Effort> for EffortLevel {
    fn from(e: llm_gateway::Effort) -> Self {
        match e {
            llm_gateway::Effort::Low => EffortLevel::Low,
            llm_gateway::Effort::Medium => EffortLevel::Medium,
            llm_gateway::Effort::High => EffortLevel::High,
        }
    }
}

impl From<EffortLevel> for llm_gateway::Effort {
    fn from(e: EffortLevel) -> Self {
        match e {
            EffortLevel::Low => llm_gateway::Effort::Low,
            EffortLevel::Medium => llm_gateway::Effort::Medium,
            EffortLevel::High => llm_gateway::Effort::High,
        }
    }
}

/// モデル一覧の用途（どの見え方で返すか）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurpose {
    /// チャット。呼び出した本人から見た一覧（本人専用のモデルは連携した本人にだけ出る）。
    #[default]
    Chat,
    /// ワークフローの `llm.invoke`。保存時の照合・実行時の解決と同じ一覧（本人専用のモデルは本人にも出さない）。
    Workflow,
}

/// 一覧に出すモデルの種類。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelModality {
    /// 文章を生成するモデル（チャット・`llm.invoke`）。
    #[default]
    Chat,
    /// 判定用のモデル（`ai.decide`）。
    Decision,
}

impl From<llm_gateway::Modality> for ModelModality {
    fn from(m: llm_gateway::Modality) -> Self {
        match m {
            llm_gateway::Modality::Chat => ModelModality::Chat,
            llm_gateway::Modality::Decision => ModelModality::Decision,
        }
    }
}

/// `GET /llm/models` のクエリ。
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListModelsQuery {
    /// 一覧の用途（省略は chat）。
    #[serde(default)]
    pub purpose: ModelPurpose,
    /// モデルの種類（省略は chat）。`decision` は判定ノードのモデル欄向けで、`auto` は常に `null`。
    #[serde(default)]
    pub modality: ModelModality,
}

/// 選べるモデル 1 件。
#[derive(Debug, Serialize, ToSchema)]
pub struct LlmModelOption {
    /// チャットへ送るモデル ID（論理名）。
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    /// プロバイダの表示名（例「Anthropic」「社内 GPU」）。
    pub provider_label: String,
    /// 日本国外で処理される（「国外処理」バッジ）。
    pub processed_abroad: bool,
    /// 思考の深さを選べるか。
    pub supports_effort: bool,
}

/// Auto の振り先（「Auto は何を使うか」の表示用）。
#[derive(Debug, Serialize, ToSchema)]
pub struct AutoTarget {
    pub model: String,
    pub label: String,
    pub effort: EffortLevel,
}

/// `GET /llm/models` のレスポンス。
#[derive(Debug, Serialize, ToSchema)]
pub struct LlmModelsResponse {
    /// 組織の既定（`auto` かモデル ID）。チャットの新しい会話と、ワークフローでモデルを省略したときに使う。
    pub default_model: String,
    /// モデルを選べるか（false ならチャットは選択 UI を出さず、ワークフローは既定以外のモデルを保存できない）。
    pub allow_user_choice: bool,
    /// 特定のモデルで動くとき（選んだモデル・既定の特定モデル）の思考の深さの初期値。
    pub default_effort: EffortLevel,
    /// Auto の振り先（使えるモデルが無い構成と、`modality=decision` では `null`）。
    pub auto: Option<AutoTarget>,
    pub models: Vec<LlmModelOption>,
}

/// gateway が無い（LLM 未構成）なら 503。
pub(crate) fn gateway(state: &AppState) -> Result<&Arc<llm_gateway::LlmGateway>, ApiError> {
    state
        .llm
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("llm gateway is not configured".into()))
}

/// プロバイダ ID → 表示名。
pub(crate) fn provider_label(gw: &llm_gateway::LlmGateway, provider_id: &str) -> String {
    gw.providers()
        .into_iter()
        .find(|p| p.id == provider_id)
        .map_or_else(|| provider_id.to_string(), |p| p.label)
}

/// 選べるモデルの一覧（既定はチャット・`purpose=workflow` はワークフローのモデル欄）。
#[utoipa::path(
    get,
    path = "/llm/models",
    params(ListModelsQuery),
    responses(
        (status = 200, description = "選べるモデル", body = LlmModelsResponse),
        (status = 401, description = "未認証"),
        (status = 503, description = "LLM 未構成"),
    ),
    security(("session" = [])),
)]
pub async fn list_models(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    Query(q): Query<ListModelsQuery>,
) -> Result<Json<LlmModelsResponse>, ApiError> {
    models_for(gateway(&state)?, &ctx, q.purpose, q.modality)
        .await
        .map(Json)
}

/// 用途ごとの見え方で、選べるモデルの一覧を組む（`modality` の種類だけ）。
pub(crate) async fn models_for(
    gw: &llm_gateway::LlmGateway,
    ctx: &authz::AuthContext,
    purpose: ModelPurpose,
    modality: ModelModality,
) -> Result<LlmModelsResponse, ApiError> {
    let settings = match purpose {
        // 呼び出した本人から見た設定（使えない本人専用のモデルを除き、既定・Auto もそれに従う・#854）。
        ModelPurpose::Chat => gw.settings_for(ctx).await,
        // ワークフローの照合・解決と同じ設定（本人専用のモデルは本人にも出さない・BUG-33）。
        ModelPurpose::Workflow => gw.settings_without_personal(ctx).await,
    }
    .map_err(|e| ApiError::Internal(format!("tenant llm settings: {e}")))?;
    let catalog = gw.catalog();
    let enabled = match modality {
        ModelModality::Chat => settings.enabled_chat_models(catalog),
        ModelModality::Decision => settings.enabled_decision_models(catalog),
    };
    let models = enabled
        .into_iter()
        .map(|m| LlmModelOption {
            id: m.id.clone(),
            label: m.label().to_string(),
            description: m.description.clone(),
            provider_label: provider_label(gw, m.provider_id()),
            processed_abroad: m.processed_abroad,
            supports_effort: gw.supports_effort(m),
        })
        .collect();
    // 判定用のモデルに Auto は無い（判定ノードはモデルを必ず書く）。
    let auto = (modality == ModelModality::Chat)
        .then(|| gw.auto_target(&settings))
        .flatten()
        .map(|(model, effort)| AutoTarget {
            label: catalog
                .get(&model)
                .map_or_else(|| model.clone(), |m| m.label().to_string()),
            model,
            effort: effort.into(),
        });
    Ok(LlmModelsResponse {
        default_model: settings.default_model,
        allow_user_choice: settings.allow_user_choice,
        default_effort: settings.default_effort.into(),
        auto,
        models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_gateway::{
        AutoConfig, ChatGptAccount, ChatGptSettings, GatewayConfig, LlmGateway, ModelCatalog,
        ModelEntry, ProviderConfig, ProviderKind, SettingsPatch, CHATGPT_PROVIDER_ID,
    };

    /// stub の fast・smart・判定用の decide ＋ ChatGPT プラン（chatgpt:gpt・Auto の振り先）。連携者は `ctx` の本人。
    fn gateway(pool: sqlx::PgPool, ctx: &authz::AuthContext, dir: &std::path::Path) -> LlmGateway {
        let path = dir.join("chatgpt.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "client_id": "oaiapp_1",
                "owner": {"tenant_id": ctx.tenant_id, "org": ctx.org, "principal_id": ctx.principal.id},
                "tokens": {
                    "access_token": "at",
                    "refresh_token": "rt",
                    "expires_at": "2999-01-01T00:00:00Z"
                }
            })
            .to_string(),
        )
        .unwrap();
        let entry = |id: &str| ModelEntry {
            id: id.into(),
            ..Default::default()
        };
        LlmGateway::build(
            pool,
            reqwest::Client::new(),
            GatewayConfig {
                extra_providers: vec![ProviderConfig {
                    id: CHATGPT_PROVIDER_ID.into(),
                    kind: ProviderKind::ChatgptPlan,
                    ..Default::default()
                }],
                catalog: ModelCatalog {
                    default_model: "fast".into(),
                    models: vec![
                        entry("fast"),
                        ModelEntry {
                            id: "decide".into(),
                            modality: llm_gateway::Modality::Decision,
                            ..Default::default()
                        },
                        entry("smart"),
                        ModelEntry {
                            id: "chatgpt:gpt".into(),
                            real_id: Some("gpt".into()),
                            provider: Some(CHATGPT_PROVIDER_ID.into()),
                            ..Default::default()
                        },
                    ],
                },
                auto: AutoConfig {
                    model: "chatgpt:gpt".into(),
                    ..AutoConfig::default()
                },
                chatgpt: Some(std::sync::Arc::new(ChatGptAccount::open(
                    reqwest::Client::new(),
                    ChatGptSettings::new(path),
                ))),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn ids(res: &LlmModelsResponse) -> Vec<&str> {
        res.models.iter().map(|m| m.id.as_str()).collect()
    }

    /// `purpose=workflow` は、連携者本人にも本人専用のモデルを出さず、保存時の照合（`WorkflowModels::ids`）と
    /// 同じ集合を返す。既定・Auto の振り先もその見え方で計算する（BUG-33・D-46）。
    /// 実 DB の組織の設定を読むので `STORAGE_TEST_DATABASE_URL` が無ければスキップ。
    #[tokio::test]
    #[allow(clippy::print_stderr)]
    async fn workflow_purpose_matches_the_workflow_model_check() {
        let Ok(url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
        let tenant = format!("llmm-{}", uuid::Uuid::new_v4().simple());
        let owner = authz::AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "owner".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(tenant.clone()),
            },
            "acme".into(),
            tenant,
        );
        let dir = std::env::temp_dir().join(format!("shiki-llmm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let gw = gateway(pool.clone(), &owner, &dir);

        // チャット: 連携者本人には本人専用のモデルが出て、Auto もそれに振られる。判定用のモデルは出ない。
        let chat = models_for(&gw, &owner, ModelPurpose::Chat, ModelModality::Chat)
            .await
            .unwrap();
        assert_eq!(ids(&chat), ["fast", "smart", "chatgpt:gpt"]);
        assert_eq!(chat.auto.as_ref().unwrap().model, "chatgpt:gpt");
        // ワークフロー: 本人でも出さず、Auto は次の候補（デプロイ既定の fast）。
        let wf = models_for(&gw, &owner, ModelPurpose::Workflow, ModelModality::Chat)
            .await
            .unwrap();
        assert_eq!(ids(&wf), ["fast", "smart"]);
        assert_eq!(wf.default_model, "auto");
        assert_eq!(wf.auto.as_ref().unwrap().model, "fast");
        let check = crate::workflow_catalog::WorkflowModels::new(Some(gw.clone()), Vec::new());
        assert_eq!(check.ids(&owner).await.unwrap(), ids(&wf));
        // 判定ノードのモデル欄: 判定用のモデルだけで、Auto は無い。
        let decide = models_for(&gw, &owner, ModelPurpose::Workflow, ModelModality::Decision)
            .await
            .unwrap();
        assert_eq!(ids(&decide), ["decide"]);
        assert!(decide.auto.is_none());

        // 管理者が既定を本人専用のモデルにし、選択を禁じても、ワークフローの既定は本人専用にならない。
        let patch = SettingsPatch {
            default_model: Some("chatgpt:gpt".into()),
            allow_user_choice: Some(false),
            ..Default::default()
        };
        gw.update_tenant_settings(&owner, &patch)
            .await
            .unwrap()
            .unwrap();
        let chat = models_for(&gw, &owner, ModelPurpose::Chat, ModelModality::Chat)
            .await
            .unwrap();
        assert_eq!(chat.default_model, "chatgpt:gpt");
        let wf = models_for(&gw, &owner, ModelPurpose::Workflow, ModelModality::Chat)
            .await
            .unwrap();
        assert_eq!(wf.default_model, "auto");
        assert!(!wf.allow_user_choice);
        assert_eq!(ids(&wf), ["fast", "smart"]);
        // 既定を本人専用にした保存の後も、判定用のモデルは許可リストに残る。
        let decide = models_for(&gw, &owner, ModelPurpose::Workflow, ModelModality::Decision)
            .await
            .unwrap();
        assert_eq!(ids(&decide), ["decide"]);
        for table in ["tenant_llm_model", "tenant_llm_settings"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
                .bind(&owner.tenant_id)
                .execute(&pool)
                .await
                .unwrap();
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn purpose_defaults_to_chat_and_parses_workflow() {
        let q: ListModelsQuery = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(q.purpose, ModelPurpose::Chat);
        assert_eq!(q.modality, ModelModality::Chat);
        let q: ListModelsQuery =
            serde_json::from_value(serde_json::json!({ "modality": "decision" })).unwrap();
        assert_eq!(q.modality, ModelModality::Decision);
        let q: ListModelsQuery =
            serde_json::from_value(serde_json::json!({ "purpose": "workflow" })).unwrap();
        assert_eq!(q.purpose, ModelPurpose::Workflow);
        assert!(
            serde_json::from_value::<ListModelsQuery>(serde_json::json!({ "purpose": "x" }))
                .is_err()
        );
    }
}
