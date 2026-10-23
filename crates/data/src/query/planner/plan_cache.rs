//! プランキャッシュ（data-platform §16.3・Task 15.10）: クエリ**形状** → コンパイル済みの SQL。
//!
//! Postgres の parse / plan も sqlx の prepared statement キャッシュも**SQL テキストがキー**なので、
//! 同じ論理クエリでテキストが揺れると全クエリが parse + plan を払う。降下（[`super::lower`]）は
//! 「同一形状なら必ずバイト同一のテキスト」を生成する契約で、値・認可材料・値の長さ・材料の有無は
//! すべてバインド（配列・真偽スイッチ・型つき `NULL`）で渡す。ここは
//!
//! - **コンパイル済みの文**（[`compile`]）: `(形状, 文の種類, 計画の変種)` → テキストとバインドの種類の並び。
//!   命中したら降下でテキストを組まず（[`SqlBuf::skip_text`]）、同じ降下の走査で**バインドの値だけ**を集める。
//!   計画の変種（[`Variant`]）は実行時に決まる構造（戦略の入口・検査の並び・位置の有無・索引の状態・
//!   認可駆動の入口の形）だけで、値・認可材料は入らない。命中時はバインドの種類の並びを照合し、食い違えば
//!   組み直す（形の取り違えを実行させない）。検証ビルド（`debug_assertions`）では命中のたびに組み直して
//!   テキストとバインドが一致することを確かめる（[`PlanCacheStats::verify_failures`] は 0 であるべき）
//! - **台帳**（[`PlanCache::intern`]）: 形状キーごとに、実行した文の種類（ラベル）とテキストを intern する
//!   （同じテキストは同じ `Arc<str>`）。命中・初見の回数を数える（初見が続く形状は契約違反の兆候）
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

use super::lower::Source;
use crate::model::{FieldId, TableKeys};
use crate::query::bound::{Atom, BoundQuery, Expr, Leaf, Val};
use crate::query::ir::Metric;
use crate::query::sql::{Bind, SqlBuf};
use crate::DataError;

/// 降下の契約の版（生成するテキストの形を変えたら上げる。古い形状のテキストと混ぜない）。
/// 2: 射影刈り込み（本体の取得で必要なキーだけを取る）・比較値の型を形状に入れる（監査 B2-1）。
pub(crate) const COMPILER_VERSION: u32 = 2;
/// 保持する（形状, 文の種類）の上限（超えたら古いものから捨てる）。
const MAX_ENTRIES: usize = 8_192;
/// 1 つの（形状, 文の種類）に並べてよいテキストの数（戦略・ページ位置の有無・索引の状態で数通りに分かれる）。
const MAX_TEXTS_PER_ENTRY: usize = 16;

/// クエリ形状のキー（正準表記の SHA-256）。
pub(crate) type ShapeKey = [u8; 32];
/// 計画の変種のキー（[`Variant`] の SHA-256）。
pub(crate) type VariantKey = [u8; 32];

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
    /// 降下でテキストを組んだ回数（コンパイル済みの文の初見）。
    pub compiled: u64,
    /// コンパイル済みの文に命中し、降下でテキストを組まなかった回数。
    pub reused: u64,
    /// 命中したがバインドの種類の並びが違い、組み直した回数（0 であるべき）。
    pub mismatches: u64,
    /// 検証ビルドで命中を組み直して確かめた回数。
    pub verified: u64,
    /// 検証で命中が組み直しと食い違った回数（0 であるべき・変種のキーの漏れ）。
    pub verify_failures: u64,
}

/// コンパイル済みの文（テキストとバインドの種類の並び）。
struct Compiled {
    text: Arc<str>,
    sig: Vec<u8>,
}

