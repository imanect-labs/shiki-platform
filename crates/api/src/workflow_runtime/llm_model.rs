//! ワークフローのモデル解決（BUG-12・D-5・D-23・D-30・`ports.rs` から分割）。
//!
//! `llm.invoke` と instructions だけの skill は、ノードを実行するたびに（attempt ごとに）実行主体の組織の
//! 設定でモデルを決める。規則は llm-gateway の [`LlmGateway::resolve_node_choice`] にある。
//! - 省略（空文字を含む）は組織の既定（Auto を含む）。管理者が既定を変えると次の実行から変わる。
//! - 明示したモデルが組織で使えなくなっていれば、既定へ戻さずに失敗する（fail-closed）。
//! - 組織がモデルの選択を禁じていれば、既定と違うモデルは失敗する。
//! - 本人専用のモデル（ChatGPT プラン）は起動した人によらず使わない（誰が起動しても同じモデル）。
//!
//! 失敗は設定を直すまで通らないので再試行しない。組織の設定を読めないときだけ再試行する。

use authz::AuthContext;
use llm_gateway::{ChoiceError, LlmGateway, ResolvedChoice};
use workflow_engine::{ExecCtx, PortError};

/// モデルが使えない（未登録・組織で無効・選択の禁止）ときの step の誤りの code。
const MODEL_UNAVAILABLE: &str = "model_unavailable";

/// 実行のたびに実行主体の組織の設定でモデルを解決する。
pub(super) async fn resolve_for_workflow(
    gateway: &LlmGateway,
    ec: &ExecCtx,
    ctx: &AuthContext,
    model: Option<&str>,
) -> Result<ResolvedChoice, PortError> {
    match gateway.resolve_node_choice(ctx, model, None).await {
        Ok(Ok(choice)) => Ok(choice),
        Ok(Err(e)) => Err(choice_port_error(gateway, &e)),
        Err(e) => {
            // DB の詳細は step に出さない（どの run のどの step かで引けるようにログへ残す）。
            tracing::warn!(
                target: "workflow.llm",
                tenant = %ec.tenant_id,
                run_id = ?ec.run_id,
                step = ?ec.step_path,
                error = %e,
                "組織の AI 設定を読めなかった"
            );
            Err(PortError::unavailable(
                "組織の AI の設定を読めませんでした。しばらくしてからもう一度実行してください。",
            ))
        }
    }
}

/// 解決の失敗を step の誤りへ写す（どれも設定を直すまで通らないので再試行しない）。
fn choice_port_error(gateway: &LlmGateway, e: &ChoiceError) -> PortError {
    match e {
        ChoiceError::ModelNotAllowed(model) if gateway.catalog().get(model).is_none() => {
            unknown_model(model)
        }
        ChoiceError::ModelNotAllowed(model) => disabled_model(model),
        ChoiceError::ChoiceDisabled => PortError::new(
            "model_choice_disabled",
            "この組織ではモデルを選べません。ノードの設定を「組織の既定に従う」にしてください。",
            false,
        ),
        ChoiceError::NoModel => PortError::new(
            "no_model",
            "組織で使えるモデルがありません。管理者に AI の設定を確認してもらってください。",
            false,
        ),
    }
}

/// カタログに無いモデル（ノードの設定に書いた値なので名前を出す）。
pub(super) fn unknown_model(model: &str) -> PortError {
    PortError::new(
        MODEL_UNAVAILABLE,
        format!(
            "AI モデル「{model}」は登録されていません。\
             ノードの設定でモデルを選び直すか、「組織の既定に従う」にしてください。"
        ),
        false,
    )
}

/// 組織で使えないモデル（無効にされた・本人専用）。
pub(super) fn disabled_model(model: &str) -> PortError {
    PortError::new(
        MODEL_UNAVAILABLE,
        format!(
            "AI モデル「{model}」はこの組織では使えません。\
             ノードの設定でモデルを選び直すか、「組織の既定に従う」にしてください。"
        ),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_gateway::{GatewayConfig, ModelCatalog, ModelEntry};

    // connect_lazy は tokio ランタイムを要する。
    async fn gateway() -> LlmGateway {
        let db = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(500))
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/none")
            .unwrap();
        let config = GatewayConfig {
            catalog: ModelCatalog {
                default_model: "fast".into(),
                models: vec![ModelEntry {
                    id: "fast".into(),
                    ..Default::default()
                }],
            },
            ..Default::default()
        };
        LlmGateway::build(db, reqwest::Client::new(), config).unwrap()
    }

    #[tokio::test]
    async fn choice_errors_are_permanent_and_in_japanese() {
        let gw = gateway().await;
        let unknown = choice_port_error(&gw, &ChoiceError::ModelNotAllowed("ghost".into()));
        assert_eq!(unknown.code, MODEL_UNAVAILABLE);
        assert!(
            unknown
                .message
                .starts_with("AI モデル「ghost」は登録されていません。"),
            "{}",
            unknown.message
        );
        let disabled = choice_port_error(&gw, &ChoiceError::ModelNotAllowed("fast".into()));
        assert_eq!(disabled.code, MODEL_UNAVAILABLE);
        assert!(
            disabled
                .message
                .starts_with("AI モデル「fast」はこの組織では使えません。"),
            "{}",
            disabled.message
        );
        let locked = choice_port_error(&gw, &ChoiceError::ChoiceDisabled);
        assert_eq!(locked.code, "model_choice_disabled");
        let none = choice_port_error(&gw, &ChoiceError::NoModel);
        assert_eq!(none.code, "no_model");
        for e in [&unknown, &disabled, &locked, &none] {
            assert!(!e.retryable, "{}", e.code);
            assert!(e.message.contains("組織"), "{}", e.message);
        }
    }

    /// 組織の設定を読めないとき（DB 不達）は一時障害として再試行し、DB の詳細を出さない。
    #[tokio::test]
    async fn unreadable_settings_are_retried() {
        let gw = gateway().await;
        let ec = ExecCtx {
            tenant_id: "t1".into(),
            org: "acme".into(),
            principal: "alice".into(),
            principal_kind: "user".into(),
            trace_id: None,
            run_id: None,
            step_path: Some("ai".into()),
        };
        let ctx = super::super::auth::auth_ctx(&ec);
        let e = resolve_for_workflow(&gw, &ec, &ctx, None)
            .await
            .unwrap_err();
        assert_eq!(e.code, "unavailable");
        assert!(e.retryable);
        assert!(!e.message.contains("127.0.0.1"), "{}", e.message);
    }
}
