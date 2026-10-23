//! 統計から合成するベンチデータ（query-optimizer.md §9.3・Task 15.12）。
//!
//! 入力は [`data::TableShape`]（行数・列の型・種類数・頻度の形（Zipf の指数と最頻値の頻度）・値なしの率・
//! 作成順との相関・多値の要素数）だけで、**値を含まない**。値は `c{列}v{順位}` のような記号で作る。
//! 書込経路を通さず、本体と索引行を SQL で一括生成する（[`super::bulk`] と同じ正規化）。
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]

use authz::AuthContext;
use data::{ColumnShape, DataStore, FieldType, PolicyExpr, RowPolicy, ShapeType, TableShape};
use serde_json::{json, Map, Value};
use sqlx::PgPool;

use super::{create_table, field, keys_of, schema};

/// 決定的な擬似乱数（xorshift）。
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// [0, 1)。
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// 列の値の分布（Zipf の累積分布・順位 0 が最頻）。
pub struct Dist {
    cdf: Vec<f64>,
}

impl Dist {
    /// 種類数と頻度の形から。最頻値の頻度があればその通りに、残りを Zipf(s) で埋める。
    pub fn from_shape(c: &ColumnShape) -> Dist {
        let d = c.distinct.clamp(1, 20_000) as usize;
        let mut w: Vec<f64> = (0..d)
            .map(|r| 1.0 / ((r + 1) as f64).powf(c.zipf_s.max(0.0)))
            .collect();
        let total: f64 = w.iter().sum();
        w.iter_mut().for_each(|x| *x /= total);
        // 最頻値の頻度（観測した形）で上書きし、残りの重みを按分し直す。
        let top: Vec<f64> = c.top_freqs.iter().take(d).copied().collect();
        if !top.is_empty() {
            let fixed: f64 = top.iter().sum::<f64>().min(1.0);
            let rest: f64 = w[top.len()..].iter().sum();
            for (i, f) in top.iter().enumerate() {
                w[i] = *f;
            }
            if rest > 0.0 {
                for x in &mut w[top.len()..] {
                    *x *= (1.0 - fixed).max(0.0) / rest;
                }
            }
        }
        let mut acc = 0.0;
        let cdf = w
            .iter()
            .map(|x| {
                acc += x;
                acc
            })
            .collect();
        Dist { cdf }
    }

    pub fn len(&self) -> usize {
        self.cdf.len()
    }

    /// 一様乱数 u ∈ [0, 1) の位置の順位。
    pub fn rank(&self, u: f64) -> usize {
        let total = self.cdf.last().copied().unwrap_or(1.0);
        let x = u * total;
        self.cdf
            .partition_point(|c| *c <= x)
            .min(self.cdf.len() - 1)
    }
}

/// 値の名前（順位 → 文字列）。合成ベンチは記号、元のテーブル（顧客データの代わり）は業務的な値。
pub type Namer<'a> = &'a dyn Fn(usize, usize) -> String;

/// 合成ベンチの値の名前（`c{列}v{順位}`）。
pub fn symbolic(col: usize, rank: usize) -> String {
    format!("c{col}v{rank}")
}

fn shape_type(c: &ColumnShape) -> FieldType {
    match c.ty {
        // 選択肢の上限に掛からないよう分類は user_ref（text の値索引・値頻度表の対象）。
        ShapeType::Category => FieldType::UserRef,
        ShapeType::MultiCategory => FieldType::MultiSelect,
        ShapeType::Number => FieldType::Number,
        ShapeType::Text => FieldType::Text,
    }
}

/// 形からテーブルのスキーマ（列は `c{番号}`・全列索引済み・`policy` なら作成者だけが見える）。
pub fn schema_of(shape: &TableShape, policy: bool, namer: Namer<'_>) -> data::TableSchema {
    let mut fields = Vec::new();
    for (i, c) in shape.columns.iter().enumerate() {
        let mut f = field(&format!("c{i}"), shape_type(c));
        f.indexed = true;
        if c.ty == ShapeType::MultiCategory {
            f.options = (0..c.distinct.clamp(1, 200) as usize)
                .map(|r| namer(i, r))
                .collect();
        }
        fields.push(f);
    }
    let mut s = schema(fields);
    if policy {
        s.row_policy = Some(RowPolicy {
            read: PolicyExpr::IsOwner,
            write: None,
        });
    }
    s
}

/// 1 列の値（値なしは `None`）。`pos` は作成順の位置（0〜1）。
fn value(
    c: &ColumnShape,
    i: usize,
    dist: &Dist,
    r: &mut Rng,
    pos: f64,
    namer: Namer<'_>,
) -> Option<Value> {
    if r.unit() < c.null_rate {
        return None;
    }
    // 作成順との相関: |ρ| の確率で位置から順位を決める（ρ > 0 なら新しい行ほど順位が後ろ）。
    let rho = c.creation_correlation.clamp(-1.0, 1.0);
    let u = if r.unit() < rho.abs() {
        if rho > 0.0 {
            pos
        } else {
            1.0 - pos
        }
    } else {
        r.unit()
    };
    match c.ty {
        ShapeType::Number => Some(json!(dist.rank(u) as i64 * 10 - 50)),
        ShapeType::MultiCategory => {
            let n = (c.avg_elems.max(1.0) + r.unit() - 0.5).round().max(1.0) as usize;
            let mut v: Vec<String> = (0..n)
                .map(|k| namer(i, dist.rank(if k == 0 { u } else { r.unit() }).min(199)))
                .collect();
            v.sort();
            v.dedup();
            Some(json!(v))
        }
        _ => Some(json!(namer(i, dist.rank(u)))),
    }
}

