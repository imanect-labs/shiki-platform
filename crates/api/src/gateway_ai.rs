//! ゲートウェイ agent.invoke の port 実装（Task 9.9）。
//!
//! agent-core `run_agent` を呼出ユーザーの [`AuthContext`] で起動する（昇格しない＝
//! confused-deputy 防御）。提示ツール = **アプリ宣言 ∩ ToolName 閉集合 ∩ 実配線**。
//! doc_search は permission-aware 検索（SearchService・pre/post-filter 二段 authz）を
//! そのまま使うため、アプリ経由でも非可読文書は混入しない。doc_read（#425）も chat と同じ
//! 実装（StorageService の viewer 判定＋可読集合の pre-filter）を通る。
//! 読み取り系ツールは、対応する API と同じ能力スコープがアプリに効いているときだけ提示する
//! （doc_search = `rag.query`・doc_read = `storage.read`）。agent.invoke を scope の迂回路にしない。
//! LLM 呼び出しは llm-gateway を通り `app_id` 付きで `llm_usage` に計上される。

use std::sync::Arc;

use agent_core::{
    run_agent, AgentEvent, AgentOptions, DocSearchTool, EventSink, RunContext, Tool, ToolName,
};
use app_gateway::{AgentInvokeSpec, AgentPort, AiEvent, AiEventStream, GatewayError};
use authz::{AuthContext, CapabilityScope};
use llm_gateway::{LlmGateway, Message as LlmMessage, Role as LlmRole};
use serde_json::json;
use tokio::sync::mpsc;

/// agent.invoke の 1 実行あたりの既定/上限ステップ数。
const DEFAULT_MAX_STEPS: usize = 8;
const MAX_STEPS_CEILING: usize = 24;

pub(crate) struct GatewayAgentPort {
    pub llm: Arc<LlmGateway>,
    pub search: Option<Arc<rag::SearchService>>,
    /// doc_read の認可（viewer 判定）に使う。search と両方そろって doc_read を配線する。
    pub storage: Option<Arc<storage::StorageService>>,
}

/// 宣言ツール名を閉集合（ToolName）で照合し、実配線があるものだけ構築する。
///
/// 未知名・未配線は**黙って落とす**のではなく呼び出し側へ返す（`skipped`・SSE 冒頭で通知）。
fn build_tools(
    declared: &[String],
    scopes: &[CapabilityScope],
    search: Option<&Arc<rag::SearchService>>,
    storage: Option<&Arc<storage::StorageService>>,
) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut skipped = Vec::new();
    let allowed = |tool: ToolName| required_scope(tool).is_none_or(|s| scopes.contains(&s));
    // doc_read が実際に提示されるときだけ、doc_search の説明で本文の読み方を案内する。
    let doc_read_wired = search.is_some()
        && storage.is_some()
        && allowed(ToolName::DocRead)
        && declared
            .iter()
            .any(|d| ToolName::parse(d) == Some(ToolName::DocRead));
    let mut seen = std::collections::HashSet::new();
    for name in declared {
        // 同じツールの重複宣言は 1 つにまとめる（同名ツールが 2 つあるとプロバイダが拒否する）。
        if !seen.insert(name.as_str()) {
            continue;
        }
        match ToolName::parse(name) {
            // 対応する API のスコープが効いていない読み取り系ツールは提示しない。
            Some(tool) if !allowed(tool) => skipped.push(name.clone()),
            Some(ToolName::DocSearch) => {
                if let Some(s) = search {
                    let tool = DocSearchTool::with_scope(Arc::clone(s), None);
                    tools.push(Arc::new(if doc_read_wired {
                        tool.with_doc_read()
                    } else {
                        tool
                    }));
                } else {
                    skipped.push(name.clone());
                }
            }
            Some(ToolName::DocRead) => {
                if let (Some(s), Some(st)) = (search, storage) {
                    tools.push(Arc::new(chat::doc_read_tool::DocReadTool::new(
                        Arc::clone(s),
                        Arc::clone(st),
                    )));
                } else {
                    skipped.push(name.clone());
                }
            }
            // その他の閉集合ツール（web/code_interpreter/fs/shell 等）はゲートウェイ実行
            // 文脈（ワークスペース/サンドボックス無し）では未配線＝提示しない。
            Some(_) | None => skipped.push(name.clone()),
        }
    }
    (tools, skipped)
}

