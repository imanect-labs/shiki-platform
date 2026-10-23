//! 結合を残した行の読取（Task 15.10）: FROM の行のページ → 結合の組 → 結合先の本体 → 事後検査 → 組み立て。
//!
//! - FROM の行のページは通常の計画（全戦略・予算付き切替・`Guard(P)`）で取る。条件には結合先の存在
//!   （結合先の行述語つきの半結合）が入っているので、組を 1 つも作れない行はページに来ない。
//! - 組は [`super::planner::join`] が `data_link` を辿り、結合先の生存・条件・行述語を通った行だけで作る。
//! - **二段 authz**: FROM の行と同じく、結合先の行も本体を取ったあと Rust で行述語を独立に再評価し
//!   （[`super::planner::verify::post_filter`]）、落ちた行を含む組は返さない。結合先のフィールドマスクは
//!   結合先の表の閲覧者本人の判定で掛ける。
//! - ページングは「FROM の行の位置 ＋ その行で返した組の数」。1 行が複数の組に広がるので、行の途中で
//!   ページが切れたら、カーソルに直前の行の位置と返した組の数を入れて続きから返す。

use std::collections::{HashMap, HashSet};

use authz::AuthContext;
use chrono::SecondsFormat;
use serde_json::Value;
use uuid::Uuid;

use super::bound::BoundQuery;
use super::planner::exec::{Exec, PageOut};
use super::planner::keys::{from_payload, to_payload, Pos};
use super::planner::lower::fetch_sql;
use super::planner::verify::{post_filter, PolicyEnv, RowView};
use super::scopes::ScopeTables;
use super::sql::SqlBuf;
use crate::cursor::{CursorPayload, JoinCursor};
use crate::model::{DataRecord, TableKeys};
use crate::record::RecordRow;
use crate::store::DataStore;
use crate::{map_db, DataError};

/// 本体取得の行（`record_int` は crate 内に閉じる）。
#[derive(sqlx::FromRow)]
pub(crate) struct JoinFetched {
    record_int: i64,
    #[sqlx(flatten)]
    row: RecordRow,
}

/// 結合を残した行（トランザクションの中で集めたもの）。
#[derive(Default)]
pub(crate) struct JoinedOut {
    /// 返す組（各組は `r0..rN` の `record_int`）。
    pub tuples: Vec<Vec<i64>>,
    /// 結合 `j` の `record_int` → 結合先のレコード（事後検査を通ったもの・マスク前）。
    pub children: Vec<HashMap<i64, DataRecord>>,
    /// FROM の行の `record_int`（本体取得の順＝返す items の順）。
    pub base_ints: Vec<i64>,
}

/// 結合を残した行のページ（FROM の行の `record_int` の順と、組と、次のカーソル）。
pub(crate) struct JoinedPage {
    pub base: Vec<i64>,
    pub tuples: Vec<Vec<i64>>,
    pub next: Option<CursorPayload>,
}

/// FROM の行の位置と「先頭の行（`record_int`）で飛ばす組の数」。
pub(crate) type JoinPosition = (Option<Pos>, Option<(i64, u32)>);

/// カーソルの中身から、FROM の行の位置と「先頭の行で飛ばす組」を取り出す。
pub(crate) fn join_position(
    bq: &BoundQuery,
    p: Option<&CursorPayload>,
) -> Result<JoinPosition, DataError> {
    let Some(p) = p else {
        return Ok((None, None));
    };
    match &p.join {
        None => Ok((Some(from_payload(bq, p)?), None)),
        Some(j) => {
            let base = if j.has_base {
                Some(from_payload(bq, p)?)
            } else {
                None
            };
            Ok((base, Some((j.rint, j.skip))))
        }
    }
}

impl Exec<'_, '_> {
    /// FROM の行のページを取り、組を作って `limit` 件で切る（行の途中で切れたら続きのカーソル）。
    pub(crate) async fn joined_page(
        &mut self,
        page: PageOut,
        after: Option<&Pos>,
        resume: Option<(i64, u32)>,
        limit: i64,
    ) -> Result<JoinedPage, DataError> {
        let ids: Vec<i64> = page.rows.iter().map(|r| r.0).collect();
        let skip = resume
            .filter(|(r, _)| ids.first() == Some(r))
            .map_or(0usize, |(_, s)| s as usize);
        let lim = usize::try_from(limit.max(0)).unwrap_or(0);
        let cap = i64::try_from(skip + lim + 1).unwrap_or(i64::MAX);
        let tuples = self.expand(&ids, cap).await?;
        self.stats.scanned += tuples.len() as u64;
        let mut out: Vec<Vec<i64>> = Vec::with_capacity(lim);
        let mut skipped = 0usize;
        let mut overflow: Option<&Vec<i64>> = None;
        for t in &tuples {
            if skipped < skip && t.first() == ids.first() {
                skipped += 1;
                continue;
            }
            if out.len() == lim {
                overflow = Some(t);
                break;
            }
            out.push(t.clone());
        }
        let next = match (out.last(), overflow) {
            (Some(last), Some(nx)) if nx.first() == last.first() => {
                // 行の途中で切れた: 直前の行の位置と、この行で返した組の数。
                let r = last.first().copied().unwrap_or(0);
                let i = ids.iter().position(|x| *x == r).unwrap_or(0);
                let before = if i > 0 {
                    page.rows.get(i - 1).map(|x| x.1.clone())
                } else {
                    after.cloned()
                };
                let mut done = out.iter().filter(|t| t.first() == Some(&r)).count();
                if i == 0 {
                    done += skip;
                }
                let mut p = before.as_ref().map_or_else(empty_payload, to_payload);
                p.join = Some(JoinCursor {
                    has_base: before.is_some(),
                    rint: r,
                    skip: u32::try_from(done).unwrap_or(u32::MAX),
                });
                Some(p)
            }
            (Some(last), Some(_)) => {
                // 行の切れ目で切れた: 最後に返した行の位置から。
                let r = last.first().copied().unwrap_or(0);
                page.rows
                    .iter()
                    .find(|x| x.0 == r)
                    .map(|x| to_payload(&x.1))
            }
            _ => page.next.as_ref().map(to_payload),
        };
        let mut base = Vec::new();
        let mut seen = HashSet::new();
        for t in &out {
            if let Some(r) = t.first() {
                if seen.insert(*r) {
                    base.push(*r);
                }
            }
        }
        Ok(JoinedPage {
            base,
            tuples: out,
            next,
        })
    }

