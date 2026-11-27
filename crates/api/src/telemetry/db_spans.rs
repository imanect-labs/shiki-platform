//! DB クエリの所要時間をトレースの子 span として出す（リクエスト単位で「どのクエリが遅いか」を見る）。
//!
//! sqlx はクエリの終了時に `sqlx::query` ターゲットのイベント（経過時間・行数・SQL）を出す。
//! このレイヤはそのイベントを受け、**終了時刻から経過時間を引いた時刻を開始とする client span** を
//! 現在の span（リクエスト span など）の子として後付けで作る。呼び出し側（約 2000 箇所の
//! `sqlx::query*`）は書き換えない。
//!
//! 載せる属性は OTel の DB semantic conventions の一部だけ:
//! `db.system.name`・`db.operation.name`・`db.collection.name`・`db.query.summary`・
//! `db.response.returned_rows`（と `shiki.db.rows_affected`）。
//! **SQL 本文とバインド値は載せない。** SQL はこのレイヤの中で操作名・表名を取り出すのにだけ使う
//! （sqlx の SQL はプレースホルダのままだが、本文を外に出すかは span 属性の方針に従うため）。
//!
//! 親の span が無い・サンプルされていない（バックグラウンドのポーリング等）クエリは span にしない。
//! トレースの量を、リクエストのトレースに付く分だけに抑えるため。

use std::time::{Duration, SystemTime};

use opentelemetry::{
    trace::{Span as _, SpanKind, TraceContextExt, Tracer},
    KeyValue,
};
use tracing::{
    field::{Field, Visit},
    Event, Subscriber,
};
use tracing_opentelemetry::{OtelData, PreSampledTracer};
use tracing_subscriber::{
    filter::{combinator::Or, FilterExt as _, Targets},
    layer::{Context, Filter},
    registry::LookupSpan,
    Layer,
};

/// sqlx がクエリごとのイベントを出すターゲット。
pub(crate) const SQLX_QUERY_TARGET: &str = "sqlx::query";

/// 表名として載せてよい識別子の最大長（Postgres の識別子の上限）。
const MAX_IDENT_LEN: usize = 63;

/// `sqlx::query` イベントを client span に変換するレイヤ。
pub(crate) struct DbSpanLayer<T> {
    tracer: T,
}

impl<T> DbSpanLayer<T> {
    pub(crate) fn new(tracer: T) -> Self {
        Self { tracer }
    }
}

/// `sqlx::query` の debug（sqlx の既定のクエリごとのレベル）以上だけを通すフィルタ。
pub(crate) fn query_events() -> Targets {
    Targets::new().with_target(SQLX_QUERY_TARGET, tracing::Level::DEBUG)
}

/// このレイヤに掛けるフィルタ。`spans` には OTel レイヤと同じフィルタを渡す。
///
/// per-layer フィルタの下では、レイヤから見える span もフィルタで絞られる。親のリクエスト span を
/// 引けるように「OTel レイヤに見える span ＋ `sqlx::query` のイベント」を通す。fmt レイヤには
/// 掛からないので、SQL がログに溢れることは無い。
pub(crate) fn filter<S, F>(spans: F) -> Or<F, Targets, S>
where
    F: Filter<S>,
{
    spans.or(query_events())
}

impl<S, T> Layer<S> for DbSpanLayer<T>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    T: Tracer + PreSampledTracer + Send + Sync + 'static,
    T::Span: Send + Sync + 'static,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if event.metadata().target() != SQLX_QUERY_TARGET {
            return;
        }
        // イベントの配送中は `Span::current()` が使えない（再入防止で no-op の dispatcher になる）ので、
        // 親の OTel の文脈は registry の span 拡張（OTel レイヤが入れた `OtelData`）から引く。
        let Some(span) = ctx.event_span(event) else {
            return;
        };
        let parent = {
            let mut ext = span.extensions_mut();
            let Some(data) = ext.get_mut::<OtelData>() else {
                return;
            };
            self.tracer.sampled_context(data)
        };
        if !parent.span().span_context().is_sampled() {
            return;
        }
        let mut fields = QueryFields::default();
        event.record(&mut fields);
        let Some(elapsed) = fields.elapsed() else {
            return;
        };
        let summary = summarize(fields.sql());

        let end = SystemTime::now();
        let start = end.checked_sub(elapsed).unwrap_or(end);
        let mut attributes = vec![
            KeyValue::new("db.system.name", "postgresql"),
            KeyValue::new("db.operation.name", summary.operation.clone()),
            KeyValue::new("db.query.summary", summary.name()),
            KeyValue::new("db.response.returned_rows", to_i64(fields.rows_returned)),
            KeyValue::new("shiki.db.rows_affected", to_i64(fields.rows_affected)),
        ];
        if let Some(table) = &summary.collection {
            attributes.push(KeyValue::new("db.collection.name", table.clone()));
        }
        let mut span = self
            .tracer
            .span_builder(summary.name())
            .with_kind(SpanKind::Client)
            .with_start_time(start)
            .with_attributes(attributes)
            .start_with_context(&self.tracer, &parent);
        span.end_with_timestamp(end);
    }
}

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// sqlx のイベントから拾うフィールド（`sqlx-core` の `logger.rs`）。
#[derive(Default)]
struct QueryFields {
    /// 先頭 4 語。SQL が 4 語以下なら SQL 全体。
    summary: String,
    /// SQL 全体（4 語を超えるときだけ入る）。外には出さない。
    statement: String,
    elapsed_secs: Option<f64>,
    rows_returned: u64,
    rows_affected: u64,
}