/// ツールに要る能力スコープ（同じものを読む API と同じ・None はスコープ不要）。
fn required_scope(tool: ToolName) -> Option<CapabilityScope> {
    match tool {
        ToolName::DocSearch => Some(CapabilityScope::RagQuery),
        ToolName::DocRead => Some(CapabilityScope::StorageRead),
        _ => None,
    }
}

/// [`AgentEvent`] → SSE 中立形。
fn to_ai_event(ev: &AgentEvent) -> AiEvent {
    match ev {
        AgentEvent::Text(t) => AiEvent {
            event: "text".into(),
            data: json!({ "text": t }),
        },
        AgentEvent::Thinking(t) => AiEvent {
            event: "thinking".into(),
            data: json!({ "text": t }),
        },
        AgentEvent::ToolCall {
            id,
            name,
            input,
            step,
        } => AiEvent {
            event: "tool_call".into(),
            data: json!({ "id": id, "name": name, "input": input, "step": step }),
        },
        AgentEvent::ToolResult {
            tool_call_id,
            ok,
            content,
        } => AiEvent {
            event: "tool_result".into(),
            data: json!({ "tool_call_id": tool_call_id, "ok": ok, "content": content }),
        },
        AgentEvent::Citation(c) => AiEvent {
            event: "citation".into(),
            data: serde_json::to_value(c).unwrap_or_else(|_| json!({})),
        },
        AgentEvent::BudgetWarning { kind, used, limit } => AiEvent {
            event: "budget_warning".into(),
            data: json!({ "kind": format!("{kind:?}"), "used": used, "limit": limit }),
        },
        // その他（Artifact/Plan/承認系）はゲートウェイ agent 文脈では発生しない構成だが、
        // 将来増えても情報落ちしないよう Debug 表現で流す。
        other => AiEvent {
            event: "event".into(),
            data: json!({ "debug": format!("{other:?}") }),
        },
    }
}

/// AgentEvent を mpsc へ写す sink（受信側が消えたら協調キャンセル）。
struct ChannelSink {
    tx: mpsc::Sender<AiEvent>,
}

#[async_trait::async_trait]
impl EventSink for ChannelSink {
    async fn emit(&mut self, event: AgentEvent) -> Result<(), agent_core::AgentError> {
        // コンテキストの内訳はチャット画面の表示用。ミニアプリの契約には載せない。
        if matches!(event, AgentEvent::ContextUsage(_)) {
            return Ok(());
        }
        // 受信側 drop（クライアント切断）は is_cancelled が拾う。ここでは無視してよい。
        let _ = self.tx.send(to_ai_event(&event)).await;
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.tx.is_closed()
    }
}

