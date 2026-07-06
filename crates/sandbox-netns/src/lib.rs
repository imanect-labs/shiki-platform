//! 非特権 user+net namespace の生成と、その中で bind したリスナ FD の受け渡し。
//!
//! # なぜこの形か
//! egress プロキシは「サンドボックス netns 内で bind したソケットを、ホスト netns の非同期ランタイムが
//! service する」ことで、ホスト netns 側の `CAP_NET_ADMIN` 無しにホスト名 allowlist を強制する
//! （PIT-25）。netns 内 bind と、上流への dial（ホスト netns）を 1 プロセスで跨ぐには、netns 内で
//! bind したリスナ FD をホストプロセスへ **SCM_RIGHTS** で渡す必要がある。
//!
//! namespace 生成は `unshare --user --net --map-root-user` の別プロセス（[`holder`] バイナリ）で行う
//! （userns 生成は単一スレッド必須で、tokio マルチスレッドの orchestrator 本体からは `unshare(2)`
//! できないため）。holder は netns 内でリスナを bind し、この制御ソケット経由で FD を送って park する。
//! 制御ソケットが閉じると holder は終了し netns は消える。
//!
//! # unsafe の封じ込め
//! `from_raw_fd`（受領 FD を std リスナへ）と recvmsg の cmsg 解釈のみ unsafe。呼び出し側には
//! `std::net::{TcpListener, UdpSocket}` の安全な型だけを返す。

use std::io::{self, IoSliceMut};
use std::net::{TcpListener, UdpSocket};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags, UnixAddr};

/// netns の構成パラメータ。
#[derive(Debug, Clone)]
pub struct NetnsSpec {
    /// netns 内ゲートウェイ IP（プロキシ/DNS が待ち受ける・例 169.254.0.1）。
    pub gateway: std::net::Ipv4Addr,
    /// ゲートウェイのプレフィクス長（/30 など）。
    pub prefix: u8,
    /// TCP プロキシを bind するポート群（80/443＋allowlist の各ポート）。
    pub tcp_ports: Vec<u16>,
    /// 偽 DNS を待ち受ける UDP ポート（通常 53）。
    pub dns_port: u16,
}

/// 生成済み netns とその中で bind されたリスナ群。`shutdown()` まで生存させる。
pub struct Netns {
    holder: Child,
    /// 制御ソケット。drop すると holder が EOF を見て終了→netns 破棄。
    _ctrl: UnixStream,
    pid: u32,
    tcp: Vec<(u16, TcpListener)>,
    dns: UdpSocket,
}

impl Netns {
    /// holder バイナリを `unshare` 経由で spawn し、netns 内リスナ FD を受け取る。
    ///
    /// `sock_dir` に制御用 unix ソケットを作る（呼び出し側が用意した短命ディレクトリ）。
    pub fn spawn(holder_bin: &Path, sock_dir: &Path, spec: &NetnsSpec) -> io::Result<Netns> {
        let sock_path = sock_dir.join("holder.sock");
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path)?;

