//! ChatGPT プラン（#854）の gateway 側の関所: 呼び出し主体の照合と、モデル一覧での見え方。
//!
//! プラン枠は連携した本人のものなので、生成（[`LlmGateway::stream`]）の前に必ず照合する。
//! どの経路（チャット・エージェント・ワークフロー・ミニアプリ）も gateway を通るので、ここで
//! 拒否すれば本人以外がプラン枠を使う道は無い。

use std::sync::Arc;

use authz::AuthContext;

use super::LlmGateway;
use crate::chatgpt::ChatGptAccount;
use crate::config::{ModelEntry, ProviderKind};
use crate::provider::LlmError;

impl LlmGateway {
    /// ChatGPT プランの接続（`llm.chatgpt` を有効にしたときだけ）。
    pub fn chatgpt(&self) -> Option<&Arc<ChatGptAccount>> {
        self.inner.config.chatgpt.as_ref()
    }

    /// 呼び出し主体が ChatGPT プランの連携者か（違えば生成させない）。
    pub(super) async fn ensure_chatgpt_owner(&self, ctx: &AuthContext) -> Result<(), LlmError> {
        match self.chatgpt() {
            Some(account) => account.ensure_owner(ctx).await,
            None => Err(LlmError::Config("chatgpt_plan requires llm.chatgpt".into())),
        }
    }

    /// モデル選択に出してよいか。ChatGPT プランのモデルは連携者にだけ見せる
    /// （他の利用者には選んでも使えないモデルになるため）。
    pub async fn model_visible_to(&self, ctx: &AuthContext, entry: &ModelEntry) -> bool {
        let chatgpt = self
            .inner
            .providers
            .get(entry.provider_id())
            .is_some_and(|s| s.config.kind == ProviderKind::ChatgptPlan);
        if !chatgpt {
            return true;
        }
        match self.chatgpt() {
            Some(account) => account.is_owner(ctx).await,
            None => false,
        }
    }
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
            "t".into(),
        )
    }

    /// 既定（stub）＋ ChatGPT プランの構成。資格情報ファイルの連携者は `owner`。
    fn gateway(dir: &std::path::Path) -> LlmGateway {
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
        let account = ChatGptAccount::open(reqwest::Client::new(), ChatGptSettings::new(path));
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

        assert!(gw.model_visible_to(&owner, &chatgpt).await);
        assert!(!gw.model_visible_to(&other, &chatgpt).await);
        assert!(!gw.model_visible_to(&workflow, &chatgpt).await);
        assert!(gw.model_visible_to(&other, &stub).await);

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
