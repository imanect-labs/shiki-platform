//! テスト用: `tracing` の span を名前・属性・親・イベントごと捕まえる最小のレイヤ。
//!
//! 結合テスト（`common::capture`）と本体の単体テスト（`telemetry.rs` が `#[path]` で読む）の
//! 両方から使う。

#![allow(
    dead_code,
    unreachable_pub,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic
)]

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// 捕まえた span 1 件。
#[derive(Debug, Clone, Default)]
pub struct SpanRec {
    pub name: String,
    pub parent: Option<String>,
    pub fields: HashMap<String, String>,
    pub events: Vec<String>,
    pub closed: bool,
}

impl SpanRec {
    /// 属性の値（無ければ panic・テストの読みやすさ優先）。
    pub fn field(&self, key: &str) -> &str {
        self.fields
            .get(key)
            .unwrap_or_else(|| panic!("{} に属性 {key} が無い: {:?}", self.name, self.fields))
    }
}

#[derive(Default)]
struct State {
    spans: Vec<SpanRec>,
    by_id: HashMap<u64, usize>,
}

/// 捕まえた span の束。
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<State>>);

impl Captured {
    /// このスレッドの既定 subscriber にする（async テストは current-thread ランタイムで使う）。
    pub fn guard(&self) -> tracing::subscriber::DefaultGuard {
        let sub = tracing_subscriber::registry().with(CaptureLayer(self.clone()));
        tracing::subscriber::set_default(sub)
    }

    pub fn run(&self, f: impl FnOnce()) {
        let _g = self.guard();
        f();
    }

    /// 名前で最初の span を引く（無ければ panic）。
    pub fn span(&self, name: &str) -> SpanRec {
        self.find(name)
            .unwrap_or_else(|| panic!("span {name} が無い: {:?}", self.names()))
    }

    pub fn find(&self, name: &str) -> Option<SpanRec> {
        let st = self.0.lock().unwrap();
        st.spans.iter().find(|s| s.name == name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        let st = self.0.lock().unwrap();
        st.spans.iter().map(|s| s.name.clone()).collect()
    }

    /// 捕まえた全 span の全属性値（本文が載っていないかの確認用）。
    pub fn all_values(&self) -> Vec<String> {
        let st = self.0.lock().unwrap();
        st.spans
            .iter()
            .flat_map(|s| s.fields.values().cloned().chain(s.events.clone()))
            .collect()
    }
}

struct CaptureLayer(Captured);

struct Visitor<'a>(&'a mut HashMap<String, String>);

impl Visit for Visitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.0
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut rec = SpanRec {
            name: attrs.metadata().name().to_string(),
            parent: ctx
                .span(id)
                .and_then(|s| s.parent())
                .map(|p| p.name().to_string()),
            ..SpanRec::default()
        };
        attrs.record(&mut Visitor(&mut rec.fields));
        let mut st = (self.0).0.lock().unwrap();
        st.spans.push(rec);
        let idx = st.spans.len() - 1;
        st.by_id.insert(id.into_u64(), idx);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut st = (self.0).0.lock().unwrap();
        if let Some(&idx) = st.by_id.get(&id.into_u64()) {
            values.record(&mut Visitor(&mut st.spans[idx].fields));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.event_span(event) else {
            return;
        };
        let mut fields = HashMap::new();
        event.record(&mut Visitor(&mut fields));
        let mut st = (self.0).0.lock().unwrap();
        if let Some(&idx) = st.by_id.get(&span.id().into_u64()) {
            let msg = fields.remove("message").unwrap_or_default();
            st.spans[idx].events.push(msg);
        }
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        let mut st = (self.0).0.lock().unwrap();
        if let Some(idx) = st.by_id.remove(&id.into_u64()) {
            st.spans[idx].closed = true;
        }
    }
}