        let ports_csv = spec
            .tcp_ports
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let holder = Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--net",
                "--",
                holder_bin.to_string_lossy().as_ref(),
                "--sock",
                sock_path.to_string_lossy().as_ref(),
                "--gateway",
                &spec.gateway.to_string(),
                "--prefix",
                &spec.prefix.to_string(),
                "--ports",
                &ports_csv,
                "--dns-port",
                &spec.dns_port.to_string(),
            ])
            .spawn()?;

        // holder からの接続を待つ（起動失敗時に無限待ちしないよう accept に緩いタイムアウトを敷く）。
        listener.set_nonblocking(false)?;
        let (stream, _addr) = accept_with_timeout(&listener, std::time::Duration::from_secs(10))?;

        let (ports, fds) = recv_fds(&stream, spec.tcp_ports.len() + 1)?;
        // 期待順: tcp_0..tcp_n, udp。ポート配列は holder が本文で送る（bind 実ポート＝要求ポート）。
        let mut owned: Vec<OwnedFd> = fds;
        let udp_fd = owned
            .pop()
            .ok_or_else(|| io::Error::other("holder sent no udp fd"))?;
        if owned.len() != spec.tcp_ports.len() || ports.len() != spec.tcp_ports.len() {
            return Err(io::Error::other("holder fd/port count mismatch"));
        }
        let tcp: Vec<(u16, TcpListener)> = ports
            .into_iter()
            .zip(owned)
            .map(|(p, fd)| (p, TcpListener::from(fd)))
            .collect();
        for (_, l) in &tcp {
            l.set_nonblocking(true)?;
        }
        let dns = UdpSocket::from(udp_fd);
        dns.set_nonblocking(true)?;

        let pid = holder.id();
        Ok(Netns {
            holder,
            _ctrl: stream,
            pid,
            tcp,
            dns,
        })
    }

    /// holder プロセスの PID（`nsenter -t <pid>` で子ランタイムを参加させる）。
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// netns の proc パス（`nsenter --net=<path>`）。
    #[must_use]
    pub fn netns_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/{}/ns/net", self.pid))
    }

    /// userns の proc パス（`nsenter --user=<path>`）。
    #[must_use]
    pub fn userns_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/{}/ns/user", self.pid))
    }

    /// 受領した TCP リスナ群（ポート→リスナ）。所有権は呼び出し側に移す。
    #[must_use]
    pub fn take_tcp(&mut self) -> Vec<(u16, TcpListener)> {
        std::mem::take(&mut self.tcp)
    }

    /// 受領した DNS 用 UDP ソケットを複製して返す。
    pub fn dns_socket(&self) -> io::Result<UdpSocket> {
        self.dns.try_clone()
    }

    /// 制御ソケットを閉じ holder を確実に終了させる（netns 破棄）。
    pub fn shutdown(mut self) {
        // _ctrl の drop で holder は EOF→自発終了するが、確実性のため kill も行う。
        let _ = self.holder.kill();
        let _ = self.holder.wait();
    }
}

impl Drop for Netns {
    fn drop(&mut self) {
        let _ = self.holder.kill();
        let _ = self.holder.wait();
    }
}

/// 緩いタイムアウト付き accept（holder 起動失敗でハングしない）。
fn accept_with_timeout(
    listener: &UnixListener,
    timeout: std::time::Duration,
) -> io::Result<(UnixStream, std::os::unix::net::SocketAddr)> {
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok(pair) => return Ok(pair),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "netns holder did not connect in time",
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
}

/// 制御ソケットから本文（ポート CSV）と SCM_RIGHTS の FD 群を受信する。
fn recv_fds(stream: &UnixStream, max_fds: usize) -> io::Result<(Vec<u16>, Vec<OwnedFd>)> {
    stream.set_nonblocking(false)?;
    let mut buf = [0u8; 512];
    let mut iov = [IoSliceMut::new(&mut buf)];
    // cmsg バッファ: 最大 max_fds 個の RawFd を収容できる領域。
    let mut cmsg_space = nix::cmsg_space!([RawFd; 64]);
    let fd = stream.as_raw_fd();
    let msg = recvmsg::<UnixAddr>(fd, &mut iov, Some(&mut cmsg_space), MsgFlags::empty())
        .map_err(io_from_errno)?;

    let mut fds: Vec<OwnedFd> = Vec::new();
    for cmsg in msg.cmsgs().map_err(io_from_errno)? {
        if let ControlMessageOwned::ScmRights(received) = cmsg {
            for raw in received {
                if fds.len() >= max_fds {
                    // 予期しない過剰 FD は閉じる（リーク防止）。
                    let _ = unsafe { OwnedFd::from_raw_fd(raw) };
                    continue;
                }
                // SCM_RIGHTS で受領した FD の所有権をここで得る。
                fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
            }
        }
    }

    let n = msg.bytes;
    let ports: Vec<u16> = std::str::from_utf8(&buf[..n])
        .map_err(|_| io::Error::other("holder payload not utf8"))?
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u16>().map_err(|_| io::Error::other("bad port")))
        .collect::<Result<_, _>>()?;
    Ok((ports, fds))
}

fn io_from_errno(e: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}
