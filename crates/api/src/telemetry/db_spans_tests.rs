use std::time::Duration;

use opentelemetry::{
    trace::{SpanKind, TracerProvider as _},
    Value,
};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tracing_subscriber::layer::SubscriberExt;

use super::*;

fn summary(op: &str, table: Option<&str>) -> QuerySummary {
    QuerySummary {
        operation: op.into(),
        collection: table.map(str::to_string),
    }
}

#[test]
fn summarize_extracts_operation_and_main_table() {
    let cases = [
        (
            "SELECT id, name FROM documents WHERE tenant_id = $1",
            summary("SELECT", Some("documents")),
        ),
        (
            "select (SELECT count(*) FROM acl a) AS n FROM public.folders f WHERE f.id = $1",
            summary("SELECT", Some("public.folders")),
        ),
        (
            "INSERT INTO audit_events (id, kind) VALUES ($1, 'login')",
            summary("INSERT", Some("audit_events")),
        ),
        (
            "UPDATE ONLY \"Sessions\" SET seen_at = now() WHERE id = $1",
            summary("UPDATE", Some("Sessions")),
        ),
        (
            "DELETE FROM data_rows WHERE tenant_int = $1",
            summary("DELETE", Some("data_rows")),
        ),
        (
            "WITH t AS (SELECT id FROM jobs WHERE x = $1) UPDATE jobs SET s = 'r' FROM t",
            summary("UPDATE", Some("jobs")),
        ),
        ("BEGIN", summary("BEGIN", None)),
        ("SELECT $1::int + 1", summary("SELECT", None)),
        ("SELECT * FROM (VALUES (1)) AS v", summary("SELECT", None)),
        ("", summary("UNKNOWN", None)),
        ("$1", summary("UNKNOWN", None)),
    ];
    for (sql, want) in cases {
        assert_eq!(summarize(sql), want, "{sql}");
    }
}

#[test]
fn summarize_never_returns_literals_or_comments() {
    // 値（文字列リテラル）・コメントの中の語は表名・操作名に化けない。
    let s = summarize("/* FROM secret */ -- FROM secret2\nSELECT 'FROM secret3' FROM t");
    assert_eq!(s, summary("SELECT", Some("t")));
    // 識別子の形でないもの・長すぎるものは載せない。
    assert_eq!(summarize("SELECT 1 FROM $1").collection, None);
    let long = format!("SELECT 1 FROM {}", "a".repeat(64));
    assert_eq!(summarize(&long).collection, None);
    assert_eq!(summarize("SELECT 1 FROM t.").collection, None);
    // 閉じていないリテラル・コメントでも落ちない。
    assert_eq!(summarize("SELECT 'abc"), summary("SELECT", None));
    assert_eq!(summarize("SELECT /* x"), summary("SELECT", None));
    // 括弧の閉じすぎでも落ちない。
    assert_eq!(summarize("SELECT a) FROM t"), summary("SELECT", Some("t")));
}

#[test]
fn filter_only_enables_sqlx_query_debug() {
    let f = query_events();
    assert!(f.would_enable(SQLX_QUERY_TARGET, &tracing::Level::DEBUG));
    assert!(!f.would_enable(SQLX_QUERY_TARGET, &tracing::Level::TRACE));
    assert!(!f.would_enable("shiki_api", &tracing::Level::ERROR));
}

struct Harness {
    provider: SdkTracerProvider,
    exporter: InMemorySpanExporter,
}

impl Harness {
    fn new() -> Self {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        Self { provider, exporter }
    }

    /// 本番と同じ並び（[`crate::telemetry::otel_layers`]・`RUST_LOG` 未設定の既定フィルタ）の subscriber。
    fn subscriber(&self) -> impl Subscriber + Send + Sync {
        tracing_subscriber::registry()
            .with(crate::telemetry::otel_layers(self.provider.tracer("test")))
    }

    fn spans(&self) -> Vec<SpanData> {
        self.provider.force_flush().unwrap();
        self.exporter.get_finished_spans().unwrap()
    }
}