impl QueryFields {
    fn sql(&self) -> &str {
        if self.statement.trim().is_empty() {
            &self.summary
        } else {
            &self.statement
        }
    }

    fn elapsed(&self) -> Option<Duration> {
        self.elapsed_secs
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(Duration::from_secs_f64)
    }

    fn set_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "summary" => value.clone_into(&mut self.summary),
            "db.statement" => value.clone_into(&mut self.statement),
            _ => {}
        }
    }
}

impl Visit for QueryFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.set_str(field, value);
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.elapsed_secs = Some(value);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "rows_returned" => self.rows_returned = value,
            "rows_affected" => self.rows_affected = value,
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_u64(field, u64::try_from(value).unwrap_or(0));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // `String` を `?` で渡す版の sqlx でも拾えるように（`elapsed` 等の Debug は使わない）。
        if matches!(field.name(), "summary" | "db.statement") {
            let s = format!("{value:?}");
            let s = s
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(&s);
            self.set_str(field, s);
        }
    }
}

/// SQL から取り出した、外に出してよい要約（操作名と表名だけ）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct QuerySummary {
    /// `SELECT`・`INSERT` など。分からなければ `UNKNOWN`。
    pub(crate) operation: String,
    /// 主たる表（`FROM`・`INTO`・`UPDATE` の直後）。識別子の形でなければ `None`。
    pub(crate) collection: Option<String>,
}

impl QuerySummary {
    /// `db.query.summary` と span 名（`SELECT documents`）。
    pub(crate) fn name(&self) -> String {
        match &self.collection {
            Some(t) => format!("{} {t}", self.operation),
            None => self.operation.clone(),
        }
    }
}

/// SQL の字句（深さ 0 の語だけを使う）。
struct Token<'a> {
    text: &'a str,
    depth: u32,
}

/// 括弧の深さ付きで語に分ける。値・本文を拾わないため、文字列リテラル（`'…'`・`E'…'`）・
/// ドル引用（`$$…$$`・`$tag$…$tag$`）・コメント（`--`・入れ子の `/* */`）は読み飛ばす。
/// 二重引用符の識別子（`"My Table"`）は 1 語として扱う。
fn tokenize(sql: &str) -> Vec<Token<'_>> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut depth: u32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'\'' => {
                // `E'…'` はバックスラッシュでエスケープできる。直前の語 `E` は語として残さない。
                let escaped = tokens.last().is_some_and(|t: &Token<'_>| {
                    t.text.eq_ignore_ascii_case("e") && sql[..i].ends_with(t.text)
                });
                if escaped {
                    tokens.pop();
                    i = skip_escaped(bytes, i + 1);
                } else {
                    i = skip_quoted(bytes, i + 1, b'\'');
                }
            }
            b'$' => i = skip_dollar(bytes, i),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_until(bytes, i + 2, b"\n"),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i + 2),
            _ if is_ident_byte(c) || c == b'"' => {
                // 識別子（`public."My Table"` のように引用部分を含んでよい）を 1 語にする。
                let start = i;
                while i < bytes.len() && (is_ident_byte(bytes[i]) || bytes[i] == b'"') {
                    i = if bytes[i] == b'"' {
                        skip_quoted(bytes, i + 1, b'"')
                    } else {
                        i + 1
                    };
                }
                tokens.push(Token {
                    text: &sql[start..i],
                    depth,
                });
            }
            _ => i += 1,
        }
    }
    tokens
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.')
}

