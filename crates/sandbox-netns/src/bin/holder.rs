//! netns holder: `unshare --user --net --map-root-user` 経由で新しい user+net namespace 内に起動され、
//! ゲートウェイ IP とリスナ群を bind し、その FD を制御ソケット経由で親（orchestrator）へ渡して park する。
//!
//! 親が制御ソケットを閉じると（EOF）終了し、netns もろとも消える。単独では意味を持たない補助バイナリ。

use std::io::{IoSlice, Read};
use std::net::{TcpListener, UdpSocket};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::{Command, ExitCode};

use nix::sys::socket::{sendmsg, ControlMessage, MsgFlags, UnixAddr};

struct Args {
    sock: String,
    gateway: String,
    prefix: String,
    ports: Vec<u16>,
    dns_port: u16,
}

fn parse_args() -> Result<Args, String> {
    let mut sock = None;
    let mut gateway = None;
    let mut prefix = None;
    let mut ports = Vec::new();
    let mut dns_port = 53u16;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let val = it
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--sock" => sock = Some(val),
            "--gateway" => gateway = Some(val),
            "--prefix" => prefix = Some(val),
            "--ports" => {
                ports = val
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.parse::<u16>().map_err(|e| format!("bad port {s}: {e}")))
                    .collect::<Result<_, _>>()?;
            }
            "--dns-port" => {
                dns_port = val.parse().map_err(|e| format!("bad dns-port: {e}"))?;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(Args {
        sock: sock.ok_or("missing --sock")?,
        gateway: gateway.ok_or("missing --gateway")?,
        prefix: prefix.ok_or("missing --prefix")?,
        ports,
        dns_port,
    })
}

/// netns 内のネットワークを構成する（lo を up し、ゲートウェイ IP を lo に付与）。
///
/// dummy カーネルモジュール依存を避けるため lo に付与する（bind は 0.0.0.0 なので到達可能）。
/// gVisor/FC 用の veth/tap は各バックエンドが nsenter 後に追加する。
fn setup_net(gateway: &str, prefix: &str) -> Result<(), String> {
    run_ip(&["link", "set", "lo", "up"])?;
    run_ip(&["addr", "add", &format!("{gateway}/{prefix}"), "dev", "lo"])?;
    Ok(())
}

fn run_ip(args: &[&str]) -> Result<(), String> {
    let status = Command::new("ip")
        .args(args)
        .status()
        .map_err(|e| format!("spawn ip: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ip {args:?} failed: {status}"))
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    setup_net(&args.gateway, &args.prefix)?;

    // リスナを bind（0.0.0.0 で netns 内全 IF から到達可能）。順序: tcp..., udp。
    let mut tcp: Vec<TcpListener> = Vec::with_capacity(args.ports.len());
    for &p in &args.ports {
        let l = TcpListener::bind(("0.0.0.0", p)).map_err(|e| format!("bind tcp :{p}: {e}"))?;
        tcp.push(l);
    }
    let udp = UdpSocket::bind(("0.0.0.0", args.dns_port))
        .map_err(|e| format!("bind udp :{}: {e}", args.dns_port))?;

    // FD 群と本文（ポート CSV）を SCM_RIGHTS で親へ送る。
    let mut ctrl = UnixStream::connect(&args.sock).map_err(|e| format!("connect ctrl: {e}"))?;
    let mut raw_fds: Vec<RawFd> = tcp.iter().map(AsRawFd::as_raw_fd).collect();
    raw_fds.push(udp.as_raw_fd());
    let ports_csv = args
        .ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let iov = [IoSlice::new(ports_csv.as_bytes())];
    let cmsg = [ControlMessage::ScmRights(&raw_fds)];
    sendmsg::<UnixAddr>(ctrl.as_raw_fd(), &iov, &cmsg, MsgFlags::empty(), None)
        .map_err(|e| format!("sendmsg fds: {e}"))?;

    // 親が閉じるまで park（FD は親が複製済みなのでこちら側は保持し続ける）。
    let mut sink = [0u8; 1];
    loop {
        match ctrl.read(&mut sink) {
            Ok(0) => break, // 親が制御ソケットを閉じた→終了。
            Ok(_) => {}     // 予備の合図（今は無視）。
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    // tcp/udp を drop させないまま終了（プロセス終了で FD は解放される）。
    drop(tcp);
    drop(udp);
    Ok(())
}

fn main() -> ExitCode {
    if let Err(e) = run() {
        eprintln!("netns-holder: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
