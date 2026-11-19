//! ログ解析課題: 8 本のアクセスログ（計 4,000 行・約 45 万字）を調べ、障害調査のレポートを
//! `report.md` に書く。
//!
//! 全部を読むと窓に載らないので、`search`（一致件数と先頭 80 件）と行範囲の読み取りで調べる。
//! 答えはレポートの決まった形から読み取って採点する（書き込みツールで成果物を作れるか）。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use agent_core::Tool;
use serde_json::json;

use super::{vfs, Grade, Rng, Task};

const SVCS: &[&str] = &["api", "auth", "billing", "search"];
const PATHS: &[&str] = &[
    "/v1/orders",
    "/v1/users",
    "/v1/login",
    "/v1/invoices",
    "/v1/search",
    "/v1/items",
    "/health",
];
const MSGS: &[&str] = &[
    "request completed",
    "cache hit",
    "cache miss; loaded from store",
    "user session refreshed",
    "rate limit check passed",
];

pub(crate) struct Logs {
    n_files: usize,
    five_xx: BTreeMap<String, usize>,
    top_ip: (String, usize),
    refused: (String, String),
    billing_warn: usize,
    files: vfs::Files,
    done: Arc<Mutex<Option<String>>>,
}

impl Logs {
    pub(crate) fn new(seed: u64, n_files: usize) -> Self {
        let mut rng = Rng::new(seed ^ 0x10c5);
        let mut files = BTreeMap::new();
        let mut five_xx: BTreeMap<String, usize> =
            SVCS.iter().map(|s| ((*s).to_string(), 0)).collect();
        let mut ips: BTreeMap<String, usize> = BTreeMap::new();
        let mut refused: Vec<String> = Vec::new();
        let mut billing_warn = 0;
        let bad_ip = format!("10.0.{}.{}", rng.range(1, 9), rng.range(10, 250));
        let mut sec: u64 = 8 * 3600;
        for f in 0..n_files {
            let mut body = String::new();
            for _ in 0..500 {
                sec += rng.range(1, 9);
                let ts = format!(
                    "2026-03-14T{:02}:{:02}:{:02}Z",
                    sec / 3600 % 24,
                    sec / 60 % 60,
                    sec % 60
                );
                let svc = *rng.pick(SVCS);
                let roll = rng.range(0, 999);
                let (level, status, msg) = if roll < 16 {
                    ("ERROR", 500 + rng.range(0, 3), "upstream error")
                } else if roll < 22 && (3..6).contains(&f) {
                    ("ERROR", 503, "db: connection refused")
                } else if roll < 90 {
                    ("WARN", 200, "slow response")
                } else {
                    (
                        "INFO",
                        if rng.range(0, 9) == 0 { 404 } else { 200 },
                        *rng.pick(MSGS),
                    )
                };
                let ip = if status >= 500 && rng.range(0, 9) < 4 {
                    bad_ip.clone()
                } else {
                    format!("10.0.{}.{}", rng.range(1, 9), rng.range(10, 250))
                };
                if status >= 500 {
                    *five_xx.get_mut(svc).unwrap() += 1;
                    *ips.entry(ip.clone()).or_default() += 1;
                }
                if msg.contains("connection refused") {
                    refused.push(ts.clone());
                }
                if level == "WARN" && svc == "billing" {
                    billing_warn += 1;
                }
                body.push_str(&format!(
                    "{ts} {level} svc={svc} status={status} path={} ms={} ip={ip} msg=\"{msg}\" req={:08x}\n",
                    rng.pick(PATHS),
                    rng.range(3, if level == "WARN" { 9000 } else { 400 }),
                    rng.next() as u32
                ));
            }
            files.insert(format!("logs/app-{:02}.log", f + 1), body);
        }
        let top_ip = ips.into_iter().max_by_key(|(_, c)| *c).unwrap();
        Logs {
            n_files,
            five_xx,
            top_ip,
            refused: (
                refused.first().cloned().unwrap_or_default(),
                refused.last().cloned().unwrap_or_default(),
            ),
            billing_warn,
            files: vfs::files(files),
            done: Arc::new(Mutex::new(None)),
        }
    }
}