/// コンパイル済みの文のキー。
type CompiledKey = (ShapeKey, &'static str, VariantKey);

#[derive(Default)]
struct Inner {
    map: HashMap<(ShapeKey, &'static str), Vec<Arc<str>>>,
    order: VecDeque<(ShapeKey, &'static str)>,
    compiled: HashMap<CompiledKey, Compiled>,
    corder: VecDeque<CompiledKey>,
}

/// プロセス内のプランキャッシュ（全員共通・認可材料を持たない）。
pub(crate) struct PlanCache {
    inner: Mutex<Inner>,
    hits: AtomicU64,
    misses: AtomicU64,
    overflows: AtomicU64,
    compiled: AtomicU64,
    reused: AtomicU64,
    mismatches: AtomicU64,
    verified: AtomicU64,
    verify_failures: AtomicU64,
    /// 命中のたびに組み直して一致を確かめる（既定は検証ビルドだけ）。
    verify: std::sync::atomic::AtomicBool,
}

impl Default for PlanCache {
    fn default() -> Self {
        PlanCache {
            inner: Mutex::default(),
            hits: AtomicU64::default(),
            misses: AtomicU64::default(),
            overflows: AtomicU64::default(),
            compiled: AtomicU64::default(),
            reused: AtomicU64::default(),
            mismatches: AtomicU64::default(),
            verified: AtomicU64::default(),
            verify_failures: AtomicU64::default(),
            verify: std::sync::atomic::AtomicBool::new(cfg!(debug_assertions)),
        }
    }
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
            compiled: self.compiled.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
            mismatches: self.mismatches.load(Ordering::Relaxed),
            verified: self.verified.load(Ordering::Relaxed),
            verify_failures: self.verify_failures.load(Ordering::Relaxed),
        }
    }

    /// 命中のたびに組み直して確かめるか（計測で検証の分を除くため）。
    pub(crate) fn set_verify(&self, on: bool) {
        self.verify.store(on, Ordering::Relaxed);
    }

    fn lookup(&self, key: &CompiledKey) -> Option<(Arc<str>, Vec<u8>)> {
        let g = self.inner.lock().ok()?;
        g.compiled
            .get(key)
            .map(|c| (Arc::clone(&c.text), c.sig.clone()))
    }

    fn store(&self, key: CompiledKey, text: Arc<str>, sig: Vec<u8>) {
        let Ok(mut g) = self.inner.lock() else {
            return;
        };
        if !g.compiled.contains_key(&key) {
            if g.corder.len() >= MAX_ENTRIES {
                if let Some(old) = g.corder.pop_front() {
                    g.compiled.remove(&old);
                }
            }
            g.corder.push_back(key);
        }
        g.compiled.insert(key, Compiled { text, sig });
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

/// バインドの種類の並び（値を含まない）。
fn signature(binds: &[Bind]) -> Vec<u8> {
    binds.iter().map(Bind::kind_tag).collect()
}

/// 1 文をコンパイルする（命中ならテキストを組まずにバインドだけを集める）。
///
/// `build` は同じ文を組む降下（テキストを返す）。`plan` か `variant` が無ければ毎回組む（台帳だけに載せる）。
pub(crate) fn compile(
    plan: Option<&PlanHandle<'_>>,
    label: &'static str,
    variant: Option<&VariantKey>,
    keys: TableKeys,
    build: impl Fn(&mut SqlBuf) -> Result<String, DataError>,
) -> Result<(Arc<str>, Vec<Bind>), DataError> {
    let full = || -> Result<(String, Vec<Bind>), DataError> {
        let mut sql = SqlBuf::scoped(keys.tenant_int, keys.table_int);
        let text = build(&mut sql)?;
        Ok((text, sql.binds))
    };
    let (Some(p), Some(v)) = (plan, variant) else {
        let (text, binds) = full()?;
        let text = match plan {
            Some(p) => p.intern(label, text),
            None => text.into(),
        };
        return Ok((text, binds));
    };
    let c = p.cache;
    let key = (p.shape, label, *v);
    if let Some((text, sig)) = c.lookup(&key) {
        let mut sql = SqlBuf::scoped_binds_only(keys.tenant_int, keys.table_int);
        build(&mut sql)?;
        if signature(&sql.binds) == sig {
            if c.verify.load(Ordering::Relaxed) {
                c.verified.fetch_add(1, Ordering::Relaxed);
                let (fresh, binds) = full()?;
                if *fresh != *text || binds != sql.binds {
                    c.verify_failures.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(
                        label,
                        "プランキャッシュの命中が降下と一致しない（計画の変種のキーの漏れ）"
                    );
                    debug_assert!(
                        false,
                        "プランキャッシュの命中が降下と一致しない: {label}\n命中: {text}\n降下: {fresh}"
                    );
                    let fresh = p.intern(label, fresh);
                    c.store(key, Arc::clone(&fresh), signature(&binds));
                    return Ok((fresh, binds));
                }
            }
            c.reused.fetch_add(1, Ordering::Relaxed);
            c.hits.fetch_add(1, Ordering::Relaxed);
            return Ok((text, sql.binds));
        }
        // 同じ変種でバインドの形が違う（変種のキーの漏れ）: 組み直して置き換える。
        c.mismatches.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            label,
            "プランキャッシュの命中でバインドの形が違うため組み直す"
        );
    }
    let (text, binds) = full()?;
    c.compiled.fetch_add(1, Ordering::Relaxed);
    let text = p.intern(label, text);
    c.store(key, Arc::clone(&text), signature(&binds));
    Ok((text, binds))
}

/// 1 リクエストの文脈キー: 降下のテキストを変え得る実行時の状態のうち、文ごとの変種に現れないもの
/// （主表と参照先の索引の状態・複合索引の有無と型）。値・認可材料は積まない。認可駆動の入口の形
/// （`HasRole` の畳み込みの結果・data-platform §16.3）は入口を使う文の変種だけが積む（[`Variant::source_with`]）。
pub(crate) fn context_key(
    guard: &super::guard::Guard<'_>,
    scopes: &[super::lower::ScopeLow<'_>],
    composite: Option<&super::exec::CompositeSet>,
) -> [u8; 32] {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(&COMPILER_VERSION.to_le_bytes());
    guard.shape_into(&mut out);
    out.extend_from_slice(&(scopes.len() as u64).to_le_bytes());
    for s in scopes {
        s.guard.shape_into(&mut out);
    }
    match composite {
        None => out.push(0),
        Some((_, kinds)) => {
            out.push(1);
            for (k, desc) in kinds {
                out.push(*k as u8);
                out.push(u8::from(*desc));
            }
        }
    }
    Sha256::digest(&out).into()
}

/// 計画の変種（実行時に決まる構造だけを積む・値と認可材料は積まない）。
pub(crate) struct Variant(Enc);

impl Variant {
    /// 1 リクエストの文脈（索引の状態・認可駆動の入口の形など・[`super::exec::Exec::ctx_key`]）から始める。
    pub(crate) fn new(ctx: &[u8; 32]) -> Self {
        let mut e = Enc(Vec::with_capacity(128));
        e.0.extend_from_slice(ctx);
        Variant(e)
    }

    pub(crate) fn tag(mut self, t: u8) -> Self {
        self.0.tag(t);
        self
    }

    pub(crate) fn flag(mut self, b: bool) -> Self {
        self.0.tag(u8::from(b));
        self
    }

    pub(crate) fn num(mut self, n: u64) -> Self {
        self.0.u64(n);
        self
    }

    /// 検査の並び（順序どおり）。
    pub(crate) fn checks(mut self, checks: &[&Expr], q: &BoundQuery) -> Self {
        self.0.u64(checks.len() as u64);
        for c in checks {
            self.0.expr(c, q);
        }
        self
    }

    /// 候補集合の入口。
    /// 候補集合の入口（認可駆動なら行述語の選言の形も積む）。
    pub(crate) fn source_with(self, s: &Source<'_>, guard: &super::guard::Guard<'_>) -> Self {
        let mut v = self.source(s);
        if matches!(s, Source::Policy(_)) {
            guard.drivers_into(&mut v.0 .0);
        }
        v
    }

    pub(crate) fn source(mut self, s: &Source<'_>) -> Self {
        match s {
            Source::Atom(a) => {
                self.0.tag(1);
                self.0.atom(a);
            }
            Source::Union(v) => {
                self.0.tag(2);
                self.0.u64(v.len() as u64);
                for a in v {
                    self.0.atom(a);
                }
            }
            // 認可駆動の入口の形は [`Self::source_with`] が行述語の選言の形として積む。
            Source::Policy(parts) => {
                self.0.tag(3);
                self.0.u64(parts.len() as u64);
            }
            Source::Window { key, lo, hi } => {
                self.0.tag(4);
                self.0.field(key.field);
                self.0.tag(u8::from(key.desc));
                for b in [lo, hi] {
                    self.0.tag(match b {
                        None => 0,
                        Some((_, false)) => 1,
                        Some((_, true)) => 2,
                    });
                }
            }
            Source::All => self.0.tag(5),
            Source::Intersect(a, b) => {
                self.0.tag(6);
                self.0.atom(a);
                self.0.atom(b);
            }
            Source::Ids(_) => self.0.tag(7),
        }
        self
    }

    /// 行述語の選言の入口の形（種類・列だけ・材料は積まない）。
    pub(crate) fn driver(mut self, d: &super::guard::Driver) -> Self {
        d.shape_into(&mut self.0 .0);
        self
    }

    pub(crate) fn finish(self) -> VariantKey {
        Sha256::digest(&self.0 .0).into()
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

    /// 原子式の構造（フィールド・葉の種類・範囲の端の有無と開閉・比較値の型。値は積まない）。
    fn atom(&mut self, a: &Atom) {
        fn val_tag(v: &Val) -> u8 {
            match v {
                Val::Text(_) => 1,
                Val::Num(_) => 2,
                Val::Ref(_) => 3,
            }
        }
        self.field(a.field);
        match &a.leaf {
            Leaf::Eq(v) => {
                self.tag(10);
                self.tag(val_tag(v));
            }
            Leaf::In(_) => self.tag(11),
            Leaf::Range { lo, hi } => {
                self.tag(12);
                for b in [lo, hi] {
                    match b {
                        None => self.tag(0),
                        Some((v, inc)) => {
                            self.tag(1 + u8::from(*inc));
                            self.tag(val_tag(v));
                        }
                    }
                }
            }
            Leaf::StartsWith(_) => self.tag(13),
            Leaf::Contains(_) => self.tag(14),
            Leaf::EndsWith(_) => self.tag(15),
            Leaf::Exists => self.tag(16),
        }
    }

    fn expr(&mut self, e: &Expr, q: &BoundQuery) {
        match e {
            Expr::True => self.tag(1),
            Expr::False => self.tag(2),
            Expr::Atom(a) => {
                self.tag(3);
                self.atom(a);
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