#[async_trait::async_trait]
impl AgentPort for GatewayAgentPort {
    async fn invoke(
        &self,
        ctx: &AuthContext,
        spec: AgentInvokeSpec,
    ) -> Result<AiEventStream, GatewayError> {
        let (tools, skipped) = build_tools(
            &spec.declared_tools,
            &spec.scopes,
            self.search.as_ref(),
            self.storage.as_ref(),
        );

        let max_steps = spec
            .max_steps
            .unwrap_or(DEFAULT_MAX_STEPS)
            .clamp(1, MAX_STEPS_CEILING);
        let mut opts = AgentOptions::chat(max_steps);
        // モデルは省略も明示も組織の設定で決める（agent-core の既定＝デプロイ既定に頼らない・明示モデルは置き換えず、
        // 使えなければ断る・BUG-12・D-117）。ゲートウェイの経路が先に同じ規則で確かめ、分かる誤りで断っている。
        let choice =
            app_gateway::resolve_app_model(&self.llm, ctx, spec.model.as_deref(), None).await?;
        opts.model = Some(choice.model);
        opts.effort = choice.effort;
        if let Some(cap) = spec.max_tokens.and_then(|t| u32::try_from(t).ok()) {
            opts.max_tokens = Some(opts.max_tokens.map_or(cap, |d| d.min(cap)));
        }
        // 日次残額をループ予算にも渡す（実行中の超過もステップ境界で停止）。
        opts.budget.max_cost_usd_micros = Some(spec.max_cost_usd_micros);

        let (tx, rx) = mpsc::channel::<AiEvent>(64);
        if !skipped.is_empty() {
            // 宣言されたが提示できないツールを冒頭で通知する（黙って弱くしない）。
            let _ = tx
                .send(AiEvent {
                    event: "tools_unavailable".into(),
                    data: json!({ "tools": skipped }),
                })
                .await;
        }

        let llm = Arc::clone(&self.llm);
        let ctx_owned = ctx.clone();
        let prompt = spec.prompt.clone();
        let app_id = spec.app_id;
        let trace_id = spec.trace_id.clone();
        tokio::spawn(async move {
            let run_ctx = RunContext {
                ctx: &ctx_owned,
                // ゲートウェイ agent.invoke の 1 回が trace の起点。
                owns_trace: true,
                idempotency_prefix: format!("gw-agent:{app_id}:{}", uuid::Uuid::new_v4()),
                trace_id,
                input_preview: prompt.chars().take(200).collect(),
                app_id: Some(app_id),
            };
            let history = vec![LlmMessage::text(LlmRole::User, prompt)];
            let mut sink = ChannelSink { tx: tx.clone() };
            match run_agent(
                &llm, &tools, history, &run_ctx, &opts, None, None, &mut sink,
            )
            .await
            {
                Ok(outcome) => {
                    let _ = tx
                        .send(AiEvent {
                            event: "done".into(),
                            data: json!({ "stop": format!("{:?}", outcome.stop) }),
                        })
                        .await;
                }
                Err(e) => {
                    let _ = tx
                        .send(AiEvent {
                            event: "error".into(),
                            data: json!({ "error": e.to_string() }),
                        })
                        .await;
                }
            }
        });

        use futures::StreamExt;
        Ok(ReceiverStream::new(rx).boxed())
    }
}

/// tokio mpsc → Stream の薄いアダプタ。
struct ReceiverStream<T>(mpsc::Receiver<T>);

impl<T> ReceiverStream<T> {
    fn new(rx: mpsc::Receiver<T>) -> Self {
        ReceiverStream(rx)
    }
}

