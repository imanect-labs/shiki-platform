//! テキストエンジンの結合テスト用の HTTP スタブ（Host とパスで応答を選び、受けた要求を記録する）。

#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::unnecessary_wraps,
    clippy::format_push_string
)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use authz::{AuthContext, Principal, TenantId};
use sandbox_client::net_guard::HostResolver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 受けた要求。
#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub host: String,
    /// Host ヘッダのポート（スタブのポート）。
    pub port: u16,
    pub target: String,
    pub cookie: Option<String>,
    pub origin: Option<String>,
    pub body: String,
}

pub type Route = Arc<dyn Fn(&Req) -> Vec<u8> + Send + Sync>;

/// 応答を組み立てる。
pub fn http(status: &str, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut s = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        s.push_str("Content-Type: text/html; charset=utf-8\r\n");
    }
    for (k, v) in headers {
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    s.push_str("\r\n");
    let mut v = s.into_bytes();
    v.extend_from_slice(body.as_bytes());
    v
}

/// スタブを起動する。全ホスト名がこのアドレスに解決される前提（[`ToStub`]）。
pub async fn serve(route: Route) -> (SocketAddr, Arc<Mutex<Vec<Req>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let route = route.clone();
            let log = l.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // ヘッダの終わりまで読み、Content-Length 分の本文を読む。
                let (head_end, content_length) = loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_string();
                        let cl = head
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse().ok())?
                            })
                            .unwrap_or(0usize);
                        break (i + 4, cl);
                    }
                };
                while buf.len() < head_end + content_length {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let mut lines = head.lines();
                let mut first = lines.next().unwrap_or("").split(' ');
                let method = first.next().unwrap_or("").to_string();
                let target = first.next().unwrap_or("").to_string();
                let header = |name: &str| {
                    head.lines().skip(1).find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                    })
                };
                let host_header = header("host").unwrap_or_default();
                let (host, port) = host_header
                    .split_once(':')
                    .unwrap_or((host_header.as_str(), "80"));
                let req = Req {
                    method,
                    host: host.to_string(),
                    port: port.parse().unwrap_or(80),
                    target,
                    cookie: header("cookie"),
                    origin: header("origin"),
                    body: String::from_utf8_lossy(&buf[head_end..]).to_string(),
                };
                log.lock().unwrap().push(req.clone());
                let resp = route(&req);
                let _ = sock.write_all(&resp).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (addr, log)
}

/// どのホスト名もスタブへ解決する。
pub struct ToStub(pub SocketAddr);

#[async_trait::async_trait]
impl HostResolver for ToStub {
    async fn lookup(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, &'static str> {
        Ok(vec![self.0])
    }
}

/// テストだけがループバックのスタブへ繋ぐための解決後 IP の検証。
pub fn allow_loopback(a: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str> {
    Ok(a)
}

pub fn ctx() -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "u1".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("t1".into()),
        },
        "org1".into(),
        TenantId::parse("t1").unwrap(),
    )
}
