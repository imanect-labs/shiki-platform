//! Firecracker ゲストの PID1 エージェント。proc/sys/dev をマウントし、ワークスペースドライブを
//! `/workspace` に載せ、vsock でホストの要求（exec/ファイル/停止）を逐次処理する。
//!
//! 隔離境界は VM（KVM）そのもの。ホスト由来入力は敵対的として最小限の検証で扱う。

mod exec;
mod fsops;
mod vsock;

use std::io::Write;

use nix::mount::{mount, MsFlags};
use nix::sys::reboot::{reboot, RebootMode};
use shiki_sandbox_agent_proto::{read_frame, write_frame, Event, Request};

use vsock::{VsockConn, VsockListener};

/// ゲストがリッスンする vsock ポート（ホストの `CONNECT <port>` と一致）。
const AGENT_PORT: u32 = 5000;
/// ワークスペース ext4 が載るブロックデバイス（FC の 2 番目ドライブ）。
const WORKSPACE_DEV: &str = "/dev/vdb";
const WORKSPACE_DIR: &str = "/workspace";

fn main() -> std::process::ExitCode {
    // PID1 として最低限のマウントを整える（失敗は致命的でないものは無視）。
    setup_mounts();
    // ネットワーク（egress 時）: resolv.conf をゲートウェイに向ける。kernel の ip= が IF を上げている。
    let _ = std::fs::write("/etc/resolv.conf", "nameserver 169.254.0.1\n");

    let listener = match VsockListener::bind_any(AGENT_PORT) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("agent: vsock bind failed: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // 1 接続ずつ処理する（切断されたら再 accept）。Shutdown 要求で電源オフ。
    loop {
        match listener.accept() {
            Ok(conn) => {
                if serve(conn) {
                    break; // Shutdown を受けた。
                }
            }
            Err(e) => {
                eprintln!("agent: accept failed: {e}");
            }
        }
    }

    // 電源オフ（FC が VM を終了）。到達しなければホストが SIGKILL する。
    let _ = reboot(RebootMode::RB_POWER_OFF);
    std::process::ExitCode::SUCCESS
}

/// 1 接続を処理する。Shutdown を受けたら `true`。
fn serve(mut conn: VsockConn) -> bool {
    // 起動完了通知。
    if write_frame(
        &mut conn,
        &Event::Ready {
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .is_err()
    {
        return false;
    }
    loop {
        let req: Option<Request> = match read_frame(&mut conn) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("agent: read_frame: {e}");
                return false;
            }
        };
        let Some(req) = req else {
            return false; // ホストが切断。
        };
        match req {
            Request::Shutdown => {
                let _ = write_frame(&mut conn, &Event::Ok);
                let _ = conn.flush();
                return true;
            }
            Request::Exec { argv, timeout_ms } => {
                exec::run(&mut conn, &argv, timeout_ms, WORKSPACE_DIR);
            }
            Request::WriteFile { path, b64 } => {
                let ev = fsops::write_file(&path, &b64);
                let _ = write_frame(&mut conn, &ev);
            }
            Request::ReadFile { path } => {
                let ev = fsops::read_file(&path);
                let _ = write_frame(&mut conn, &ev);
            }
            Request::ListDir { path } => {
                let ev = fsops::list_dir(&path);
                let _ = write_frame(&mut conn, &ev);
            }
        }
    }
}

/// proc/sysfs/devtmpfs/tmpfs とワークスペースドライブをマウントする。
fn setup_mounts() {
    let n = None::<&str>;
    let _ = mount(n, "/proc", Some("proc"), MsFlags::empty(), n);
    let _ = mount(n, "/sys", Some("sysfs"), MsFlags::empty(), n);
    let _ = mount(n, "/dev", Some("devtmpfs"), MsFlags::empty(), n);
    let _ = std::fs::create_dir_all("/dev/pts");
    let _ = mount(n, "/dev/pts", Some("devpts"), MsFlags::empty(), n);
    let _ = std::fs::create_dir_all("/dev/shm");
    let _ = mount(n, "/dev/shm", Some("tmpfs"), MsFlags::empty(), n);
    let _ = mount(n, "/tmp", Some("tmpfs"), MsFlags::empty(), n);
    // ワークスペース ext4 ドライブ。
    let _ = std::fs::create_dir_all(WORKSPACE_DIR);
    let _ = mount(
        Some(WORKSPACE_DEV),
        WORKSPACE_DIR,
        Some("ext4"),
        MsFlags::empty(),
        n,
    );
}
