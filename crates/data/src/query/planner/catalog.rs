//! 統計表の読み出し（query-optimizer.md §4・O2）。**統計の唯一の読取経路**（PIT-75）。
//!
//! このモジュールは `query::planner` の外から見えない。読んだ統計は計画（戦略の選択・予算・受付制御）に
//! 使って捨て、利用者・API・エラー文言へ返さない。
//!
//! - テーブル統計とフィールド統計は `(tenant_int, table_int)` ごとに短時間（[`TTL`]）プロセス内に置く。
//!   全員共通で権限を表さないので PIT-18 の「材料をキャッシュしない」の対象外。
//! - 値頻度表は計画ごとに、必要な (列, 値) だけを 1 文で引く。行述語の選言（`owner = me` など）の値は
//!   材料由来なのでキャッシュしない（リクエストを跨がない）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use sqlx::types::Json;
use sqlx::PgConnection;

use crate::index::IndexTable;
use crate::model::{FieldId, TableKeys};
use crate::{map_db, DataError};

/// 統計のキャッシュの寿命。
const TTL: Duration = Duration::from_secs(30);
/// キャッシュの最大件数（溢れたら全消去）。
const MAX_ENTRIES: usize = 4_096;

/// 1 列（索引表ごと）の統計。頻度は「値を持つ行」に対する割合。
#[derive(Debug, Clone, Default)]
pub(crate) struct FieldStats {
    pub n_values: f64,
    pub n_entries: f64,
    pub n_distinct: f64,
    pub mcv: Vec<(String, f64)>,
    pub hist: Vec<String>,
    pub truncated_frac: f64,
    pub avg_elems: f64,
    pub correlation: f64,
}

/// リンク統計（方向ごと）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LinkStats {
    pub n_src: f64,
    pub n_edges: f64,
    pub max_fanout: f64,
    pub link_frac: f64,
}

/// テーブルの統計（計画に使う分）。
#[derive(Debug, Clone, Default)]
pub(crate) struct TableStats {
    pub n_rows: f64,
    /// 値頻度表を持つ列（owner 擬似列 0 を含む）。
    pub counts_fields: HashSet<FieldId>,
    pub fields: HashMap<(FieldId, IndexTable), FieldStats>,
    pub links: HashMap<(FieldId, bool), LinkStats>,
}

impl TableStats {
    pub(crate) fn field(&self, f: FieldId, t: IndexTable) -> Option<&FieldStats> {
        self.fields.get(&(f, t))
    }
}

/// キャッシュの中身（`(tenant_int, table_int)` → (読んだ時刻, 統計)）。
type CacheMap = HashMap<(i32, i64), (Instant, Arc<TableStats>)>;

/// リンク統計の行（列, 逆方向か, 参照元の行数, 辺の数, 最大ファンアウト, リンクを持つ行の割合）。
type LinkRow = (i16, bool, i64, i64, i32, f32);

/// プロセス内のキャッシュ。
#[derive(Debug, Default)]
pub(crate) struct StatsCache {
    map: Mutex<CacheMap>,
}

impl StatsCache {
    fn get(&self, k: (i32, i64)) -> Option<Arc<TableStats>> {
        let m = self.map.lock().ok()?;
        let (at, s) = m.get(&k)?;
        (at.elapsed() < TTL).then(|| s.clone())
    }

    fn put(&self, k: (i32, i64), s: Arc<TableStats>) {
        if let Ok(mut m) = self.map.lock() {
            if m.len() >= MAX_ENTRIES {
                m.clear();
            }
            m.insert(k, (Instant::now(), s));
        }
    }

    /// テーブルの統計を落とす（統計収集の直後など）。
    pub(crate) fn invalidate(&self, k: (i32, i64)) {
        if let Ok(mut m) = self.map.lock() {
            m.remove(&k);
        }
    }
}

#[derive(sqlx::FromRow)]
struct FieldRow {
    field_id: i16,
    index_table: i16,
    n_values: i64,
    n_entries: i64,
    n_distinct: i64,
    mcv: Json<Value>,
    histogram: Json<Value>,
    truncated_frac: f32,
    avg_elems: f32,
    correlation: f32,
}

