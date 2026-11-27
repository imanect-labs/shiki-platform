//! 道具の組み立ての単体テスト（web_fetch と web_translate の取得が同じ中継を通る・BUG-274）。

use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_core::{EgressProxy, Tool};
use authz::{AuthContext, Principal};
use llm_gateway::{
    GatewayConfig, LlmGateway, ModelCatalog, ModelEntry, ProviderConfig, ProviderKind,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::configure_fetch;
use crate::worker::WorkerConfig;

/// 外の世界の代わり（受けたら決まった HTML を返す）。
async fn origin() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let body = "<html><body><p>Hello from the other side of the proxy.</p></body></html>";
            let res = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(res.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    addr
}

/// 偽の中継（受けた CONNECT の 1 行目を記録し、`origin` へつなぐ・agent-core の tests_proxy と同じ形）。
async fn fake_proxy(origin: SocketAddr) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && sock.read(&mut byte).await.unwrap_or(0) == 1 {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            log.lock()
                .unwrap()
                .push(head.lines().next().unwrap_or_default().to_string());
            let mut upstream = tokio::net::TcpStream::connect(origin).await.unwrap();
            let _ = sock
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await;
            let _ = tokio::io::copy_bidirectional(&mut sock, &mut upstream).await;
        }
    });
    (addr, seen)
}

fn ctx() -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("t".into()),
        },
        "acme".into(),
        authz::TenantId::parse("t").unwrap(),
    )
}

/// どの名前も公開の IP に解決する（外の DNS を引かない・中継へ頼む CONNECT の宛先になる）。
struct PublicResolver;

#[async_trait::async_trait]
impl sandbox_client::net_guard::HostResolver for PublicResolver {
    async fn lookup(&self, _host: &str, port: u16) -> Result<Vec<SocketAddr>, &'static str> {
        Ok(vec![SocketAddr::from(([93, 184, 216, 34], port))])
    }
}

/// 会話に提示する取得の道具と同じ組み立て（名前の引き方だけ差し替える）。
fn fetch_tool(config: &WorkerConfig) -> agent_core::WebFetchTool {
    configure_fetch(
        agent_core::WebFetchTool::with_resolver(Arc::new(PublicResolver)),
        config,
        None,
    )
}

/// DB に触れない stub のゲートウェイ（訳す段は失敗してよい・取得が中継を通ったかだけを見る）。
fn stub_gateway() -> LlmGateway {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://shiki@127.0.0.1:1/none")
        .unwrap();
    let config = GatewayConfig {
        provider: ProviderConfig {
            kind: ProviderKind::Stub,
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
    LlmGateway::build(pool, reqwest::Client::new(), config).unwrap()
}

/// web_translate の取得も、web_fetch と同じく運用者が設定した中継を通る（BUG-274: 前は中継を付けていなかった）。
#[tokio::test]
async fn web_translate_fetches_through_the_configured_proxy() {
    let (proxy, connects) = fake_proxy(origin().await).await;
    let config = WorkerConfig {
        egress_proxy: Some(EgressProxy::new(&format!("http://{proxy}"), None).unwrap()),
        ..WorkerConfig::default()
    };
    // web_fetch（同じ組み立て）。
    let fetch = fetch_tool(&config);
    let out = fetch
        .call(
            &ctx(),
            serde_json::json!({ "url": "http://news.example.invalid/a" }),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(
        *connects.lock().unwrap(),
        ["CONNECT 93.184.216.34:80 HTTP/1.1"]
    );
    // web_translate（push_translate_tool と同じ組み立て）。
    let translate = agent_core::WebTranslateTool::new(
        fetch_tool(&config),
        stub_gateway(),
        "run:1".into(),
        Arc::new(AtomicBool::new(false)),
    );
    let _ = tokio::time::timeout(
        Duration::from_secs(20),
        translate.call(
            &ctx(),
            serde_json::json!({ "url": "http://news.example.invalid/b", "target_lang": "ja" }),
            None,
        ),
    )
    .await;
    assert_eq!(
        connects.lock().unwrap().len(),
        2,
        "web_translate の取得も中継に届く: {:?}",
        connects.lock().unwrap()
    );
}