    /// 結合先の本体を取り、行述語を Rust で独立に再評価する（落ちた行を含む組は返さない）。
    pub(crate) async fn joined_children(
        &mut self,
        ctx: &AuthContext,
        bq: &BoundQuery,
        tables: &ScopeTables,
        tuples: &[Vec<i64>],
    ) -> Result<Vec<HashMap<i64, DataRecord>>, DataError> {
        let mut out = Vec::with_capacity(bq.joins.len());
        for (i, j) in bq.joins.iter().enumerate() {
            let Some(st) = bq.scopes.get(j.scope).and_then(|s| tables.get(s.table)) else {
                return Err(DataError::Forbidden);
            };
            let mut ints: Vec<i64> = tuples
                .iter()
                .filter_map(|t| t.get(i + 1).copied())
                .collect();
            ints.sort_unstable();
            ints.dedup();
            if ints.is_empty() {
                out.push(HashMap::new());
                continue;
            }
            let keys: TableKeys = st.table.keys;
            let mut sql = SqlBuf::scoped(keys.tenant_int, keys.table_int);
            let text = self.intern(
                "join_fetch",
                fetch_sql(&ctx.tenant_id, ints, None, &mut sql),
            );
            self.stats.statements += 1;
            let mut rows: Vec<JoinFetched> =
                super::sql::bind_query_as(sqlx::query_as::<_, JoinFetched>(&text), &sql.binds)
                    .fetch_all(&mut *self.conn)
                    .await
                    .map_err(map_db)?;
            self.captured.push(super::planner::exec::Captured {
                label: "join_fetch",
                sql: text.to_string(),
                binds: sql.binds,
            });
            let shared: HashSet<Uuid> = st.material.shared_record_ids.iter().copied().collect();
            let env = PolicyEnv {
                schema: &st.table.schema,
                material: &st.material,
                user: &ctx.principal.id,
                shared: &shared,
            };
            self.stats.verify_dropped += post_filter(
                &env,
                &mut rows,
                |r| RowView {
                    id: r.row.id,
                    owner: &r.row.owner,
                    data: &r.row.data.0,
                },
                st.table.id,
            );
            out.push(
                rows.into_iter()
                    .map(|r| (r.record_int, r.row.into_record(&st.table.schema)))
                    .collect(),
            );
        }
        Ok(out)
    }
}

fn empty_payload() -> CursorPayload {
    CursorPayload {
        sort_values: Vec::new(),
        record_int: 0,
        segment: 1,
        snapshot_seq: None,
        join: None,
    }
}

/// 擬似列の値（結合先の `_id` / `_owner` / `_created_at` / `_updated_at`）。
fn pseudo_value(r: &DataRecord, key: &str) -> Option<Value> {
    Some(match key {
        "_id" => Value::String(r.id.to_string()),
        "_owner" => Value::String(r.owner.clone()),
        "_created_at" => Value::String(r.created_at.to_rfc3339_opts(SecondsFormat::Micros, true)),
        "_updated_at" => Value::String(r.updated_at.to_rfc3339_opts(SecondsFormat::Micros, true)),
        _ => return None,
    })
}

impl DataStore {
    /// 組を items に組み立てる（FROM の行のレコードに `別名.参照名` で結合先の列を足す）。
    ///
    /// `base` と `children` はマスク・射影を済ませたもの。事後検査で落ちた行を含む組は返さない。
    pub(crate) fn compose_joined(
        bq: &BoundQuery,
        joined: &JoinedOut,
        base: Vec<DataRecord>,
    ) -> Vec<DataRecord> {
        let by_int: HashMap<i64, DataRecord> = joined.base_ints.iter().copied().zip(base).collect();
        let mut out = Vec::with_capacity(joined.tuples.len());
        'tuples: for t in &joined.tuples {
            let Some(b) = t.first().and_then(|r| by_int.get(r)) else {
                continue;
            };
            let mut rec = b.clone();
            for (i, j) in bq.joins.iter().enumerate() {
                let Some(c) = t
                    .get(i + 1)
                    .and_then(|r| joined.children.get(i).and_then(|m| m.get(r)))
                else {
                    continue 'tuples;
                };
                if let Some(obj) = rec.data.as_object_mut() {
                    for k in &j.select {
                        let v = pseudo_value(c, k)
                            .or_else(|| c.data.get(k).cloned())
                            .unwrap_or(Value::Null);
                        obj.insert(format!("{}.{k}", j.alias), v);
                    }
                }
            }
            out.push(rec);
        }
        out
    }
}
