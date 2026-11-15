//! 職員間メッセージ（Phase 14）の配線（`wiring.rs` から分割・行数規約）。

use std::sync::Arc;

use api::config::{AppConfig, WebSearchBackend};

/// メッセージのサービスとパーソナルエージェントの実行部を組む。
///
/// - 配信の起床は Redis（チャットと同じ接続先）。つながらなくてもプロセス内＋DB ポーリングで動く。
/// - リンクのプレビューは **外部接続のある構成だけ**（web 検索が実プロバイダで構成されている）。
///   未設定・Stub（テスト・エアギャップ）では取得自体を組まない（NFR-2 の外部接続必須機能）。
/// - 保存期間の削除は毎時回す（既定は無期限なので、期限を設定した org だけが対象）。
pub(crate) async fn wire_messaging(
    config: &AppConfig,
    db: &sqlx::PgPool,
    authz: &Arc<dyn authz::AuthzClient>,
    storage: &Arc<storage::StorageService>,
    chat: Option<&Arc<chat::ChatStore>>,
    search: Option<&Arc<rag::SearchService>>,
    llm: Option<&Arc<llm_gateway::LlmGateway>>,
) -> (
    Arc<messaging::MessagingService>,
    Option<Arc<dyn messaging::AgentRuntime>>,
) {
    let redis_url = config
        .chat
        .redis_url
        .clone()
        .unwrap_or_else(|| config.session.redis_url.clone());
    let hub = messaging::pubsub::Hub::connect(&redis_url).await;
    let mut service = messaging::MessagingService::new(
        db.clone(),
        Arc::clone(authz),
        hub,
        Arc::new(messaging::files::StorageFiles(Arc::clone(storage))),
    );
    let external = matches!(
        config.websearch.backend,
        Some(b) if b != WebSearchBackend::Stub
    );
    if external {
        service = service.with_link_previews(messaging::preview::LinkPreviewer::new(Arc::new(
            sandbox_client::net_guard::SystemResolver,
        )));
    }
    messaging::retention::spawn_retention_timer(db.clone());
    let runtime = llm.map(|llm| {
        Arc::new(api::messaging_runtime::GatewayAgentRuntime {
            llm: Arc::clone(llm),
            search: search.cloned(),
            chat: chat.cloned(),
        }) as Arc<dyn messaging::AgentRuntime>
    });
    tracing::info!(
        link_previews = external,
        agent = runtime.is_some(),
        "職員間メッセージを有効化しました"
    );
    (Arc::new(service), runtime)
}
