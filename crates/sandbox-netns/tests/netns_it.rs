//! netns 機構の gated 結合テスト（unshare/nsenter が使える環境でのみ・`SANDBOX_NETNS_IT=1`）。
//!
//! 「netns 内で bind したリスナを、ホストプロセスが service する」トリックを end-to-end で実証する:
//! holder を spawn → FD 受領 → ホスト側スレッドが accept → netns 内クライアント（nsenter 経由の
//! python3）から gateway:port へ接続 → ホスト側でエコー往復。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

use shiki_sandbox_netns::{Netns, NetnsSpec};

fn gated() -> bool {
    std::env::var("SANDBOX_NETNS_IT").as_deref() == Ok("1")
}

fn holder_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_shiki-netns-holder"))
}

#[test]
fn in_netns_listener_serviced_by_host() {
    if !gated() {
        eprintln!("skip: set SANDBOX_NETNS_IT=1 (needs unshare/nsenter)");
        return;
    }
    let tmp = tempdir();
    let spec = NetnsSpec {
        gateway: "169.254.0.1".parse().unwrap(),
        prefix: 30,
        tcp_ports: vec![7443],
        dns_port: 53,
    };
    let mut ns = Netns::spawn(&holder_bin(), &tmp, &spec).expect("spawn netns");

    // 受領リスナでエコーサーバをホストランタイムで走らせる。
    let tcp = ns.take_tcp();
    assert_eq!(tcp.len(), 1);
    let (_port, listener): (u16, TcpListener) = tcp.into_iter().next().unwrap();
    listener.set_nonblocking(false).unwrap();
    let handle = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept in host");
        let mut buf = [0u8; 64];
        let n = sock.read(&mut buf).expect("read");
        let upper = String::from_utf8_lossy(&buf[..n]).to_uppercase();
        sock.write_all(upper.as_bytes()).expect("write");
    });

    // netns 内クライアント（nsenter -U -n）。0-cap でも userns 経由で netns に入れる。
    let out = std::process::Command::new("nsenter")
        .args([
            "-t",
            &ns.pid().to_string(),
            "-U",
            "-n",
            "--preserve-credentials",
            "--",
            "python3",
            "-c",
            "import socket;s=socket.create_connection(('169.254.0.1',7443),3);s.sendall(b'hello');print(s.recv(16).decode())",
        ])
        .output()
        .expect("run nsenter client");
    handle.join().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("HELLO"),
        "expected echo HELLO, got stdout={stdout:?} stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    ns.shutdown();
}

/// 短命テスト用ディレクトリ（tempfile を足さないための最小実装）。
fn tempdir() -> PathBuf {
    let base = std::env::temp_dir().join(format!("netns-it-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    base
}
