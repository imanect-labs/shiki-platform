//! egress スタックの gated 結合テスト（`SANDBOX_NETNS_IT=1`＋`NETNS_HOLDER_BIN`）。
//!
//! 実 namespace ＋実プロキシで、allowlist に載るホストへの接続だけが上流へ届くことを確認する。
//! CI（unshare/nsenter 不可）では skip。holder バイナリのパスは `NETNS_HOLDER_BIN` で渡す。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::io::{Read, Write};
use std::net::TcpListener as StdTcpListener;
use std::path::PathBuf;

use sandbox_client::{Egress, EgressRule};
use sandbox_orchestrator::backend::egress::{EgressAudit, EgressStack};

fn gated() -> Option<PathBuf> {
    if std::env::var("SANDBOX_NETNS_IT").as_deref() != Ok("1") {
        eprintln!("skip: set SANDBOX_NETNS_IT=1");
        return None;
    }
    if let Ok(p) = std::env::var("NETNS_HOLDER_BIN") {
        Some(PathBuf::from(p))
    } else {
        eprintln!("skip: set NETNS_HOLDER_BIN to the shiki-netns-holder path");
        None
    }
}

/// netns 内で gateway:port へ TLS ClientHello(SNI=`sni`) を送り、受信文字列を返す。
/// 拒否されると即切断されるため空文字列になる。
fn probe(pid: u32, port: u16, sni: &str) -> String {
    // python 側で最小 ClientHello を組む（SNI を含む）。上流は接続直後に "UPSTREAM_OK" を送る。
    let script = format!(
        r"
import socket,struct
def ch(host):
    host=host.encode()
    sn=b'\x00'+struct.pack('>H',len(host))+host
    snl=struct.pack('>H',len(sn))+sn
    ext=b'\x00\x00'+struct.pack('>H',len(snl))+snl
    exts=struct.pack('>H',len(ext))+ext
    body=b'\x03\x03'+b'\x00'*32+b'\x00'+struct.pack('>H',2)+b'\x13\x01'+b'\x01'+b'\x00'+exts
    hs=b'\x01'+struct.pack('>I',len(body))[1:]+body
    return b'\x16\x03\x01'+struct.pack('>H',len(hs))+hs
s=socket.create_connection(('169.254.0.1',{port}),3)
s.sendall(ch('{sni}'))
s.settimeout(3)
try:
    print(s.recv(64).decode(errors='replace'))
except Exception:
    print('')
"
    );
    let out = std::process::Command::new("nsenter")
        .args([
            "-t",
            &pid.to_string(),
            "-U",
            "-n",
            "--preserve-credentials",
            "--",
            "python3",
            "-c",
            &script,
        ])
        .output()
        .expect("nsenter client");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn egress_allows_listed_host_blocks_others() {
    let Some(holder) = gated() else { return };

    // 上流サーバ: 接続直後に固定バナーを送る（ホスト netns・ループバック）。
    let up = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let up_port = up.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in up.incoming() {
            let Ok(mut s) = stream else { break };
            std::thread::spawn(move || {
                let _ = s.write_all(b"UPSTREAM_OK");
                let mut b = [0u8; 64];
                let _ = s.read(&mut b);
            });
        }
    });

    // allowlist: SNI=127.0.0.1 の up_port のみ許可。
    let egress = Egress {
        static_allow: vec![EgressRule {
            host_pattern: "127.0.0.1".into(),
            port: up_port,
        }],
        ..Egress::blocked()
    };
    let state = std::env::temp_dir().join(format!("egress-it-{}", std::process::id()));
    let audit = EgressAudit {
        tenant_id: "t".into(),
        sandbox_id: "s".into(),
    };
    let stack = EgressStack::start(&egress, audit, &holder, &state)
        .await
        .expect("start egress stack");
    let pid = stack.netns_pid();

    // 許可: SNI=127.0.0.1 → 上流バナーが返る。
    let allowed = probe(pid, up_port, "127.0.0.1");
    assert!(
        allowed.contains("UPSTREAM_OK"),
        "allowed connection should reach upstream, got {allowed:?}"
    );

    // 拒否: 443 に SNI=evil.test → allowlist 不一致で即切断（空）。
    let denied = probe(pid, 443, "evil.test");
    assert!(
        !denied.contains("UPSTREAM_OK"),
        "denied connection must not reach upstream, got {denied:?}"
    );

    stack.shutdown();
}