/// テーブルの統計を読む（数え直し前・統計が無ければ `None` = O1 の件数探索だけで選ぶ）。
pub(crate) async fn load(
    conn: &mut PgConnection,
    cache: &StatsCache,
    keys: TableKeys,
) -> Result<Option<Arc<TableStats>>, DataError> {
    let k = (keys.tenant_int, keys.table_int);
    if let Some(s) = cache.get(k) {
        return Ok(Some(s));
    }
    let head: Option<(i64, Vec<i16>, bool)> = sqlx::query_as(
        "SELECT n_rows, counts_fields, counts_ready FROM data_table_stats \
         WHERE tenant_int = $1 AND table_int = $2 AND analyzed_at IS NOT NULL",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    let Some((n_rows, counts_fields, true)) = head else {
        return Ok(None);
    };
    let rows: Vec<FieldRow> = sqlx::query_as(
        "SELECT field_id, index_table, n_values, n_entries, n_distinct, mcv, histogram, \
                truncated_frac, avg_elems, correlation \
         FROM data_field_stats WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    let links: Vec<LinkRow> = sqlx::query_as(
        "SELECT field_id, is_reverse, n_src, n_edges, max_fanout, link_frac \
         FROM data_link_stats WHERE tenant_int = $1 AND table_int = $2",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    #[allow(clippy::cast_precision_loss)]
    let mut s = TableStats {
        n_rows: n_rows.max(0) as f64,
        counts_fields: counts_fields
            .into_iter()
            .filter_map(|f| FieldId::try_from(f).ok())
            .collect(),
        ..Default::default()
    };
    for r in rows {
        let (Ok(f), Some(t)) = (
            FieldId::try_from(r.field_id),
            IndexTable::from_code(r.index_table),
        ) else {
            continue;
        };
        let mcv = r
            .mcv
            .0
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| Some((e.get(0)?.as_str()?.to_string(), e.get(1)?.as_f64()?)))
                    .collect()
            })
            .unwrap_or_default();
        let hist = r
            .histogram
            .0
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        #[allow(clippy::cast_precision_loss)]
        s.fields.insert(
            (f, t),
            FieldStats {
                n_values: r.n_values as f64,
                n_entries: r.n_entries as f64,
                n_distinct: r.n_distinct as f64,
                mcv,
                hist,
                truncated_frac: f64::from(r.truncated_frac),
                avg_elems: f64::from(r.avg_elems),
                correlation: f64::from(r.correlation),
            },
        );
    }
    for (f, rev, n_src, n_edges, max_fanout, frac) in links {
        let Ok(f) = FieldId::try_from(f) else {
            continue;
        };
        #[allow(clippy::cast_precision_loss)]
        s.links.insert(
            (f, rev),
            LinkStats {
                n_src: n_src as f64,
                n_edges: n_edges as f64,
                max_fanout: f64::from(max_fanout),
                link_frac: f64::from(frac),
            },
        );
    }
    let s = Arc::new(s);
    cache.put(k, s.clone());
    Ok(Some(s))
}

/// 値頻度表から (列, 値) の件数を 1 文で引く（値頻度表を持つ列だけ・無い値は 0 件）。
pub(crate) async fn value_counts(
    conn: &mut PgConnection,
    keys: TableKeys,
    wants: &[(FieldId, String)],
) -> Result<HashMap<(FieldId, String), u64>, DataError> {
    if wants.is_empty() {
        return Ok(HashMap::new());
    }
    let f: Vec<i16> = wants
        .iter()
        .map(|(f, _)| i16::try_from(*f).unwrap_or(i16::MAX))
        .collect();
    let v: Vec<String> = wants.iter().map(|(_, v)| v.clone()).collect();
    let rows: Vec<(i16, String, i64)> = sqlx::query_as(
        "SELECT c.field_id, c.val, c.cnt FROM data_value_count c \
         WHERE c.tenant_int = $1 AND c.table_int = $2 \
           AND (c.field_id, c.val) IN (SELECT u.f, u.v COLLATE \"C\" \
                                       FROM unnest($3::int2[], $4::text[]) AS u(f, v))",
    )
    .bind(keys.tenant_int)
    .bind(keys.table_int)
    .bind(&f)
    .bind(&v)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(rows
        .into_iter()
        .filter_map(|(f, v, c)| Some(((FieldId::try_from(f).ok()?, v), c.max(0) as u64)))
        .collect())
}