/// 形から `n` 行を合成して投入する（本体＋擬似列と各列の索引行）。`owner` は作成者の分布
/// （形に owner があればそれ・無ければ全行 `alice`）。
#[allow(clippy::too_many_arguments)]
pub async fn generate(
    pool: &PgPool,
    store: &DataStore,
    c: &AuthContext,
    name: &str,
    shape: &TableShape,
    n: i64,
    policy: bool,
    seed: u64,
    namer: Namer<'_>,
) -> data::DataTable {
    let table = create_table(store, c, name, schema_of(shape, policy, namer)).await;
    let (ti, tb) = keys_of(pool, &c.tenant_id, table.id).await;
    let dists: Vec<Dist> = shape.columns.iter().map(Dist::from_shape).collect();
    let owner_dist = shape.owner.as_ref().map(Dist::from_shape);
    let ids: Vec<i16> = (0..shape.columns.len())
        .map(|i| i16::try_from(table.schema.field(&format!("c{i}")).unwrap().id).unwrap())
        .collect();
    let mut r = Rng(seed | 1);
    let batch = 10_000i64;
    let mut g = 0i64;
    while g < n {
        let m = batch.min(n - g);
        let mut owners = Vec::with_capacity(m as usize);
        let mut datas = Vec::with_capacity(m as usize);
        let mut seqs = Vec::with_capacity(m as usize);
        for k in 0..m {
            let row = g + k;
            let pos = row as f64 / n.max(1) as f64;
            let mut obj = Map::new();
            for (i, col) in shape.columns.iter().enumerate() {
                if let Some(v) = value(col, i, &dists[i], &mut r, pos, namer) {
                    obj.insert(format!("f{}", ids[i]), v);
                }
            }
            let owner = match (&shape.owner, &owner_dist) {
                (Some(oc), Some(d)) => {
                    let u = if r.unit() < oc.creation_correlation.abs() {
                        pos
                    } else {
                        r.unit()
                    };
                    format!("o{}", d.rank(u))
                }
                _ => "alice".to_string(),
            };
            owners.push(owner);
            datas.push(Value::Object(obj));
            seqs.push(row);
        }
        sqlx::query(
            "INSERT INTO data_record (id, table_id, table_int, record_int, created_at, updated_at, \
               tenant_int, tenant_id, org, owner, data) \
             SELECT gen_random_uuid(), $1, $2, nextval('data_record_int_seq'), \
                    timestamptz '2026-01-01' + u.g * interval '1 millisecond', \
                    timestamptz '2026-01-01' + u.g * interval '1 millisecond', \
                    $3, $4, 'acme', u.owner, u.data \
             FROM unnest($5::text[], $6::jsonb[], $7::int8[]) AS u(owner, data, g) ORDER BY u.g",
        )
        .bind(table.id)
        .bind(tb)
        .bind(ti)
        .bind(&c.tenant_id)
        .bind(&owners)
        .bind(&datas)
        .bind(&seqs)
        .execute(pool)
        .await
        .unwrap();
        g += m;
    }
    let iso = "to_char(%s AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')";
    let pseudo = [
        (0i16, "owner".to_string()),
        (1, iso.replace("%s", "created_at")),
        (2, iso.replace("%s", "updated_at")),
    ];
    for (f, expr) in pseudo {
        sqlx::query(&format!(
            "INSERT INTO data_index_text (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
             SELECT tenant_int, table_int, $3, record_int, 0, {expr}, false FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2"
        ))
        .bind(ti)
        .bind(tb)
        .bind(f)
        .execute(pool)
        .await
        .unwrap();
    }
    for (i, col) in shape.columns.iter().enumerate() {
        let f = ids[i];
        let sql = match col.ty {
            ShapeType::Number => {
                "INSERT INTO data_index_num (tenant_int, table_int, field_id, record_int, ord, val) \
                 SELECT tenant_int, table_int, $3, record_int, 0, (data ->> ('f' || $3::int))::numeric \
                 FROM data_record WHERE tenant_int = $1 AND table_int = $2 AND data ? ('f' || $3::int)"
            }
            ShapeType::MultiCategory => {
                "INSERT INTO data_index_text (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
                 SELECT r.tenant_int, r.table_int, $3, r.record_int, (e.o - 1)::int2, e.v, false \
                 FROM data_record r, jsonb_array_elements_text(r.data -> ('f' || $3::int)) \
                   WITH ORDINALITY AS e(v, o) \
                 WHERE r.tenant_int = $1 AND r.table_int = $2 AND r.data ? ('f' || $3::int)"
            }
            _ => {
                "INSERT INTO data_index_text (tenant_int, table_int, field_id, record_int, ord, val, truncated) \
                 SELECT tenant_int, table_int, $3, record_int, 0, data ->> ('f' || $3::int), false \
                 FROM data_record WHERE tenant_int = $1 AND table_int = $2 AND data ? ('f' || $3::int)"
            }
        };
        sqlx::query(sql)
            .bind(ti)
            .bind(tb)
            .bind(f)
            .execute(pool)
            .await
            .unwrap();
    }
    for t in ["data_record", "data_index_text", "data_index_num"] {
        sqlx::query(&format!("ANALYZE {t}"))
            .execute(pool)
            .await
            .unwrap();
    }
    table
}