impl<T> futures::Stream for ReceiverStream<T> {
    type Item = T;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<T>> {
        self.0.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_declared_intersect_closed_set_and_wiring() {
        // search 未配線: doc_search も skipped。
        let all = [CapabilityScope::RagQuery, CapabilityScope::StorageRead];
        let (tools, skipped) = build_tools(
            &[
                "doc_search".into(),
                "doc_read".into(),
                "doc_read".into(),
                "shell".into(),
                "bogus".into(),
            ],
            &all,
            None,
            None,
        );
        assert!(tools.is_empty());
        // 重複宣言は 1 つにまとまる。
        assert_eq!(skipped, vec!["doc_search", "doc_read", "shell", "bogus"]);
        // 宣言なし → 提示なし。
        let (tools, skipped) = build_tools(&[], &all, None, None);
        assert!(tools.is_empty() && skipped.is_empty());
    }

    /// モデルを省略したミニアプリの agent.invoke は、agent-core の既定（デプロイ既定）ではなく組織の既定で
    /// 動き、会計にも同じ名前が残る（BUG-12）。`STORAGE_TEST_DATABASE_URL` が無ければスキップ。
    #[tokio::test]
    #[allow(clippy::print_stderr)]
    async fn omitted_agent_model_follows_the_organization_default() {
        use futures::StreamExt;
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
        let entry = |id: &str| llm_gateway::ModelEntry {
            id: id.into(),
            ..Default::default()
        };
        let llm = LlmGateway::build(
            pool.clone(),
            reqwest::Client::new(),
            llm_gateway::GatewayConfig {
                catalog: llm_gateway::ModelCatalog {
                    default_model: "stub-m".into(),
                    models: vec![entry("stub-m"), entry("stub-n")],
                },
                ..Default::default()
            },
        )
        .unwrap();
        let tenant = format!("t-{}", uuid::Uuid::new_v4().simple());
        let ctx = AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(tenant.clone()),
            },
            "acme".into(),
            authz::TenantId::parse(tenant.clone()).unwrap(),
        );
        let patch = llm_gateway::SettingsPatch {
            default_model: Some("stub-n".into()),
            ..Default::default()
        };
        llm.update_tenant_settings(&ctx, &patch)
            .await
            .unwrap()
            .unwrap();
        let port = GatewayAgentPort {
            llm: Arc::new(llm),
            search: None,
            storage: None,
        };
        let app_id = uuid::Uuid::new_v4();
        let events = port
            .invoke(
                &ctx,
                AgentInvokeSpec {
                    app_id,
                    prompt: "こんにちは".into(),
                    model: None,
                    declared_tools: vec![],
                    scopes: vec![],
                    max_tokens: None,
                    max_steps: Some(1),
                    max_cost_usd_micros: 1_000_000,
                    trace_id: None,
                },
            )
            .await
            .unwrap();
        let events: Vec<AiEvent> = events.collect().await;
        assert!(events.iter().any(|e| e.event == "done"), "{events:?}");
        let models: Vec<String> =
            sqlx::query_scalar("SELECT model FROM llm_usage WHERE tenant_id = $1 AND app_id = $2")
                .bind(&tenant)
                .bind(app_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(models, ["stub-n"]);

        // 組織がモデルの選択を禁じると、既定と違うモデルを明示した呼び出しは port でも断る（ゲートウェイの経路が先に
        // 確かめた後に設定が変わったときの守り・D-117）。既定と同じモデルの明示は通る。
        let lock = llm_gateway::SettingsPatch {
            allow_user_choice: Some(false),
            ..Default::default()
        };
        port.llm
            .update_tenant_settings(&ctx, &lock)
            .await
            .unwrap()
            .unwrap();
        let spec = |model: &str| AgentInvokeSpec {
            app_id,
            prompt: "こんにちは".into(),
            model: Some(model.into()),
            declared_tools: vec![],
            scopes: vec![],
            max_tokens: None,
            max_steps: Some(1),
            max_cost_usd_micros: 1_000_000,
            trace_id: None,
        };
        let refused = port.invoke(&ctx, spec("stub-m")).await.err();
        assert!(
            matches!(&refused, Some(GatewayError::Invalid(m)) if m.contains("「stub-m」は使えません")),
            "選択を禁じた組織で既定と違うモデルが通った: {refused:?}"
        );
        let events: Vec<AiEvent> = port
            .invoke(&ctx, spec("stub-n"))
            .await
            .unwrap()
            .collect()
            .await;
        assert!(events.iter().any(|e| e.event == "done"), "{events:?}");

        // 作った行を片付ける（会計は run_agent が終わる前に書く）。
        for table in ["llm_usage", "tenant_llm_model", "tenant_llm_settings"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
                .bind(&tenant)
                .execute(&pool)
                .await
                .unwrap();
        }
    }

