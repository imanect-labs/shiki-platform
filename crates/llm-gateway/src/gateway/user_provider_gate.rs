//! ユーザー別の許可リストは、一覧と生成の両方で同じ判定を通す。
use authz::{AuthContext, PrincipalKind};

use super::LlmGateway;

pub(super) type UserProviders = std::collections::HashMap<
    String,
    std::collections::HashMap<String, std::collections::HashSet<String>>,
>;

impl LlmGateway {
    pub(super) fn provider_allowed_for(&self, ctx: &AuthContext, provider: &str) -> bool {
        if ctx.principal.kind != PrincipalKind::User {
            return true;
        }
        self.inner
            .user_providers
            .get(&ctx.tenant_id)
            .and_then(|users| users.get(&ctx.principal.id))
            .is_none_or(|providers| providers.contains(provider))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        GatewayConfig, ModelCatalog, ModelEntry, ProviderConfig, UserProviderRule,
    };
    use crate::model::GenerateRequest;
    use crate::provider::LlmError;
    use sqlx::postgres::PgPoolOptions;

    fn ctx(tenant: &str, user: &str) -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: PrincipalKind::User,
                id: user.into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(tenant.into()),
            },
            "org".into(),
            tenant.into(),
        )
    }

    fn gateway() -> LlmGateway {
        let config = GatewayConfig {
            extra_providers: vec![ProviderConfig {
                id: "allowed".into(),
                ..Default::default()
            }],
            user_provider_rules: vec![UserProviderRule {
                tenant_id: "tenant".into(),
                user_sub: "restricted".into(),
                providers: vec!["allowed".into()],
            }],
            catalog: ModelCatalog {
                default_model: "hidden".into(),
                models: vec![
                    ModelEntry {
                        id: "hidden".into(),
                        ..Default::default()
                    },
                    ModelEntry {
                        id: "visible".into(),
                        provider: Some("allowed".into()),
                        ..Default::default()
                    },
                ],
            },
            ..Default::default()
        };
        let db = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        LlmGateway::build(db, reqwest::Client::new(), config).unwrap()
    }

    #[tokio::test]
    async fn visibility_is_scoped_to_tenant_and_subject() {
        let gw = gateway();
        let catalog = gw.catalog();
        let hidden = catalog.get("hidden").unwrap();
        let visible = catalog.get("visible").unwrap();
        assert!(!gw.model_visible_to(&ctx("tenant", "restricted"), hidden));
        assert!(gw.model_visible_to(&ctx("tenant", "restricted"), visible));
        assert!(gw.model_visible_to(&ctx("other", "restricted"), hidden));
        assert!(gw.model_visible_to(&ctx("tenant", "other"), hidden));
        assert!(!gw.provider_allowed_for(&ctx("tenant", "restricted"), "future-provider"));
    }

    #[tokio::test]
    async fn explicit_model_is_denied_before_database_or_provider_access() {
        let gw = gateway();
        let req = GenerateRequest {
            model: Some("hidden".into()),
            ..GenerateRequest::new(vec![])
        };
        let result = gw.stream(&ctx("tenant", "restricted"), req).await;
        assert!(
            matches!(result, Err(LlmError::Config(message)) if message.contains("not enabled for this user"))
        );
    }

    #[tokio::test]
    async fn unknown_provider_rule_fails_at_startup() {
        let gw = gateway();
        let mut config = gw.inner.config.clone();
        config.user_provider_rules[0].providers = vec!["typo".into()];
        let db = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        assert!(LlmGateway::build(db, reqwest::Client::new(), config).is_err());
    }
    #[tokio::test]
    async fn duplicate_rules_intersect_and_empty_identity_is_rejected() {
        let gw = gateway();
        let mut config = gw.inner.config.clone();
        config.user_provider_rules.push(UserProviderRule {
            tenant_id: "tenant".into(),
            user_sub: "restricted".into(),
            providers: vec!["default".into()],
        });
        let db = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let narrowed =
            LlmGateway::build(db.clone(), reqwest::Client::new(), config.clone()).unwrap();
        assert!(!narrowed.provider_allowed_for(&ctx("tenant", "restricted"), "allowed"));
        assert!(!narrowed.provider_allowed_for(&ctx("tenant", "restricted"), "default"));
        let mut workflow = ctx("tenant", "restricted");
        workflow.principal.kind = PrincipalKind::Workflow;
        assert!(narrowed.provider_allowed_for(&workflow, "default"));
        config.user_provider_rules[0].tenant_id.clear();
        assert!(LlmGateway::build(db.clone(), reqwest::Client::new(), config.clone()).is_err());
        config.user_provider_rules[0].tenant_id = "tenant".into();
        config.user_provider_rules[0].user_sub.clear();
        assert!(LlmGateway::build(db, reqwest::Client::new(), config).is_err());
    }
    #[tokio::test]
    async fn invalid_identifiers_are_rejected_instead_of_silently_unrestricting() {
        let gw = gateway();
        for (tenant, subject) in [
            ("tenant ", "restricted"),
            ("tenant/name", "restricted"),
            ("te nant", "restricted"),
            (" tenant", "restricted"),
            ("tenant", "restricted "),
            ("tenant", " restricted"),
        ] {
            let mut config = gw.inner.config.clone();
            config.user_provider_rules[0].tenant_id = tenant.into();
            config.user_provider_rules[0].user_sub = subject.into();
            let db = PgPoolOptions::new()
                .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
                .unwrap();
            assert!(LlmGateway::build(db, reqwest::Client::new(), config).is_err());
        }
    }
}