/// `label` の行（`- api: 12` など）から最初の整数を読む。
fn number_after(report: &str, label: &str) -> Option<usize> {
    report.lines().find_map(|l| {
        let t = l.trim().trim_start_matches(['-', '*']).trim();
        let rest = t.strip_prefix(label)?;
        let digits: String = rest
            .trim_start_matches([':', '：', ' '])
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

/// 見出し `heading` から次の見出しまで。
fn section<'a>(report: &'a str, heading: &str) -> &'a str {
    let Some(at) = report.find(heading) else {
        return "";
    };
    let rest = &report[at + heading.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    &rest[..end]
}

impl Task for Logs {
    fn system(&self) -> String {
        "あなたは SRE です。ログはツールで調べ、数えた値だけを書いてください（推測で埋めない）。"
            .into()
    }

    fn prompt(&self) -> String {
        format!("logs/ 以下の {} 本のログ（2026-03-14 の 1 日分・時刻順）から障害を調べ、`report.md` に次の形でレポートを書いてください。\
         ログの 1 行は `時刻 レベル svc=サービス status=ステータス path=… ms=… ip=… msg=\"…\" req=…` の形です。\
         ログは大きいので、全部は読まずに search（一致件数を返す）を使ってください。\n\n\
         ```\n# 障害調査レポート\n\n## 5xx の件数\n- api: <件数>\n- auth: <件数>\n- billing: <件数>\n- search: <件数>\n\n\
         ## 5xx が最も多い IP\n- <IP>（<件数> 件）\n\n## connection refused\n- 最初: <時刻>\n- 最後: <時刻>\n\n\
         ## billing の WARN\n- <件数> 件\n```\n\n\
         5xx は status が 500〜599 の行です。時刻はログに書かれた形（例: 2026-03-14T09:15:02Z）のまま書いてください。\
         書き終えたら finish を呼んでください。", self.n_files)
    }

    fn max_steps(&self) -> usize {
        40
    }

    fn final_tool(&self) -> &'static str {
        "finish"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut t = vfs::tools(&self.files, true);
        t.push(vfs::finish_tool(&self.done));
        t
    }

    fn grade(&self) -> Grade {
        let report = self
            .files
            .lock()
            .unwrap()
            .get("report.md")
            .cloned()
            .unwrap_or_default();
        let expected = json!({"five_xx": self.five_xx, "top_ip": self.top_ip,
                              "refused": self.refused, "billing_warn": self.billing_warn});
        if report.is_empty() {
            return Grade {
                score: 0.0,
                success: false,
                details: json!({"error": "no report.md", "expected": expected}),
            };
        }
        let five = section(&report, "## 5xx の件数");
        let svc_ok = self
            .five_xx
            .iter()
            .filter(|(s, n)| number_after(five, s) == Some(**n))
            .count();
        let ip_ok = section(&report, "## 5xx が最も多い IP").contains(&self.top_ip.0);
        let refused = section(&report, "## connection refused");
        let first_ok = refused
            .lines()
            .any(|l| l.contains("最初") && l.contains(&self.refused.0));
        let last_ok = refused
            .lines()
            .any(|l| l.contains("最後") && l.contains(&self.refused.1));
        let warn = section(&report, "## billing の WARN");
        let warn_ok = warn
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .any(|s| s.parse::<usize>().ok() == Some(self.billing_warn));
        let score = 0.1 * svc_ok as f64
            + if ip_ok { 0.15 } else { 0.0 }
            + if first_ok { 0.15 } else { 0.0 }
            + if last_ok { 0.15 } else { 0.0 }
            + if warn_ok { 0.15 } else { 0.0 };
        Grade {
            score,
            success: score > 0.999,
            details: json!({"report": report, "expected": expected,
                            "parts": {"svc": svc_ok, "ip": ip_ok, "first": first_ok, "last": last_ok, "warn": warn_ok},
                            "finished": self.done.lock().unwrap().is_some()}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_correct_report_scores_full() {
        let l = Logs::new(2, 8);
        let mut r = String::from("# 障害調査レポート\n\n## 5xx の件数\n");
        for (s, n) in &l.five_xx {
            r.push_str(&format!("- {s}: {n}\n"));
        }
        r.push_str(&format!(
            "\n## 5xx が最も多い IP\n- {}（{} 件）\n\n## connection refused\n- 最初: {}\n- 最後: {}\n\n## billing の WARN\n- {} 件\n",
            l.top_ip.0, l.top_ip.1, l.refused.0, l.refused.1, l.billing_warn
        ));
        l.files.lock().unwrap().insert("report.md".into(), r);
        let g = l.grade();
        assert!(g.success, "{}", g.details);
        // 5xx の件数は search で数えられる形（svc と status が隣り合う）。
        let fs = l.files.lock().unwrap();
        let api: usize = fs
            .values()
            .map(|b| b.matches("svc=api status=5").count())
            .sum();
        assert_eq!(api, l.five_xx["api"]);
    }
}
