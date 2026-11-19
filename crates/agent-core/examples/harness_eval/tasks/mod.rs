//! 評価課題（決定的に採点できる、メモリ上のツールだけで閉じた課題）。

mod codefix;
mod config;
mod csvq;
mod docedit;
mod ledger;
mod logs;
mod refactor;
mod sales;
mod tickets;
mod vfs;

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolOutcome};
use authz::AuthContext;

pub(crate) const ALL: &[&str] = &["ledger", "codefix", "tickets", "tickets-long"];

/// 多様な課題（ファイル編集・データ分析）。`--task diverse` で選ぶ。
pub(crate) const DIVERSE: &[&str] = &[
    "csv-sales",
    "csv-join",
    "refactor",
    "config",
    "logs",
    "docedit",
];

pub(crate) struct Grade {
    /// 0.0〜1.0 の部分点。
    pub(crate) score: f64,
    /// 完全正解か。
    pub(crate) success: bool,
    pub(crate) details: serde_json::Value,
}

pub(crate) trait Task: Send + Sync {
    fn system(&self) -> String;
    fn prompt(&self) -> String;
    fn max_steps(&self) -> usize;
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    fn grade(&self) -> Grade;
    /// 最後に呼ぶべきツール（`full` の variant で `AgentOptions::final_tool` に渡す）。
    fn final_tool(&self) -> &'static str {
        "submit_answer"
    }
}

pub(crate) fn make(name: &str, seed: u64) -> Box<dyn Task> {
    match name {
        "ledger" => Box::new(ledger::Ledger::new(seed)),
        "codefix" => Box::new(codefix::CodeFix::new()),
        "tickets" => Box::new(tickets::Tickets::new()),
        "tickets-long" => Box::new(tickets::Tickets::long(seed)),
        "csv-sales" => Box::new(sales::Sales::new(seed, false)),
        "csv-join" => Box::new(sales::Sales::new(seed, true)),
        "refactor" => Box::new(refactor::Refactor::new(seed)),
        "config" => Box::new(config::Config::new(seed)),
        "logs" => Box::new(logs::Logs::new(seed)),
        "docedit" => Box::new(docedit::DocEdit::new(seed)),
        other => panic!("unknown task {other}"),
    }
}

type Handler = dyn Fn(&serde_json::Value) -> Result<String, String> + Send + Sync;

/// クロージャで書くツール（製品のツールと同じく、引数は型どおりに厳格に読む）。
pub(crate) struct FnTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    read_only: bool,
    handler: Box<Handler>,
}

impl FnTool {
    pub(crate) fn new(
        name: &str,
        description: &str,
        schema: serde_json::Value,
        read_only: bool,
        handler: impl Fn(&serde_json::Value) -> Result<String, String> + Send + Sync + 'static,
    ) -> Arc<dyn Tool> {
        Arc::new(FnTool {
            name: name.into(),
            description: description.into(),
            schema,
            read_only,
            handler: Box::new(handler),
        })
    }
}

#[async_trait::async_trait]
impl Tool for FnTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn input_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }
    fn is_read_only(&self) -> bool {
        self.read_only
    }
    async fn call(
        &self,
        _ctx: &AuthContext,
        input: serde_json::Value,
        _trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        Ok(match (self.handler)(&input) {
            Ok(s) => ToolOutcome::ok(s),
            Err(e) => ToolOutcome::error(e),
        })
    }
}

/// 文字列引数（必須・型違いはエラー）。
pub(crate) fn arg_str<'a>(v: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    match v.get(key) {
        Some(serde_json::Value::String(s)) => Ok(s),
        Some(other) => Err(format!(
            "invalid tool input: `{key}` must be a string, got {other}"
        )),
        None => Err(format!(
            "invalid tool input: missing required field `{key}`"
        )),
    }
}

/// 整数引数（省略可・型違いはエラー）。
pub(crate) fn arg_opt_u64(v: &serde_json::Value, key: &str) -> Result<Option<u64>, String> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("invalid tool input: `{key}` must be a non-negative integer")),
        Some(other) => Err(format!(
            "invalid tool input: `{key}` must be an integer, got {other}"
        )),
    }
}

/// 決定的な擬似乱数（xorshift）。
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub(crate) fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
    pub(crate) fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() % xs.len() as u64) as usize]
    }
}

/// 四則演算の式を計算する（`calculate` ツール・製品の code_interpreter の代わり）。
pub(crate) fn calculate(expr: &str) -> Result<f64, String> {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && (self.s[self.i] as char).is_whitespace() {
                self.i += 1;
            }
        }
        fn expr(&mut self) -> Result<f64, String> {
            let mut v = self.term()?;
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'+') => {
                        self.i += 1;
                        v += self.term()?;
                    }
                    Some(b'-') => {
                        self.i += 1;
                        v -= self.term()?;
                    }
                    _ => return Ok(v),
                }
            }
        }
        fn term(&mut self) -> Result<f64, String> {
            let mut v = self.atom()?;
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'*') => {
                        self.i += 1;
                        v *= self.atom()?;
                    }
                    Some(b'/') => {
                        self.i += 1;
                        v /= self.atom()?;
                    }
                    _ => return Ok(v),
                }
            }
        }
        fn atom(&mut self) -> Result<f64, String> {
            self.ws();
            match self.s.get(self.i) {
                Some(b'(') => {
                    self.i += 1;
                    let v = self.expr()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b')') {
                        return Err("missing )".into());
                    }
                    self.i += 1;
                    Ok(v)
                }
                Some(b'-') => {
                    self.i += 1;
                    Ok(-self.atom()?)
                }
                _ => {
                    let start = self.i;
                    while self.i < self.s.len()
                        && (self.s[self.i].is_ascii_digit()
                            || matches!(self.s[self.i], b'.' | b','))
                    {
                        self.i += 1;
                    }
                    let t = std::str::from_utf8(&self.s[start..self.i])
                        .unwrap_or("")
                        .replace(',', "");
                    t.parse::<f64>()
                        .map_err(|_| format!("number expected at {start}"))
                }
            }
        }
    }
    let mut p = P {
        s: expr.as_bytes(),
        i: 0,
    };
    let v = p.expr()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("unexpected input at {}", p.i));
    }
    Ok(v)
}