    /// プロバイダ（OpenAI 互換）を `base` に向けて agent.invoke を 1 回動かし、error イベントの中身を返す（作った行は
    /// 消す）。`STORAGE_TEST_DATABASE_URL` が無ければ `None`（スキップ）。
    #[allow(clippy::print_stderr)]
    async fn agent_error_at(base: String) -> Option<String> {
        use futures::StreamExt;
        let Ok(url) = std::env::var("STORAGE_TEST_DATABASE_URL") else {
            eprintln!("STORAGE_TEST_DATABASE_URL 未設定のためスキップ");
            return None;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&pool).await.unwrap();
        let llm = LlmGateway::build(
            pool.clone(),
            reqwest::Client::new(),
            llm_gateway::GatewayConfig {
                provider: llm_gateway::ProviderConfig {
                    kind: llm_gateway::ProviderKind::Openai,
                    base_url: Some(base),
                    api_key: Some("k".into()),
                    ..Default::default()
                },
                catalog: llm_gateway::ModelCatalog {
                    default_model: "m1".into(),
                    models: vec![llm_gateway::ModelEntry {
                        id: "m1".into(),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )
        .unwrap();
        let tenant = format!("t-{}", uuid::Uuid::new_v4().simple());
        let ctx = AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "alice".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some(tenant.clone()),
            },
            "acme".into(),
            authz::TenantId::parse(tenant.clone()).unwrap(),
        );
        let port = GatewayAgentPort {
            llm: Arc::new(llm),
            search: None,
            storage: None,
        };
        let events: Vec<AiEvent> = port
            .invoke(
                &ctx,
                AgentInvokeSpec {
                    app_id: uuid::Uuid::new_v4(),
                    prompt: "こんにちは".into(),
                    model: None,
                    declared_tools: vec![],
                    scopes: vec![],
                    max_tokens: None,
                    max_steps: Some(1),
                    max_cost_usd_micros: 1_000_000,
                    trace_id: None,
                },
            )
            .await
            .unwrap()
            .collect()
            .await;
        for table in ["llm_usage", "tenant_llm_model", "tenant_llm_settings"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id = $1"))
                .bind(&tenant)
                .execute(&pool)
                .await
                .unwrap();
        }
        let error = events.iter().find(|e| e.event == "error");
        assert!(error.is_some(), "error イベントが出る: {events:?}");
        Some(error.unwrap().data.to_string())
    }

    /// プロバイダの 5xx（529 を含む）の本文（要求の ID・組織の ID）は、ミニアプリの agent.invoke の error イベントにも
    /// 載せない（誤りの種類の語だけ・BUG-210）。`STORAGE_TEST_DATABASE_URL` が無ければスキップ。
    #[tokio::test]
    async fn agent_errors_do_not_carry_the_provider_body() {
        use axum::response::IntoResponse;
        let app = axum::Router::new().fallback(|| async {
            (
                axum::http::StatusCode::from_u16(529).unwrap(),
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded for org-secret"},"request_id":"req_secret_8"}"#,
            )
                .into_response()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let Some(shown) = agent_error_at(base).await else {
            return;
        };
        for leaked in ["org-secret", "req_secret_8", "Overloaded for"] {
            assert!(!shown.contains(leaked), "{leaked} が漏れた: {shown}");
        }
        assert!(shown.contains("overloaded_error"), "{shown}");
    }

    /// プロバイダに接続できないときの agent.invoke の error イベントに、送り先の URL（ホスト・ポート・パス・base_url の
    /// userinfo）を載せない（誤りの種類だけ・BUG-218）。`STORAGE_TEST_DATABASE_URL` が無ければスキップ。
    #[tokio::test]
    async fn agent_errors_do_not_name_an_unreachable_provider() {
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = closed.local_addr().unwrap();
        drop(closed);
        let Some(shown) = agent_error_at(format!("http://user:pw-secret@{addr}/llm-base")).await
        else {
            return;
        };
        let port = addr.port().to_string();
        for leaked in [
            "127.0.0.1",
            port.as_str(),
            "pw-secret",
            "/llm-base",
            "http://",
        ] {
            assert!(!shown.contains(leaked), "{leaked} が漏れた: {shown}");
        }
        assert!(
            shown.contains("プロバイダに接続できませんでした"),
            "{shown}"
        );
    }

    #[test]
    fn read_tools_need_the_same_scope_as_their_apis() {
        assert_eq!(
            required_scope(ToolName::DocSearch),
            Some(CapabilityScope::RagQuery)
        );
        assert_eq!(
            required_scope(ToolName::DocRead),
            Some(CapabilityScope::StorageRead)
        );
        // スコープが無ければ配線の有無に関わらず skipped（配線前に弾く）。
        let (tools, skipped) = build_tools(
            &["doc_search".into(), "doc_read".into()],
            &[CapabilityScope::AgentInvoke],
            None,
            None,
        );
        assert!(tools.is_empty());
        assert_eq!(skipped, vec!["doc_search", "doc_read"]);
    }
}
