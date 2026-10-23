//! プランキャッシュ（data-platform §16.3・Task 15.10）: クエリ**形状** → 降ろした SQL テキスト。
//!
//! Postgres の parse / plan も sqlx の prepared statement キャッシュも**SQL テキストがキー**なので、
//! 同じ論理クエリでテキストが揺れると全クエリが parse + plan を払う。降下（[`super::lower`]）は
//! 「同一形状なら必ずバイト同一のテキスト」を生成する契約で、値・認可材料・値の長さ・材料の有無は
//! すべてバインド（配列・真偽スイッチ・型つき `NULL`）で渡す。ここはその契約の**台帳**で、
//!
//! - 形状キーごとに、実行した文の種類（ラベル）とテキストを intern する（同じテキストは同じ `Arc<str>`）
//! - 命中・初見の回数を数える（初見が続く形状は契約違反の兆候。運用指標・テストで固定する）
//!
//! **キーに入れるもの**: 条件木の構造（葉の種類・範囲の端の有無と開閉・リンクの半結合の入れ子）・ソートキー・
//! 射影集合・集計・グループ化・結合の形・スキーマの世代（`schema_version`）・コンパイラの版。
//! **入れないもの**: 認可材料（ロール集合・共有 id・principal ID・`HasRole` の成否）・値・`tenant_id` /
//! `table_int`（バインド値）。`HasRole` の成否は真偽スイッチで渡すので SQL テキストを変えない
//! （data-platform §16.3 の「畳み込み結果をキーに入れる」は、テキストを分岐させないことで不要にした）。
//! 戦略（索引の駆動の選び方）は文の種類と同じく実行時に決まり、戦略ごとのテキストが同じ形状の下に並ぶ
//! （query-optimizer.md §7.4「戦略の種別だけをキーに足す」）。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::model::FieldId;
use crate::query::bound::{BoundQuery, Expr, Leaf};
use crate::query::ir::Metric;

/// 降下の契約の版（生成するテキストの形を変えたら上げる。古い形状のテキストと混ぜない）。
pub(crate) const COMPILER_VERSION: u32 = 1;
/// 保持する（形状, 文の種類）の上限（超えたら古いものから捨てる）。
const MAX_ENTRIES: usize = 8_192;
/// 1 つの（形状, 文の種類）に並べてよいテキストの数（戦略・ページ位置の有無・索引の状態で数通りに分かれる）。
const MAX_TEXTS_PER_ENTRY: usize = 16;

/// クエリ形状のキー（正準表記の SHA-256）。
pub(crate) type ShapeKey = [u8; 32];

/// プランキャッシュの運用指標（値・材料・テキストは含まない）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanCacheStats {
    /// 保持している（形状, 文の種類）の数。
    pub entries: u64,
    /// 保持しているテキストの数。
    pub texts: u64,
    /// 既に見たテキストを引いた回数。
    pub hits: u64,
    /// 初めて見たテキストの回数。
    pub misses: u64,
    /// 1 つの（形状, 文の種類）にテキストが並び過ぎて保持しなかった回数（0 であるべき）。
    pub overflows: u64,
}

#[derive(Default)]
struct Inner {
    map: HashMap<(ShapeKey, &'static str), Vec<Arc<str>>>,
    order: VecDeque<(ShapeKey, &'static str)>,
}

/// プロセス内のプランキャッシュ（全員共通・認可材料を持たない）。
#[derive(Default)]
pub(crate) struct PlanCache {
    inner: Mutex<Inner>,
    hits: AtomicU64,
    misses: AtomicU64,
    overflows: AtomicU64,
}

impl std::fmt::Debug for PlanCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanCache").finish_non_exhaustive()
    }
}

impl PlanCache {
    /// テキストを引く（同じテキストを既に持っていればそれを返す）。
    pub(crate) fn intern(&self, shape: &ShapeKey, label: &'static str, text: String) -> Arc<str> {
        let Ok(mut g) = self.inner.lock() else {
            return text.into();
        };
        let key = (*shape, label);
        if let Some(v) = g.map.get(&key) {
            if let Some(t) = v.iter().find(|t| ***t == *text) {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Arc::clone(t);
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let t: Arc<str> = text.into();
        if !g.map.contains_key(&key) {
            if g.order.len() >= MAX_ENTRIES {
                if let Some(old) = g.order.pop_front() {
                    g.map.remove(&old);
                }
            }
            g.order.push_back(key);
        }
        let v = g.map.entry(key).or_default();
        if v.len() < MAX_TEXTS_PER_ENTRY {
            v.push(Arc::clone(&t));
        } else {
            self.overflows.fetch_add(1, Ordering::Relaxed);
        }
        t
    }

    pub(crate) fn stats(&self) -> PlanCacheStats {
        let (entries, texts) = self.inner.lock().map_or((0, 0), |g| {
            (
                g.map.len() as u64,
                g.map.values().map(|v| v.len() as u64).sum(),
            )
        });
        PlanCacheStats {
            entries,
            texts,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            overflows: self.overflows.load(Ordering::Relaxed),
        }
    }

    /// 保持しているテキスト（検証用・材料が混ざっていないことを確かめる）。
    pub(crate) fn texts(&self) -> Vec<(&'static str, String)> {
        self.inner.lock().map_or_else(
            |_| Vec::new(),
            |g| {
                g.map
                    .iter()
                    .flat_map(|((_, l), v)| v.iter().map(move |t| (*l, t.to_string())))
                    .collect()
            },
        )
    }
}

/// 実行側の取っ手（1 リクエスト分の形状キー）。
#[derive(Clone, Copy)]
pub(crate) struct PlanHandle<'a> {
    pub cache: &'a PlanCache,
    pub shape: ShapeKey,
}

impl PlanHandle<'_> {
    pub(crate) fn intern(&self, label: &'static str, text: String) -> Arc<str> {
        self.cache.intern(&self.shape, label, text)
    }
}

