//! 子孫プロセスの RSS 合算（cgroup が無い環境での近似メモリ計測）。
//!
//! `/proc/<pid>/stat` から ppid を、`/proc/<pid>/status` から VmRSS を読み、`root` の子孫（root 自身は
//! 除く＝ベンチプロセスのメモリを混ぜない）の VmRSS を合計する。バックエンドが spawn した
//! sidecar/runsc/firecracker のメモリを拾う。

use std::collections::HashMap;

/// `root` の全子孫プロセスの VmRSS 合計（KiB）。
#[must_use]
pub(crate) fn descendant_rss_kb(root: u32) -> u64 {
    let procs = read_procs();
    // pid -> ppid マップ。
    let ppid: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();

    let mut total = 0u64;
    for p in &procs {
        if p.pid != root && is_descendant(p.pid, root, &ppid) {
            total += p.rss_kb;
        }
    }
    total
}

struct Proc {
    pid: u32,
    ppid: u32,
    rss_kb: u64,
}

fn is_descendant(mut pid: u32, root: u32, ppid: &HashMap<u32, u32>) -> bool {
    let mut guard = 0;
    while let Some(&parent) = ppid.get(&pid) {
        guard += 1;
        if guard > 4096 {
            return false; // ループ保険。
        }
        if parent == root {
            return true;
        }
        if parent == 0 || parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}

fn read_procs() -> Vec<Proc> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if let Some(p) = read_one(pid) {
            out.push(p);
        }
    }
    out
}

fn read_one(pid: u32) -> Option<Proc> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // ppid は comm 内に空白/括弧がありうるので、最後の ')' 以降を使う。
    let after = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    // after の先頭は state, 次が ppid。
    let ppid = fields.get(1)?.parse::<u32>().ok()?;

    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kb = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(0);

    Some(Proc { pid, ppid, rss_kb })
}
