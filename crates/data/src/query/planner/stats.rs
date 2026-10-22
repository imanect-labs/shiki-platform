//! 計画時の上限付き件数探索とその短期キャッシュ（query-optimizer.md §4.3・O1 は統計表を持たない）。
//!
//! **このモジュールは `query::planner` の外から見えない**（PIT-75: 統計への読取経路を planner に閉じる）。
//! 件数は利用者へ返さない（応答・エラー文言・API スキーマのいずれにも出さない）。
//!
//! - クエリ条件 Q の述語の結果は `(table_int, 述語)` をキーに短時間（既定 60 秒）キャッシュする。
//!   全員共通で権限を表さないので、PIT-18 の「材料をキャッシュしない」の対象外。
//! - 行述語 P の選言の探索はキーが principal と認可材料から作られるので**キャッシュしない**。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sqlx::PgConnection;

use super::guard::Driver;
use super::lower::Lowerer;
use crate::model::TableKeys;
use crate::query::bound::Atom;
use crate::query::sql::{bind_scalar, Bind, SqlBuf};
use crate::{map_db, DataError};

/// 件数探索の既定上限。
pub(crate) const PROBE_CAP: i64 = 1_000;
/// 1 計画あたりの件数探索の本数（キャッシュ命中は数えない）。
pub(crate) const PROBES_PER_PLAN: u32 = 3;
/// キャッシュの寿命。
const TTL: Duration = Duration::from_mins(1);
/// キャッシュの最大件数（溢れたら古いものから捨てる代わりに全消去・単純さ優先）。
const MAX_ENTRIES: usize = 10_000;

/// 件数探索の結果（`cap` で打ち切った件数と、その時の上限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Probe {
    pub count: u64,
    pub cap: u64,
}

impl Probe {
    /// 上限内（正確な件数）か。
    pub(crate) fn exact(self) -> Option<u64> {
        (self.count < self.cap).then_some(self.count)
    }
}

/// Q の述語の件数探索キャッシュ（プロセス内）。
#[derive(Debug, Default)]
pub(crate) struct ProbeCache {
    map: Mutex<HashMap<ProbeKey, (Instant, Probe)>>,
}

/// キャッシュのキー（tenant_int, table_int, 述語の正準表記）。
type ProbeKey = (i32, i64, String);

impl ProbeCache {
    fn get(&self, k: &ProbeKey, cap: u64) -> Option<Probe> {
        let m = self.map.lock().ok()?;
        let (at, p) = m.get(k)?;
        // 小さい上限で測った「cap 以上」は大きい上限の問いに答えられない。
        (at.elapsed() < TTL && (p.count < p.cap || p.cap >= cap)).then_some(*p)
    }

    fn put(&self, k: ProbeKey, p: Probe) {
        if let Ok(mut m) = self.map.lock() {
            if m.len() >= MAX_ENTRIES {
                m.clear();
            }
            m.insert(k, (Instant::now(), p));
        }
    }
}

/// 1 計画の件数探索器（本数の上限を数える）。
pub(crate) struct Prober<'a> {
    pub cache: &'a ProbeCache,
    pub keys: TableKeys,
    pub used: u32,
    /// 探索のたびに積む（デバッグ取得用）。
    pub statements: Vec<(String, Vec<Bind>)>,
}

impl Prober<'_> {
    pub(crate) fn remaining(&self) -> u32 {
        PROBES_PER_PLAN.saturating_sub(self.used)
    }

    /// Q の原子式の件数（キャッシュ優先）。本数を使い切っていてキャッシュも無ければ `None`。
    pub(crate) async fn atom(
        &mut self,
        conn: &mut PgConnection,
        low: &Lowerer<'_>,
        a: &Atom,
        cap: i64,
        force: bool,
    ) -> Result<Option<Probe>, DataError> {
        let key = (self.keys.tenant_int, self.keys.table_int, format!("{a:?}"));
        let ucap = cap.max(1) as u64;
        if let Some(p) = self.cache.get(&key, ucap) {
            return Ok(Some(p));
        }
        if !force && self.remaining() == 0 {
            return Ok(None);
        }
        let mut sql = SqlBuf::scoped(self.keys.tenant_int, self.keys.table_int);
        let text = low.probe_sql(a, cap.max(1), &mut sql);
        let n = self.run(conn, text, sql.binds).await?;
        let p = Probe {
            count: n,
            cap: ucap,
        };
        self.cache.put(key, p);
        Ok(Some(p))
    }

    /// P の選言の件数（キャッシュしない・材料由来のキー）。
    pub(crate) async fn driver(
        &mut self,
        conn: &mut PgConnection,
        d: &Driver,
        cap: i64,
    ) -> Result<Option<Probe>, DataError> {
        let ucap = cap.max(1) as u64;
        if let Some(n) = d.known_count() {
            return Ok(Some(Probe {
                count: n.min(ucap),
                cap: ucap,
            }));
        }
        if self.remaining() == 0 {
            return Ok(None);
        }
        let mut sql = SqlBuf::scoped(self.keys.tenant_int, self.keys.table_int);
        let src = d.source_sql(&mut sql);
        let ph = sql.typed(Bind::I64(cap.max(1)));
        let text = format!("SELECT count(*) FROM ({src} LIMIT {ph}) s");
        let n = self.run(conn, text, sql.binds).await?;
        Ok(Some(Probe {
            count: n,
            cap: ucap,
        }))
    }

    async fn run(
        &mut self,
        conn: &mut PgConnection,
        text: String,
        binds: Vec<Bind>,
    ) -> Result<u64, DataError> {
        self.used += 1;
        let n: i64 = bind_scalar(sqlx::query_scalar(&text), &binds)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db)?;
        self.statements.push((text, binds));
        Ok(n.max(0) as u64)
    }
}
