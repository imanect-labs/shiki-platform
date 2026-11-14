//! ユーザー別の許可リストは、一覧と生成と判定（`ai.decide`）で同じ判定を通す。
use std::collections::HashMap;

use authz::{AuthContext, PrincipalKind};

use super::{LlmGateway, ProviderSlot};
use crate::config::{GatewayConfig, ModelEntry};
use crate::provider::LlmError;

pub(super) type UserProviders = std::collections::HashMap<
    String,
    std::collections::HashMap<String, std::collections::HashSet<String>>,
>;

/// 設定の規則（`user_provider_rules`）から許可リストを組む（起動時・不正な規則は起動を止める）。
///
/// 同じテナント・利用者の規則が複数あれば、許すプロバイダはその積にする（広げない）。
pub(super) fn build_user_providers(
    config: &GatewayConfig,
    providers: &HashMap<String, ProviderSlot>,
) -> Result<UserProviders, LlmError> {
    let mut user_providers = UserProviders::new();
    for rule in &config.user_provider_rules {
        if authz::validate_tenant_id(&rule.tenant_id).is_err()
            || rule.user_sub.trim().is_empty()
            || rule.user_sub.trim() != rule.user_sub
        {
            return Err(LlmError::Config(
                "user provider rule requires a valid tenant_id and nonempty, unpadded user_sub"
                    .into(),
            ));
        }
        if let Some(id) = rule
            .providers
            .iter()
            .find(|id| !providers.contains_key(*id))
        {
            return Err(LlmError::Config(format!(
                "user provider rule refers to unknown provider: {id}"
            )));
        }
        let users = user_providers.entry(rule.tenant_id.clone()).or_default();
        if let Some(allowed) = users.get_mut(&rule.user_sub) {
            allowed.retain(|id| rule.providers.contains(id));
        } else {
            users.insert(
                rule.user_sub.clone(),
                rule.providers.iter().cloned().collect(),
            );
        }
    }
    Ok(user_providers)
}

impl LlmGateway {
    /// `entry` のプロバイダをこの利用者に使わせてよいか（生成と判定の入口・許されなければ設定の誤りとして断る）。
    pub(super) fn ensure_provider_allowed(
        &self,
        ctx: &AuthContext,
        entry: &ModelEntry,
    ) -> Result<(), LlmError> {
        if self.provider_allowed_for(ctx, entry.provider_id()) {
            Ok(())
        } else {
            Err(LlmError::Config(
                "provider is not enabled for this user".into(),
            ))
        }
    }

    pub(super) fn provider_allowed_for(&self, ctx: &AuthContext, provider: &str) -> bool {
        if ctx.principal().kind != PrincipalKind::User {
            return true;
        }
        self.inner
            .user_providers
            .get(ctx.tenant_id())
            .and_then(|users| users.get(&ctx.principal().id))
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
            authz::TenantId::parse(tenant).unwrap(),
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

    /// 判定（`ai.decide`）も生成と同じ判定を通す。許されないプロバイダの判定用のモデルは、DB と決定器に触れる前に
    /// 断る（許された利用者は先へ進み、届かない DB で止まる）。
    #[tokio::test]
    async fn decision_is_denied_before_database_or_decider_access() {
        let gw = gateway();
        let mut config = gw.inner.config.clone();
        config.catalog.models.push(ModelEntry {
            id: "decide-hidden".into(),
            modality: crate::config::Modality::Decision,
            ..Default::default()
        });
        // 許された利用者は届かない DB で止まる（待たない）。
        let db = PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(300))
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let gw = LlmGateway::build(db, reqwest::Client::new(), config).unwrap();
        let req = crate::DecideRequest {
            model: "decide-hidden".into(),
            state: serde_json::json!({ "body": "x" }),
            questions: vec![crate::Question {
                key: "angry".into(),
                instructions: "強い不満を示しているか".into(),
                kind: crate::QuestionKind::Boolean {
                    yes: None,
                    no: None,
                },
            }],
        };
        let call = crate::DecideCall {
            idempotency_key: "k".into(),
            trace_id: None,
            source: crate::UsageSource::Workflow,
            app_id: None,
        };
        let denied = gw.decide(&ctx("tenant", "restricted"), &req, &call).await;
        assert!(
            matches!(&denied, Err(LlmError::Config(message)) if message.contains("not enabled for this user")),
            "{denied:?}"
        );
        let other = gw.decide(&ctx("tenant", "other"), &req, &call).await;
        assert!(
            !matches!(&other, Err(LlmError::Config(message)) if message.contains("not enabled for this user")),
            "{other:?}"
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
        let workflow = AuthContext::for_workflow(
            authz::TenantId::parse("tenant").unwrap(),
            "org".into(),
            "restricted",
        );
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
