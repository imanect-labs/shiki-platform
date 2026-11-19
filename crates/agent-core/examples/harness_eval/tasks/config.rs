//! 設定の移行課題: 8 サービスの設定ファイル（YAML 風・v1）を v2 の形式へ書き換える。
//!
//! 規則の一部（例外の扱い）はプロンプトに無く、docs/config-v2.md にだけ書いてある（読みに行けるか）。
//! 採点はファイルを読み込んだキーと値の対応で比べる（順序と空白は問わない・コメントは残す）。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use agent_core::Tool;
use serde_json::json;

use super::{arg_str, vfs, FnTool, Grade, Rng, Task};

const SERVICES: &[&str] = &[
    "api", "auth", "billing", "search", "notify", "export", "gateway", "worker",
];

const DOC: &str = "# 設定ファイル v2 への移行\n\n\
## 基本の規則\n\n\
- 先頭（コメントを除く最初の行）に `version: 2` を置く。\n\
- `db_host` と `db_port` は `database:` の下に `host:` と `port:` として入れ子にする（`db_host` があった位置に置く）。\n\
- 時間の値（`timeout`・`*_timeout`）はキー名に `_ms` を付け、単位の無いミリ秒の整数にする（`30s` → `30000`、`2m` → `120000`、`1500ms` → `1500`）。\n\
- `legacy_mode` は廃止。行ごと削除する。\n\
- `features:` の下の値 `on` / `off` は `true` / `false` にする。\n\
- コメント行（`#` で始まる行）は消さない。\n\n\
## 例外\n\n\
- `db_host` だけがあって `db_port` が無いサービスでは、`port: 5432` を補う。\n\
- `features:` の下の値 `yes` / `no` も `true` / `false` にする。\n\
- `timeout: 0`（無制限）は `timeout_ms: 0` にする（削除しない）。\n\
- `db_host` が無いサービスには `database:` を作らない。\n";

pub(crate) struct Config {
    expected: BTreeMap<String, BTreeMap<String, String>>,
    comments: Vec<(String, String)>,
    files: vfs::Files,
    done: Arc<Mutex<Option<String>>>,
}

fn ms(v: &str) -> String {
    if let Some(x) = v.strip_suffix("ms") {
        x.to_string()
    } else if let Some(x) = v.strip_suffix('s') {
        (x.parse::<u64>().unwrap() * 1000).to_string()
    } else if let Some(x) = v.strip_suffix('m') {
        (x.parse::<u64>().unwrap() * 60_000).to_string()
    } else {
        v.to_string()
    }
}