/// 形状の正準表記を積む（値は積まない）。
struct Enc(Vec<u8>);

impl Enc {
    fn tag(&mut self, t: u8) {
        self.0.push(t);
    }
    fn u64(&mut self, n: u64) {
        self.0.extend_from_slice(&n.to_le_bytes());
    }
    fn field(&mut self, f: FieldId) {
        self.u64(u64::from(f));
    }
    fn str(&mut self, s: &str) {
        self.u64(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }

    fn expr(&mut self, e: &Expr, q: &BoundQuery) {
        match e {
            Expr::True => self.tag(1),
            Expr::False => self.tag(2),
            Expr::Atom(a) => {
                self.tag(3);
                self.field(a.field);
                match &a.leaf {
                    Leaf::Eq(_) => self.tag(10),
                    Leaf::In(_) => self.tag(11),
                    Leaf::Range { lo, hi } => {
                        self.tag(12);
                        for b in [lo, hi] {
                            self.tag(match b {
                                None => 0,
                                Some((_, false)) => 1,
                                Some((_, true)) => 2,
                            });
                        }
                    }
                    Leaf::StartsWith(_) => self.tag(13),
                    Leaf::Contains(_) => self.tag(14),
                    Leaf::EndsWith(_) => self.tag(15),
                    Leaf::Exists => self.tag(16),
                }
            }
            Expr::Not(x) => {
                self.tag(4);
                self.expr(x, q);
            }
            Expr::And(v) | Expr::Or(v) => {
                self.tag(if matches!(e, Expr::And(_)) { 5 } else { 6 });
                self.u64(v.len() as u64);
                for x in v {
                    self.expr(x, q);
                }
            }
            Expr::Link(l) => {
                self.tag(7);
                self.field(l.field);
                self.u64(l.scope as u64);
                if let Some(s) = q.scopes.get(l.scope) {
                    self.str(&s.table.to_string());
                    self.u64(s.schema_version as u64);
                }
                self.expr(&l.filter, q);
            }
        }
    }
}

/// クエリ形状のキー（テーブル・スキーマ世代・束縛済みクエリの構造）。
pub(crate) fn query_shape(
    table_id: uuid::Uuid,
    schema_version: i64,
    q: &BoundQuery,
    has_cursor: bool,
) -> ShapeKey {
    let mut e = Enc(Vec::with_capacity(256));
    e.u64(u64::from(COMPILER_VERSION));
    e.str(&table_id.to_string());
    e.u64(schema_version as u64);
    e.expr(&q.filter, q);
    e.tag(0xF1);
    for k in &q.sort {
        e.field(k.field);
        e.tag(u8::from(k.desc));
    }
    e.tag(0xF2);
    for g in &q.group_by {
        e.u64(g.src as u64);
        e.field(g.field);
    }
    e.tag(0xF3);
    for (m, f) in &q.metrics {
        e.tag(match m {
            Metric::Count => 1,
            Metric::Sum => 2,
            Metric::Avg => 3,
            Metric::Min => 4,
            Metric::Max => 5,
        });
        match f {
            Some(c) => {
                e.u64(c.src as u64);
                e.field(c.field);
            }
            None => e.u64(u64::MAX),
        }
    }
    e.tag(0xF6);
    for j in &q.joins {
        e.u64(j.from as u64);
        e.field(j.field);
        e.u64(j.scope as u64);
        e.tag(u8::from(j.at_most_one));
        e.expr(&j.filter, q);
        let mut sel: Vec<&String> = j.select.iter().collect();
        sel.sort();
        for k in sel {
            e.str(k);
        }
    }
    e.tag(0xF4);
    if let Some(sel) = &q.select {
        let mut keys: Vec<&String> = sel.iter().collect();
        keys.sort();
        for k in keys {
            e.str(k);
        }
    }
    e.tag(0xF5);
    e.tag(u8::from(q.with_count));
    e.tag(u8::from(has_cursor));
    Sha256::digest(&e.0).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_counts_hits_and_keeps_one_copy() {
        let cache = PlanCache::default();
        let k = [7u8; 32];
        let a = cache.intern(&k, "page", "SELECT $1".into());
        let b = cache.intern(&k, "page", "SELECT $1".into());
        assert!(Arc::ptr_eq(&a, &b));
        let s = cache.stats();
        assert_eq!((s.hits, s.misses, s.texts, s.entries), (1, 1, 1, 1));
        cache.intern(&k, "count", "SELECT $1".into());
        assert_eq!(cache.stats().entries, 2);
        for i in 0..MAX_TEXTS_PER_ENTRY + 2 {
            cache.intern(&[9u8; 32], "x", format!("SELECT {i}"));
        }
        assert_eq!(cache.stats().overflows, 2);
    }
}