/// sqlx-core 0.8 の `QueryLogger::finish` と同じ形のイベントを出す。
fn emit_sqlx_like(sql: &str, elapsed: Duration, rows: u64) {
    let summary = sql.split_whitespace().take(4).collect::<Vec<_>>().join(" ");
    let statement = if summary == sql {
        String::new()
    } else {
        format!("\n\n{sql}\n")
    };
    tracing::debug!(
        target: "sqlx::query",
        summary,
        db.statement = statement,
        rows_affected = 0u64,
        rows_returned = rows,
        ?elapsed,
        elapsed_secs = elapsed.as_secs_f64(),
    );
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

#[test]
fn query_event_becomes_child_client_span_without_sql_text() {
    let h = Harness::new();
    let sql = "SELECT id FROM documents WHERE owner = $1 AND title = 'Q3 予算'";
    tracing::subscriber::with_default(h.subscriber(), || {
        let request = tracing::info_span!("request");
        let _e = request.enter();
        emit_sqlx_like(sql, Duration::from_millis(250), 3);
        emit_sqlx_like("BEGIN", Duration::from_millis(1), 0);
    });
    let spans = h.spans();
    let request = spans.iter().find(|s| s.name == "request").unwrap();
    let db = spans
        .iter()
        .find(|s| s.name == "SELECT documents")
        .expect("DB span");
    assert!(spans.iter().any(|s| s.name == "BEGIN"));

    assert_eq!(db.parent_span_id, request.span_context.span_id());
    assert_eq!(db.span_context.trace_id(), request.span_context.trace_id());
    assert_eq!(db.span_kind, SpanKind::Client);
    let took = db.end_time.duration_since(db.start_time).unwrap();
    assert!(
        (Duration::from_millis(249)..Duration::from_millis(260)).contains(&took),
        "{took:?}"
    );
    assert_eq!(attr(db, "db.system.name"), Some(&Value::from("postgresql")));
    assert_eq!(attr(db, "db.operation.name"), Some(&Value::from("SELECT")));
    assert_eq!(
        attr(db, "db.collection.name"),
        Some(&Value::from("documents"))
    );
    assert_eq!(
        attr(db, "db.query.summary"),
        Some(&Value::from("SELECT documents"))
    );
    assert_eq!(attr(db, "db.response.returned_rows"), Some(&Value::I64(3)));
    // SQL 本文・リテラルの値はどの属性にも載らない。
    let mut keys: Vec<_> = db.attributes.iter().map(|kv| kv.key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "db.collection.name",
            "db.operation.name",
            "db.query.summary",
            "db.response.returned_rows",
            "db.system.name",
            "shiki.db.rows_affected",
        ]
    );
    for kv in &db.attributes {
        let v = kv.value.as_str();
        assert!(!v.contains("Q3") && !v.contains("owner"), "{kv:?}");
    }
    // リクエスト span にイベントとして SQL が残ることもない（OTel レイヤは INFO 以上だけ）。
    assert!(request.events.is_empty());
}

#[test]
fn query_without_sampled_parent_is_not_traced() {
    let h = Harness::new();
    tracing::subscriber::with_default(h.subscriber(), || {
        emit_sqlx_like("SELECT 1 FROM t", Duration::from_millis(5), 1);
        // 経過時間の無いイベント（形の違う sqlx）も span にしない。
        let request = tracing::info_span!("request");
        let _e = request.enter();
        tracing::debug!(target: "sqlx::query", summary = "SELECT 1", "no elapsed");
        // 別ターゲットの debug は拾わない。
        tracing::debug!(target: "sqlx::other", elapsed_secs = 1.0f64);
    });
    let spans = h.spans();
    assert_eq!(
        spans.iter().map(|s| s.name.as_ref()).collect::<Vec<_>>(),
        ["request"]
    );
}

#[test]
fn query_fields_accept_debug_and_signed_values() {
    // sqlx の版によって String を `?` で渡す・行数を i64 で渡す形でも拾えること。
    let h = Harness::new();
    tracing::subscriber::with_default(h.subscriber(), || {
        let request = tracing::info_span!("request");
        let _e = request.enter();
        tracing::debug!(
            target: "sqlx::query",
            summary = ?"DELETE FROM jobs",
            rows_affected = 4i64,
            rows_returned = -1i64,
            elapsed_secs = 0.01f64,
        );
        tracing::debug!(target: "sqlx::query", summary = "SELECT 1", elapsed_secs = f64::NAN);
    });
    let spans = h.spans();
    let db = spans.iter().find(|s| s.name == "DELETE jobs").unwrap();
    assert_eq!(attr(db, "shiki.db.rows_affected"), Some(&Value::I64(4)));
    assert_eq!(attr(db, "db.response.returned_rows"), Some(&Value::I64(0)));
    assert_eq!(spans.len(), 2, "NaN の経過時間は span にしない");
}

