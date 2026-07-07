//! egress スタック: 非特権 user+net namespace（[`shiki_sandbox_netns`]）＋ホスト名フィルタ TCP プロキシ
//! ＋偽 DNS を束ね、gVisor/FC ゲストの外向き通信を allowlist に閉じ込める（PIT-25）。
//!
//! allowlist が空なら **そもそもスタックを作らない**（default-deny は構造的）。バックエンドは
//! `netns_path()` を `nsenter -U -n` に渡してゲストランタイムを同じ netns へ入れる。

mod dns;
mod proxy;
pub mod rules;
mod sni;

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sandbox_client::{Egress, SandboxError};
use shiki_sandbox_netns::{Netns, NetnsSpec};
use tokio::task::JoinHandle;

pub use proxy::{classify, EgressAudit};
pub use rules::{evaluate, Decision};

/// netns 内ゲートウェイ IP（プロキシ/DNS の待受先・リンクローカル）。
const GATEWAY: Ipv4Addr = Ipv4Addr::new(169, 254, 0, 1);
const PREFIX: u8 = 30;
const DNS_PORT: u16 = 53;

/// allowlist が非空か（空＝スタック不要・default-deny）。
#[must_use]
pub fn has_egress(egress: &Egress) -> bool {
    !egress.static_allow.is_empty() || !egress.dynamic_allow.is_empty()
}

/// 起動済み egress スタック。`shutdown()`（または drop）で全タスク停止＋netns 破棄。
pub struct EgressStack {
    ns: Netns,
    tasks: Vec<JoinHandle<()>>,
    _sock_dir: PathBuf,
}

impl EgressStack {
    /// egress ポリシからスタックを起動する。プロキシは 80/443＋allowlist の各ポートを覆う。
    pub async fn start(
        egress: &Egress,
        audit: EgressAudit,
        holder_bin: &Path,
        state_dir: &Path,
    ) -> Result<EgressStack, SandboxError> {
        let ports = proxy_ports(egress);
        let spec = NetnsSpec {
            gateway: GATEWAY,
            prefix: PREFIX,
            tcp_ports: ports,
            dns_port: DNS_PORT,
        };
        let sock_dir = state_dir.to_path_buf();
        std::fs::create_dir_all(&sock_dir)
            .map_err(|e| SandboxError::Internal(format!("egress state dir: {e}")))?;

        // Netns::spawn は同期（unshare 子の起動待ち）。ランタイムを塞がぬよう blocking へ。
        let holder_bin = holder_bin.to_path_buf();
        let sock_dir_c = sock_dir.clone();
        let mut ns =
            tokio::task::spawn_blocking(move || Netns::spawn(&holder_bin, &sock_dir_c, &spec))
                .await
                .map_err(|e| SandboxError::Internal(format!("netns spawn join: {e}")))?
                .map_err(|e| SandboxError::Unavailable(format!("netns spawn: {e}")))?;

        let egress = Arc::new(egress.clone());
        // SSRF 緩和フラグは起動時に env から一度だけ読む（本番は未設定＝内部 IP を必ず遮断）。
        let allow_private = proxy::allow_private_from_env();
        let mut tasks = Vec::new();
        for (port, std_listener) in ns.take_tcp() {
            let listener = tokio::net::TcpListener::from_std(std_listener)
                .map_err(|e| SandboxError::Internal(format!("adopt tcp listener: {e}")))?;
            tasks.push(tokio::spawn(proxy::serve_port(
                listener,
                port,
                Arc::clone(&egress),
                audit.clone(),
                allow_private,
            )));
        }
        let dns_std = ns
            .dns_socket()
            .map_err(|e| SandboxError::Internal(format!("dns socket: {e}")))?;
        let dns_sock = tokio::net::UdpSocket::from_std(dns_std)
            .map_err(|e| SandboxError::Internal(format!("adopt dns socket: {e}")))?;
        tasks.push(tokio::spawn(serve_dns(dns_sock, GATEWAY)));

        Ok(EgressStack {
            ns,
            tasks,
            _sock_dir: sock_dir,
        })
    }

    /// holder の PID（`nsenter -t <pid> -U -n`）。
    #[must_use]
    pub fn netns_pid(&self) -> u32 {
        self.ns.pid()
    }

    /// netns の proc パス。
    #[must_use]
    pub fn netns_path(&self) -> PathBuf {
        self.ns.netns_path()
    }

    /// userns の proc パス。
    #[must_use]
    pub fn userns_path(&self) -> PathBuf {
        self.ns.userns_path()
    }

    /// netns 内ゲートウェイ IP（ゲストのデフォルトルート/リゾルバ）。
    #[must_use]
    pub fn gateway(&self) -> Ipv4Addr {
        GATEWAY
    }

    /// 全プロキシ/DNS タスクを停止し netns を破棄する（明示破棄。Drop でも同等に畳まれる）。
    pub fn shutdown(self) {
        drop(self);
    }
}

impl Drop for EgressStack {
    fn drop(&mut self) {
        // プロキシ/DNS タスクを停止（JoinHandle の drop は detach なので明示 abort）。
        for t in &self.tasks {
            t.abort();
        }
        // netns holder は `Netns` の Drop が kill する（→ netns 破棄）。
    }
}

/// プロキシが覆うポート集合（80/443＋allowlist の非 0 ポート・重複排除）。
fn proxy_ports(egress: &Egress) -> Vec<u16> {
    let mut ports = vec![80u16, 443];
    for r in egress
        .static_allow
        .iter()
        .chain(egress.dynamic_allow.iter())
    {
        if r.port != 0 && !ports.contains(&r.port) {
            ports.push(r.port);
        }
    }
    ports
}

/// 偽 DNS 応答ループ（全 A クエリをゲートウェイ IP へ・AAAA は NODATA）。
async fn serve_dns(sock: tokio::net::UdpSocket, gateway: Ipv4Addr) {
    let mut buf = vec![0u8; 1500];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else {
            return;
        };
        if let Some(resp) = dns::build_response(&buf[..n], gateway) {
            let _ = sock.send_to(&resp, from).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox_client::EgressRule;

    #[test]
    fn proxy_ports_include_base_and_custom() {
        let e = Egress {
            static_allow: vec![EgressRule {
                host_pattern: "h".into(),
                port: 8443,
            }],
            dynamic_allow: vec![EgressRule {
                host_pattern: "h2".into(),
                port: 443,
            }],
            ..Egress::blocked()
        };
        let ports = proxy_ports(&e);
        assert!(ports.contains(&80) && ports.contains(&443) && ports.contains(&8443));
        // 443 は重複しない。
        assert_eq!(ports.iter().filter(|&&p| p == 443).count(), 1);
    }

    #[test]
    fn has_egress_reflects_allowlist() {
        assert!(!has_egress(&Egress::blocked()));
        let e = Egress {
            dynamic_allow: vec![EgressRule {
                host_pattern: "h".into(),
                port: 443,
            }],
            ..Egress::blocked()
        };
        assert!(has_egress(&e));
    }
}
