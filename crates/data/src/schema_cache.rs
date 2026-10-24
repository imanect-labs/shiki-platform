//! スキーマキャッシュ（Task 15.9・data-platform §16.6）。
//!
//! `data_table.schema` は全クエリで引かれる。スキーマは権限ではないので（PIT-18 の対象外）、
//! プロセス内に置いて JSONB の転送と解釈を省く。**認可材料は載せない**（§16.4。載せるのは
//! [`TableSchema`] だけ）。プランキャッシュ（形状のキャッシュ）とも別物。
//!
//! # キーと世代
//!
//! - 実効キーは **`(tenant_id, table_id, schema_version)`**（＋識別子形式 `key_format`）。
//!   `schema_version` はテーブルごとに 1 から始まるので単独では一意でない（0033 の `default 1`）。
//!   保持は `(tenant_id, table_id)` ごとに最新の 1 世代で、**使うのは DB の行が同じ版を返したときだけ**
//!   （生存・版の照合は毎回 DB の 1 クエリで行い、キャッシュは本文の再取得を省くだけ）。
//!   別レプリカでの改訂も、次のクエリで版が変わって見えるので古い本文は使われない。
//! - 改訂・テーブル削除・テナント purge で**世代（epoch）を進めて該当エントリを落とす**。
//!   読取は開始時に控えた世代と挿入時の世代が同じときだけ挿入する（1 つのロックで比較と挿入を
//!   行う＝切替の前に読んだ古い本文が切替の後に戻ってこない）。削除・purge 後の行は DB の照合で
//!   見つからないので、古い世代が読まれることも無い。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use uuid::Uuid;

use crate::model::TableSchema;

/// 保持するテーブル数の上限（超えたら全消去して作り直す・本文は DB から引き直せる）。
pub(crate) const SCHEMA_CACHE_CAPACITY: usize = 4_096;

#[derive(Clone)]
struct Entry {
    schema_version: i64,
    key_format: i16,
    schema: Arc<TableSchema>,
}

#[derive(Default)]
struct Inner {
    epoch: u64,
    entries: HashMap<(String, Uuid), Entry>,
}

/// スキーマキャッシュ（[モジュール文書](self)）。
#[derive(Default)]
pub(crate) struct SchemaCache {
    inner: Mutex<Inner>,
}

/// 読取の開始時に控えるもの（世代と、手元にある版）。
pub(crate) struct Probe {
    epoch: u64,
    cached: Option<Entry>,
}

impl Probe {
    /// 照合に使う版（無ければ決して一致しない値）。
    pub(crate) fn version(&self) -> (i64, i16) {
        self.cached
            .as_ref()
            .map_or((0, -1), |e| (e.schema_version, e.key_format))
    }

    /// DB の行が同じ版を返したときだけ本文を返す。
    pub(crate) fn hit(&self, schema_version: i64, key_format: i16) -> Option<Arc<TableSchema>> {
        self.cached
            .as_ref()
            .filter(|e| e.schema_version == schema_version && e.key_format == key_format)
            .map(|e| Arc::clone(&e.schema))
    }
}

impl SchemaCache {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn probe(&self, tenant_id: &str, table_id: Uuid) -> Probe {
        let g = self.lock();
        Probe {
            epoch: g.epoch,
            cached: g.entries.get(&(tenant_id.to_string(), table_id)).cloned(),
        }
    }

    /// DB から引いた本文を載せる。`probe` を取ってから世代が進んでいれば載せない。
    pub(crate) fn insert(
        &self,
        probe: &Probe,
        tenant_id: &str,
        table_id: Uuid,
        schema_version: i64,
        key_format: i16,
        schema: Arc<TableSchema>,
    ) {
        let mut g = self.lock();
        if g.epoch != probe.epoch {
            return;
        }
        let key = (tenant_id.to_string(), table_id);
        // 並行する読取が新しい版を先に載せていたら戻さない（版は単調増加）。
        if g.entries
            .get(&key)
            .is_some_and(|e| e.schema_version > schema_version)
        {
            return;
        }
        if g.entries.len() >= SCHEMA_CACHE_CAPACITY && !g.entries.contains_key(&key) {
            g.entries.clear();
        }
        g.entries.insert(
            key,
            Entry {
                schema_version,
                key_format,
                schema,
            },
        );
    }

    /// テーブルの改訂・削除で世代を切り替える。
    pub(crate) fn invalidate_table(&self, tenant_id: &str, table_id: Uuid) {
        let mut g = self.lock();
        g.epoch += 1;
        g.entries.remove(&(tenant_id.to_string(), table_id));
    }

    /// テナント purge で世代を切り替える（そのテナントのエントリを全て落とす）。
    pub(crate) fn invalidate_tenant(&self, tenant_id: &str) {
        let mut g = self.lock();
        g.epoch += 1;
        g.entries.retain(|(t, _), _| t != tenant_id);
    }

    /// 検証用: テナントについて保持している `(table_id, schema_version)`。
    pub(crate) fn cached_versions(&self, tenant_id: &str) -> Vec<(Uuid, i64)> {
        let g = self.lock();
        let mut v: Vec<(Uuid, i64)> = g
            .entries
            .iter()
            .filter(|((t, _), _)| t == tenant_id)
            .map(|((_, id), e)| (*id, e.schema_version))
            .collect();
        v.sort_unstable();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Arc<TableSchema> {
        Arc::new(serde_json::from_value(serde_json::json!({ "fields": [] })).unwrap())
    }

    #[test]
    fn hit_requires_same_version_and_format() {
        let c = SchemaCache::default();
        let t = Uuid::new_v4();
        let p = c.probe("a", t);
        assert!(p.hit(1, 2).is_none());
        c.insert(&p, "a", t, 3, 2, schema());
        let p = c.probe("a", t);
        assert_eq!(p.version(), (3, 2));
        assert!(p.hit(3, 2).is_some());
        assert!(p.hit(4, 2).is_none(), "改訂後の版では使わない");
        assert!(p.hit(3, 1).is_none(), "識別子形式が違えば使わない");
        // 同じ table_id でも別テナントは別エントリ（schema_version は単独で一意でない）。
        assert!(c.probe("b", t).hit(3, 2).is_none());
    }

    #[test]
    fn invalidation_switches_generation_atomically() {
        let c = SchemaCache::default();
        let t = Uuid::new_v4();
        // 削除の前に読み始めた読取は、削除の後に古い本文を載せられない。
        let before = c.probe("a", t);
        c.invalidate_table("a", t);
        c.insert(&before, "a", t, 1, 2, schema());
        assert!(c.cached_versions("a").is_empty());
        // purge も同じ（他テナントは残る）。
        let u = Uuid::new_v4();
        let p = c.probe("a", t);
        c.insert(&p, "a", t, 1, 2, schema());
        c.insert(&c.probe("b", u), "b", u, 1, 2, schema());
        let stale = c.probe("a", t);
        c.invalidate_tenant("a");
        assert!(c.cached_versions("a").is_empty());
        assert_eq!(c.cached_versions("b"), vec![(u, 1)]);
        c.insert(&stale, "a", t, 1, 2, schema());
        assert!(c.cached_versions("a").is_empty());
    }

    #[test]
    fn older_version_does_not_overwrite_newer() {
        let c = SchemaCache::default();
        let t = Uuid::new_v4();
        let p = c.probe("a", t);
        c.insert(&p, "a", t, 5, 2, schema());
        c.insert(&p, "a", t, 4, 2, schema());
        assert_eq!(c.cached_versions("a"), vec![(t, 5)]);
    }
}