/// 引用（`q` で閉じる・`qq` は中の 1 文字）を読み飛ばし、閉じの直後の位置を返す。
fn skip_quoted(bytes: &[u8], mut i: usize, q: u8) -> usize {
    while i < bytes.len() {
        if bytes[i] == q {
            if bytes.get(i + 1) == Some(&q) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// `E'…'` の本文（`\\'` と `''` をエスケープとして）を読み飛ばす。
fn skip_escaped(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'\'' if bytes.get(i + 1) == Some(&b'\'') => i += 2,
            b'\'' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// `$` から始まるもの。`$1` などのプレースホルダは読み飛ばし、`$tag$…$tag$` のドル引用は本文ごと
/// 読み飛ばす。
fn skip_dollar(bytes: &[u8], start: usize) -> usize {
    let mut j = start + 1;
    if bytes.get(j).is_some_and(u8::is_ascii_digit) {
        while bytes.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        return j;
    }
    while bytes
        .get(j)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        j += 1;
    }
    if bytes.get(j) == Some(&b'$') {
        let tag = &bytes[start..=j];
        return skip_until(bytes, j + 1, tag);
    }
    start + 1
}

/// 入れ子の `/* … */` を読み飛ばす（Postgres はブロックコメントの入れ子を許す）。
fn skip_block_comment(bytes: &[u8], mut i: usize) -> usize {
    let mut level = 1u32;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            level += 1;
            i += 2;
        } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            level -= 1;
            i += 2;
            if level == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    bytes.len()
}

/// `from` から `end` を探し、その直後の位置を返す（見つからなければ末尾）。
fn skip_until(bytes: &[u8], from: usize, end: &[u8]) -> usize {
    bytes
        .get(from..)
        .and_then(|rest| rest.windows(end.len()).position(|w| w == end))
        .map_or(bytes.len(), |p| from + p + end.len())
}

const STATEMENTS: [&str; 5] = ["SELECT", "INSERT", "UPDATE", "DELETE", "MERGE"];

/// SQL を操作名と表名に要約する。SQL 本文・リテラルは返さない。
///
/// クエリごとに呼ぶので、キーワードの比較は確保なし（`eq_ignore_ascii_case`）で行う。
pub(crate) fn summarize(sql: &str) -> QuerySummary {
    let tokens = tokenize(sql);
    let Some(first) = tokens.first() else {
        return QuerySummary {
            operation: "UNKNOWN".into(),
            collection: None,
        };
    };
    let mut op = first.text;
    let mut op_index = 0;
    if is_kw(op, "WITH") {
        // CTE の後ろの本体の文（深さ 0 の最初の SELECT/INSERT/...）を操作とみなす。
        if let Some((i, t)) = tokens
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, t)| t.depth == 0 && STATEMENTS.iter().any(|s| is_kw(t.text, s)))
        {
            op_index = i;
            op = t.text;
        }
    }
    let rest = &tokens[op_index + 1..];
    let collection = if is_kw(op, "SELECT") || is_kw(op, "DELETE") {
        after_keyword(rest, "FROM")
    } else if is_kw(op, "INSERT") || is_kw(op, "MERGE") {
        after_keyword(rest, "INTO")
    } else if is_kw(op, "UPDATE") {
        main_table(rest)
    } else {
        None
    };
    let operation =
        if !op.is_empty() && op.len() <= 16 && op.bytes().all(|b| b.is_ascii_alphabetic()) {
            op.to_ascii_uppercase()
        } else {
            "UNKNOWN".into()
        };
    QuerySummary {
        operation,
        collection,
    }
}

fn is_kw(text: &str, kw: &str) -> bool {
    text.eq_ignore_ascii_case(kw)
}

fn after_keyword(tokens: &[Token<'_>], kw: &str) -> Option<String> {
    let pos = tokens
        .iter()
        .position(|t| t.depth == 0 && is_kw(t.text, kw))?;
    main_table(&tokens[pos + 1..])
}

/// 先頭の表名（`ONLY` は読み飛ばす）。直後が括弧の中（副問い合わせ・VALUES）なら主たる表は無い。
fn main_table(tokens: &[Token<'_>]) -> Option<String> {
    let next = tokens
        .iter()
        .find(|t| t.depth > 0 || !is_kw(t.text, "ONLY"))?;
    if next.depth > 0 {
        return None;
    }
    identifier(next.text)
}

/// 識別子（`schema.table`・引用符付き可）なら引用符を外して返す。それ以外（`$1` など）は `None`。
fn identifier(text: &str) -> Option<String> {
    let name: String = text.chars().filter(|c| *c != '"').collect();
    let first = name.chars().next()?;
    let ok = (first.is_ascii_alphabetic() || first == '_')
        && name.len() <= MAX_IDENT_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'))
        && !name.ends_with('.');
    ok.then_some(name)
}

#[cfg(test)]
#[path = "db_spans_tests.rs"]
mod tests;