#[derive(Clone, Default)]
struct Buf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn sql_does_not_reach_log_output() {
    // 本番の並び（fmt＋OTel＋DB span）で、クエリのイベントがログに出ないこと（従来どおり）。
    use tracing_subscriber::Layer as _;
    let h = Harness::new();
    let buf = Buf::default();
    let writer = buf.clone();
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(move || writer.clone())
        .with_filter(crate::telemetry::env_filter());
    let sub = tracing_subscriber::registry()
        .with(fmt)
        .with(crate::telemetry::otel_layers(h.provider.tracer("test")));
    tracing::subscriber::with_default(sub, || {
        let request = tracing::info_span!("request");
        let _e = request.enter();
        emit_sqlx_like(
            "SELECT secret_col FROM t WHERE a = $1",
            Duration::from_millis(3),
            1,
        );
        tracing::info!("visible");
    });
    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(out.contains("visible"), "{out}");
    assert!(!out.contains("secret_col"), "{out}");
    assert!(h.spans().iter().any(|s| s.name == "SELECT t"));
}

/// `init` と同じ subscriber（[`crate::telemetry::subscriber`]）で、OTLP の有無を切り替えて確かめる。
fn init_like(h: &Harness, otlp: bool, buf: &Buf) -> Box<dyn Subscriber + Send + Sync> {
    use tracing_subscriber::Layer as _;
    let writer = buf.clone();
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(move || writer.clone())
        .boxed();
    crate::telemetry::subscriber(fmt, otlp.then(|| h.provider.tracer("test")))
}

/// sqlx のクエリイベントと同じ（ターゲット・レベル）の metadata。
fn sqlx_query_metadata() -> &'static tracing::Metadata<'static> {
    use tracing::callsite::Callsite as _;
    let cs = tracing::callsite! {
        name: "sqlx query",
        kind: tracing::metadata::Kind::EVENT,
        target: "sqlx::query",
        level: tracing::Level::DEBUG,
        fields: []
    };
    cs.metadata()
}

/// sqlx のクエリイベントの callsite を、その subscriber が有効にし得るか。
///
/// `enabled!`・`event!` は callsite の interest が never なら評価もイベント生成もしない（sqlx は
/// SQL の整形もしない）。interest はプロセス全体の subscriber を合わせてキャッシュされるので、
/// `cargo test` で別スレッドの subscriber が混ざらないよう、この subscriber に直接問う。
fn sqlx_query_enabled(dispatch: &tracing::Dispatch) -> bool {
    !dispatch.register_callsite(sqlx_query_metadata()).is_never()
}

#[test]
fn otlp_disabled_keeps_query_events_off_and_makes_no_db_span() {
    let h = Harness::new();
    let buf = Buf::default();
    let off = tracing::Dispatch::new(init_like(&h, false, &buf));
    // sqlx は判定が偽ならイベントを作らない（SQL の整形もしない）＝変更前と同じコスト。
    assert!(!sqlx_query_enabled(&off));
    tracing::dispatcher::with_default(&off, || {
        let request = tracing::info_span!("request");
        let _e = request.enter();
        emit_sqlx_like(
            "SELECT secret_col FROM t WHERE a = $1",
            Duration::from_millis(3),
            1,
        );
        tracing::info!("visible");
    });
    let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        out.contains("visible") && !out.contains("secret_col"),
        "{out}"
    );
    assert!(h.spans().is_empty(), "OTLP 無効では span を作らない");

    // 同じ組み立てで OTLP を有効にすると、クエリイベントが有効になり DB span が出る。
    let h = Harness::new();
    let on = tracing::Dispatch::new(init_like(&h, true, &Buf::default()));
    assert!(sqlx_query_enabled(&on));
    tracing::dispatcher::with_default(&on, || {
        let request = tracing::info_span!("request");
        let _e = request.enter();
        emit_sqlx_like(
            "SELECT secret_col FROM t WHERE a = $1",
            Duration::from_millis(3),
            1,
        );
    });
    assert!(h.spans().iter().any(|s| s.name == "SELECT t"));
}

#[path = "db_spans_bench.rs"]
mod bench;
