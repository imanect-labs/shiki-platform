//! ChatGPT プラン（#854）の gateway 側の関所: 呼び出し主体の照合と、モデル一覧での見え方。
//!
//! プラン枠は連携した本人のものなので、生成（[`LlmGateway::stream`]）の前に必ず照合する。
//! どの経路（チャット・エージェント・ワークフロー・ミニアプリ）も gateway を通るので、ここで
//! 拒否すれば本人以外がプラン枠を使う道は無い。
//!
//! 組織の既定や Auto の振り先に本人専用のモデルが選ばれていても、他の利用者が壊れないよう、
//! モデルの解決は呼び出し主体から見た設定（[`LlmGateway::settings_for`]）で行う。本人以外には
//! 本人専用のモデルが無効に見えるので、既定は Auto へ、Auto は次の候補へ落ちる。

use std::sync::Arc;

use authz::AuthContext;

use super::LlmGateway;
use crate::chatgpt::{ChatGptAccount, ChatGptPlanProvider};
use crate::config::{GatewayConfig, ModelEntry, ProviderKind};
use crate::provider::LlmError;
use crate::tenant::{TenantLlmSettings, AUTO_MODEL};

impl LlmGateway {
    /// ChatGPT プランの接続（`llm.chatgpt` を有効にしたときだけ）。
    pub fn chatgpt(&self) -> Option<&Arc<ChatGptAccount>> {
        self.inner.config.chatgpt.as_ref()
    }

    /// 呼び出し主体が ChatGPT プランの連携者か（違えば生成させない）。
    pub(super) fn ensure_chatgpt_owner(&self, ctx: &AuthContext) -> Result<(), LlmError> {
        match self.chatgpt() {
            Some(account) => account.ensure_owner(ctx),
            None => Err(LlmError::Config("chatgpt_plan requires llm.chatgpt".into())),
        }
    }

    /// 連携した本人だけが使えるモデル（ChatGPT プラン）か。
    pub fn is_personal_model(&self, entry: &ModelEntry) -> bool {
        self.inner
            .providers
            .get(entry.provider_id())
            .is_some_and(|s| s.config.kind == ProviderKind::ChatgptPlan)
    }

    /// モデル選択に出してよいか。ChatGPT プランのモデルは連携者にだけ見せる
    /// （他の利用者には選んでも使えないモデルになるため）。
    pub fn model_visible_to(&self, ctx: &AuthContext, entry: &ModelEntry) -> bool {
        if !self.provider_allowed_for(ctx, entry.provider_id()) {
            return false;
        }
        if !self.is_personal_model(entry) {
            return true;
        }
        match self.chatgpt() {
            Some(account) => account.is_owner(ctx),
            None => false,
        }
    }

    /// 呼び出し主体から見た組織の AI 設定（モデルの解決・利用者向けの一覧に使う）。
    ///
    /// 使えない本人専用のモデルを許可リストから外し、既定がそれなら Auto にする。保存はしない
    /// （管理画面は [`Self::tenant_settings`] で組織の設定そのものを読む）。
    pub async fn settings_for(&self, ctx: &AuthContext) -> Result<TenantLlmSettings, sqlx::Error> {
        let catalog = &self.inner.config.catalog;
        let settings = self.inner.tenants.load(ctx).await?.reconciled(catalog);
        let mut hidden = Vec::new();
        for m in &catalog.models {
            if !self.model_visible_to(ctx, m) {
                hidden.push(m.id.as_str());
            }
        }
        Ok(without_models(catalog, settings, &hidden))
    }
}

/// ChatGPT プランのアダプタを組む（`chatgpt_plan` のプロバイダがあるときだけ）。接続
/// （[`GatewayConfig::chatgpt`]）が無ければ構築を失敗させる。
pub(super) fn plan_provider(
    http: &reqwest::Client,
    config: &GatewayConfig,
) -> Result<Option<Arc<ChatGptPlanProvider>>, LlmError> {
    let Some(pc) = std::iter::once(&config.provider)
        .chain(config.extra_providers.iter())
        .find(|p| p.kind == ProviderKind::ChatgptPlan)
    else {
        return Ok(None);
    };
    let account = config
        .chatgpt
        .clone()
        .ok_or_else(|| LlmError::Config("chatgpt_plan requires llm.chatgpt".into()))?;
    // アダプタの既定モデルは保険（gateway は常に実 ID を詰めて渡す）。このプロバイダの先頭のモデル。
    let catalog = &config.catalog;
    let default_real = catalog
        .models
        .iter()
        .find(|m| m.provider_id() == pc.effective_id())
        .map_or_else(
            || catalog.default_model.clone(),
            |m| m.resolved_id().to_string(),
        );
    Ok(Some(Arc::new(ChatGptPlanProvider::new(
        http.clone(),
        account,
        default_real,
        config.timeouts,
    ))))
}