/// YAML 風の 2 段のキーと値（`a: 1` / `a:` + `  b: 2` → `a.b`）。コメントと空行は除く。
pub(crate) fn parse(body: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut parent: Option<String> = None;
    for line in body.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((k, v)) = t.split_once(':') else {
            out.insert(format!("?{t}"), String::new());
            continue;
        };
        let (k, v) = (k.trim(), v.trim().trim_matches('"').trim_matches('\''));
        if line.starts_with(' ') {
            let p = parent.clone().unwrap_or_default();
            out.insert(format!("{p}.{k}"), v.to_string());
        } else if v.is_empty() {
            parent = Some(k.to_string());
        } else {
            parent = None;
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

impl Config {
    pub(crate) fn new(seed: u64, xl: bool) -> Self {
        let mut rng = Rng::new(seed ^ 0xc0f1);
        let mut files = BTreeMap::new();
        let mut expected = BTreeMap::new();
        let mut comments = Vec::new();
        files.insert("docs/config-v2.md".to_string(), DOC.to_string());
        let mut services: Vec<String> = SERVICES.iter().map(|s| (*s).to_string()).collect();
        if xl {
            services.extend((9..=20).map(|k| format!("svc{k:02}")));
        }
        for (i, svc) in services.iter().enumerate() {
            let path = format!("services/{svc}.yaml");
            let mut v1 = format!(
                "# {svc} サービスの設定（担当: チーム{}）\n",
                rng.range(1, 5)
            );
            let mut want = BTreeMap::new();
            want.insert("version".to_string(), "2".to_string());
            v1.push_str(&format!("name: {svc}\n"));
            want.insert("name".into(), svc.clone());
            // DB: 無い（2 サービス）/ ホストだけ（2）/ 両方。
            match i % 4 {
                0 => {}
                1 => {
                    let h = format!("db-{}.internal", rng.range(1, 9));
                    v1.push_str(&format!("db_host: {h}\n"));
                    want.insert("database.host".into(), h);
                    want.insert("database.port".into(), "5432".into());
                }
                _ => {
                    let h = format!("db-{}.internal", rng.range(1, 9));
                    let p = rng.range(5400, 5499).to_string();
                    v1.push_str(&format!(
                        "db_host: {h}\n# 本番は読み取り専用のレプリカを使う\ndb_port: {p}\n"
                    ));
                    comments.push((
                        path.clone(),
                        "# 本番は読み取り専用のレプリカを使う".to_string(),
                    ));
                    want.insert("database.host".into(), h);
                    want.insert("database.port".into(), p);
                }
            }
            let timeout = match rng.range(0, 3) {
                0 => format!("{}s", rng.range(5, 90)),
                1 => format!("{}m", rng.range(1, 5)),
                2 => format!("{}ms", rng.range(200, 9000)),
                _ => "0".into(),
            };
            v1.push_str(&format!("timeout: {timeout}\n"));
            want.insert("timeout_ms".into(), ms(&timeout));
            if i % 3 != 2 {
                let rt = format!("{}ms", rng.range(100, 3000));
                v1.push_str(&format!("retry_timeout: {rt}\n"));
                want.insert("retry_timeout_ms".into(), ms(&rt));
            }
            if i % 2 == 0 {
                v1.push_str(&format!("legacy_mode: {}\n", rng.range(0, 1) == 1));
            }
            if i % 4 != 3 {
                v1.push_str("features:\n");
                for f in ["cache", "tracing", "audit"] {
                    if rng.range(0, 3) == 0 {
                        continue;
                    }
                    let (raw, b) = match rng.range(0, 3) {
                        0 => ("on", "true"),
                        1 => ("off", "false"),
                        2 => ("yes", "true"),
                        _ => ("no", "false"),
                    };
                    v1.push_str(&format!("  {f}: {raw}\n"));
                    want.insert(format!("features.{f}"), b.to_string());
                }
            }
            let r = rng.range(1, 6).to_string();
            v1.push_str(&format!("replicas: {r}\n"));
            want.insert("replicas".into(), r);
            comments.push((path.clone(), v1.lines().next().unwrap().to_string()));
            files.insert(path.clone(), v1);
            expected.insert(path, want);
        }
        Config {
            expected,
            comments,
            files: vfs::files(files),
            done: Arc::new(Mutex::new(None)),
        }
    }

    fn check(&self, files: &BTreeMap<String, String>) -> (usize, bool, bool, Vec<String>) {
        let mut ok = 0;
        let mut errors = Vec::new();
        for (path, want) in &self.expected {
            let body = files.get(path).map_or("", String::as_str);
            let got = parse(body);
            let first = body
                .lines()
                .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'));
            if &got == want && first.map(str::trim) == Some("version: 2") {
                ok += 1;
            } else {
                let mut diff = Vec::new();
                for (k, v) in want {
                    if got.get(k) != Some(v) {
                        diff.push(format!(
                            "{k}: 期待 {v} / 実際 {}",
                            got.get(k).map_or("（無し）", String::as_str)
                        ));
                    }
                }
                for k in got.keys().filter(|k| !want.contains_key(*k)) {
                    diff.push(format!("{k}: 余分なキー"));
                }
                if first.map(str::trim) != Some("version: 2") {
                    diff.push("先頭が version: 2 ではない".into());
                }
                errors.push(format!("{path}: {}", diff.join(" / ")));
            }
        }
        let comments_kept = self.comments.iter().all(|(p, c)| {
            files
                .get(p)
                .is_some_and(|b| b.lines().any(|l| l.trim() == c.trim()))
        });
        let extra = files.len() != self.expected.len() + 1;
        (ok, comments_kept, extra, errors)
    }
}

impl Task for Config {
    fn system(&self) -> String {
        "あなたはインフラ担当のエンジニアです。ファイルはツールで読み書きしてください。".into()
    }

    fn prompt(&self) -> String {
        format!("services/ 以下の {} 個の設定ファイルを、v1 の形式から v2 の形式に移行してください。\
         移行の規則は docs/config-v2.md にあります（例外の扱いもそこに書いてあるので、必ず読んでください）。\
         各ファイルを書き換えたら validate_config で確かめ、全ファイルが通ったら finish を呼んでください。", self.expected.len())
    }

    fn max_steps(&self) -> usize {
        50
    }

    fn final_tool(&self) -> &'static str {
        "finish"
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut t = vfs::tools(&self.files, true);
        t.push(vfs::finish_tool(&self.done));
        let files = self.files.clone();
        // 検証は形式だけを見る（値の正しさは教えない・製品のスキーマ検証に相当）。
        t.push(FnTool::new(
            "validate_config",
            "設定ファイルが v2 のスキーマに合っているかを検証する（値が正しいかまでは見ない）。",
            json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            true,
            move |v| {
                let path = arg_str(v, "path")?;
                let fs = files.lock().unwrap();
                let body = fs.get(path).ok_or_else(|| format!("no such file: {path}"))?;
                let m = parse(body);
                let mut errs = Vec::new();
                let allowed = ["version", "name", "timeout_ms", "retry_timeout_ms", "replicas"];
                for (k, val) in &m {
                    let top = k.split('.').next().unwrap_or("");
                    if !(allowed.contains(&k.as_str()) || ["database", "features"].contains(&top)) {
                        errs.push(format!("unknown key `{k}`"));
                    }
                    if k.ends_with("_ms") && val.parse::<u64>().is_err() {
                        errs.push(format!("`{k}` must be an integer (milliseconds)"));
                    }
                    if k.starts_with("features.") && !["true", "false"].contains(&val.as_str()) {
                        errs.push(format!("`{k}` must be true or false"));
                    }
                }
                if m.get("version").map(String::as_str) != Some("2") {
                    errs.push("`version: 2` is required".into());
                }
                if m.contains_key("database.host") != m.contains_key("database.port") {
                    errs.push("database requires both host and port".into());
                }
                Ok(if errs.is_empty() {
                    format!("{path}: OK")
                } else {
                    format!("{path}: {} errors\n{}", errs.len(), errs.join("\n"))
                })
            },
        ));
        t
    }

    fn grade(&self) -> Grade {
        let snapshot = self.files.lock().unwrap().clone();
        let (ok, comments, extra, errors) = self.check(&snapshot);
        let n = self.expected.len();
        let score = 0.85 * ok as f64 / n as f64 + if comments { 0.15 } else { 0.0 };
        Grade {
            score,
            success: ok == n && comments && !extra,
            details: json!({"files_ok": ok, "files": n, "comments_kept": comments, "extra_files": extra,
                            "errors": errors, "finished": self.done.lock().unwrap().is_some()}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_nested_keys_and_check_original_fails() {
        let m = parse("# c\nversion: 2\ndatabase:\n  host: a\n  port: 1\nreplicas: 2\n");
        assert_eq!(m["database.host"], "a");
        assert_eq!(m["replicas"], "2");
        let c = Config::new(1, true);
        let snap = c.files.lock().unwrap().clone();
        let (ok, comments, _, _) = c.check(&snap);
        assert_eq!(ok, 0);
        assert!(comments);
    }
}