/// `hidden` のモデルを使えないものとして見せる（既定がそれなら Auto）。
pub(super) fn without_models(
    catalog: &crate::config::ModelCatalog,
    mut settings: TenantLlmSettings,
    hidden: &[&str],
) -> TenantLlmSettings {
    if hidden.is_empty() {
        return settings;
    }
    let allowed = settings
        .enabled_models(catalog)
        .into_iter()
        .filter(|m| !hidden.contains(&m.id.as_str()))
        .map(|m| m.id.clone())
        .collect();
    settings.allowed_models = Some(allowed);
    if hidden.contains(&settings.default_model.as_str()) {
        settings.default_model = AUTO_MODEL.to_string();
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatgpt::{ChatGptSettings, CHATGPT_PROVIDER_ID};
    use crate::config::{GatewayConfig, ModelCatalog, ProviderConfig};
    use crate::model::{GenerateRequest, Message, Role};

    fn ctx(kind: authz::PrincipalKind, id: &str) -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind,
                id: id.into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t".into()),
            },
            "o".into(),
            authz::TenantId::parse("t").unwrap(),
        )
    }

    /// 既定（stub）＋ ChatGPT プランの構成。資格情報ファイルの連携者は `owner`。
    fn gateway(dir: &std::path::Path) -> LlmGateway {
        gateway_at(dir, None)
    }

    /// [`gateway`] の推論 API の送り先を `api_base` にしたもの（無ければ既定の送り先）。
    fn gateway_at(dir: &std::path::Path, api_base: Option<&str>) -> LlmGateway {
        let path = dir.join("chatgpt.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version": 1,
                "client_id": "oaiapp_1",
                "owner": {"tenant_id": "t", "org": "o", "principal_id": "owner"},
                "tokens": {
                    "access_token": "at",
                    "refresh_token": "rt",
                    "expires_at": "2999-01-01T00:00:00Z"
                }
            })
            .to_string(),
        )
        .unwrap();
        let mut settings = ChatGptSettings::new(path);
        if let Some(base) = api_base {
            settings.api_base = base.to_string();
        }
        let account = ChatGptAccount::open(reqwest::Client::new(), settings);
        let config = GatewayConfig {
            extra_providers: vec![ProviderConfig {
                id: CHATGPT_PROVIDER_ID.into(),
                kind: ProviderKind::ChatgptPlan,
                ..Default::default()
            }],
            catalog: ModelCatalog {
                default_model: "m".into(),
                models: vec![
                    ModelEntry {
                        id: "m".into(),
                        ..Default::default()
                    },
                    ModelEntry {
                        id: "chatgpt:gpt".into(),
                        real_id: Some("gpt".into()),
                        provider: Some(CHATGPT_PROVIDER_ID.into()),
                        ..Default::default()
                    },
                ],
            },
            chatgpt: Some(Arc::new(account)),
            ..Default::default()
        };
        let db = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(500))
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
            .unwrap();
        LlmGateway::build(db, reqwest::Client::new(), config).unwrap()
    }

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("shiki-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // connect_lazy は tokio ランタイムを要する。
    #[tokio::test]
    async fn only_the_owner_sees_and_uses_chatgpt_models() {
        let dir = temp_dir();
        let gw = gateway(&dir);
        let chatgpt = gw.catalog().get("chatgpt:gpt").unwrap().clone();
        let stub = gw.catalog().get("m").unwrap().clone();
        let owner = ctx(authz::PrincipalKind::User, "owner");
        let other = ctx(authz::PrincipalKind::User, "someone");
        // 同じ id でもワークフロー等の実行主体は本人ではない。
        let workflow = ctx(authz::PrincipalKind::Workflow, "owner");

        assert!(gw.model_visible_to(&owner, &chatgpt));
        assert!(!gw.model_visible_to(&other, &chatgpt));
        assert!(!gw.model_visible_to(&workflow, &chatgpt));
        assert!(gw.model_visible_to(&other, &stub));

        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        req.model = Some("chatgpt:gpt".into());
        for caller in [&other, &workflow] {
            let res = gw.stream(caller, req.clone()).await;
            assert!(
                matches!(&res, Err(LlmError::Config(m)) if m.contains("連携した本人")),
                "{:?}",
                res.err()
            );
        }
        // 本人は関所を通り、組織の設定の確認へ進む（ここでは DB が無いので Unavailable）。
        assert!(matches!(
            gw.stream(&owner, req).await,
            Err(LlmError::Unavailable(_))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 呼ばれるたびに用意した Responses API の本文を順に返すモック（尽きたら最後の本文）。呼ばれた回数を数える。
    async fn scripted_responses(
        bodies: Vec<&'static str>,
    ) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let app = axum::Router::new().fallback(move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let body = bodies[n.min(bodies.len() - 1)];
            async move { ([("content-type", "text/event-stream")], body) }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, calls)
    }

    /// ChatGPT プランも、思考だけを流して切れた応答は送り直す（BUG-224）。送り直しも連携者の本人の呼び出しとして
    /// 照合し直し、本人でない呼び出しは送り直しの経路に乗っても送らない（D-30）。
    #[tokio::test]
    async fn chatgpt_plan_streams_cut_before_anything_settles_are_sent_again() {
        use futures::StreamExt;
        use std::sync::atomic::Ordering;
        const THINK_THEN_CLOSE: &str =
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"考え中\"}\n\n";
        const ANSWER: &str = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"答え\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{}}}\n\n",
        );
        let (base, calls) = scripted_responses(vec![THINK_THEN_CLOSE, ANSWER]).await;
        let dir = temp_dir();
        let gw = gateway_at(&dir, Some(&base));
        let entry = gw.catalog().get("chatgpt:gpt").unwrap().clone();
        let slot = gw.slot_for(&entry).unwrap();
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "hi")]);
        req.model = Some("gpt".into());

        let owner = ctx(authz::PrincipalKind::User, "owner");
        let out: Vec<String> = gw
            .open_stream(&owner, slot, req.clone())
            .await
            .unwrap()
            .map(|d| match d {
                Ok(crate::model::StreamDelta::TextDelta { text }) => format!("text:{text}"),
                Ok(crate::model::StreamDelta::ThinkingDelta { text }) => format!("think:{text}"),
                Ok(crate::model::StreamDelta::Done { .. }) => "done".into(),
                Ok(other) => format!("{other:?}"),
                Err(e) => format!("err:{e}"),
            })
            .collect()
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "{out:?}");
        assert_eq!(
            out,
            ["think:考え中", "think:\n\n", "text:答え", "done"],
            "{out:?}"
        );

        // 本人でない呼び出しは、トークンの取得で断る（推論 API へは送らない）。
        let other = ctx(authz::PrincipalKind::User, "someone");
        assert!(matches!(
            gw.open_stream(&other, slot, req).await,
            Err(LlmError::Config(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn others_fall_back_when_the_org_picks_a_personal_model() {
        use crate::config::AutoConfig;
        use crate::tenant::{resolve_choice, ChoiceError, ModelChoice};
        let catalog = ModelCatalog {
            default_model: "m".into(),
            models: ["m", "n", "chatgpt:gpt"]
                .into_iter()
                .map(|id| ModelEntry {
                    id: id.into(),
                    ..Default::default()
                })
                .collect(),
        };
        // 管理者が既定にも Auto の振り先にも ChatGPT のモデルを選び、選択も禁止した組織。
        let org = TenantLlmSettings {
            default_model: "chatgpt:gpt".into(),
            auto_model: Some("chatgpt:gpt".into()),
            allow_user_choice: false,
            ..Default::default()
        };
        let auto = AutoConfig {
            model: "n".into(),
            ..Default::default()
        };
        let resolve = |s: &TenantLlmSettings, model: Option<&str>| {
            resolve_choice(
                &catalog,
                &auto,
                s,
                |_| true,
                &ModelChoice {
                    model: model.map(str::to_string),
                    effort: None,
                },
            )
        };
        // 本人（何も隠さない）は ChatGPT のモデルで動く。
        let mine = without_models(&catalog, org.clone(), &[]);
        assert_eq!(resolve(&mine, None).unwrap().model, "chatgpt:gpt");

        // 他の利用者には無効に見え、既定は Auto へ、Auto は次の候補（デプロイ設定）へ落ちる。
        let others = without_models(&catalog, org.clone(), &["chatgpt:gpt"]);
        assert_eq!(others.default_model, AUTO_MODEL);
        let got = resolve(&others, None).unwrap();
        assert_eq!((got.model.as_str(), got.auto), ("n", true));
        // 明示しても使えない（400）。組織の設定そのものは変えない。
        let mut chooser = others.clone();
        chooser.allow_user_choice = true;
        assert_eq!(
            resolve(&chooser, Some("chatgpt:gpt")),
            Err(ChoiceError::ModelNotAllowed("chatgpt:gpt".into()))
        );
        assert_eq!(org.default_model, "chatgpt:gpt");
    }

    #[tokio::test]
    async fn chatgpt_provider_without_an_account_fails_to_build() {
        let config = GatewayConfig {
            provider: ProviderConfig {
                kind: ProviderKind::ChatgptPlan,
                ..Default::default()
            },
            catalog: ModelCatalog {
                default_model: "m".into(),
                models: vec![ModelEntry {
                    id: "m".into(),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        let db = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
            .unwrap();
        assert!(matches!(
            LlmGateway::build(db, reqwest::Client::new(), config),
            Err(LlmError::Config(_))
        ));
    }
}
