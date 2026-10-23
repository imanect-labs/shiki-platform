//! 1 バッチ分の読込（ストリームの位置・変更行の before / after・awareness）。

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use super::hub::Sub;
use super::outbox::ChangeKind;
use super::visibility::RowState;
use crate::model::DataTable;
use crate::record::{RecordRow, RECORD_COLS};
use crate::store::DataStore;
use crate::{map_db, DataError};

/// ストリームの 1 件を解釈したもの。
pub(super) struct Parsed {
    pub(super) seq: i64,
    pub(super) record_id: Uuid,
    pub(super) kind: ChangeKind,
    pub(super) rev: i64,
    pub(super) before: Option<usize>,
    pub(super) after: Option<usize>,
}

/// awareness の 1 件（`state` は判定対象の行の添字）。
pub(super) struct Aware {
    pub(super) principal: String,
    pub(super) record_id: Option<Uuid>,
    pub(super) field: Option<String>,
    pub(super) at: DateTime<Utc>,
    pub(super) state: Option<usize>,
}

/// 1 バッチ分の読込結果。
pub(super) struct Batch {
    pub(super) parsed: Vec<Parsed>,
    pub(super) states: Vec<RowState>,
    pub(super) rows: HashMap<Uuid, RecordRow>,
    pub(super) awareness: Vec<Aware>,
    pub(super) awareness_at: Option<DateTime<Utc>>,
    pub(super) conn: sqlx::pool::PoolConnection<sqlx::Postgres>,
    /// 読めた最初の位置（読めなければ 0）。これより前を待つ購読は飛び番の手前で待つ。
    pub(super) first_seq: i64,
}

/// ストリームの 1 件の解釈（本体を引く前）。
struct Raw {
    seq: i64,
    record_id: Uuid,
    kind: ChangeKind,
    rev: i64,
    before: Option<Value>,
}

fn parse_entry(e: &storage::event::StreamEntry) -> Option<Raw> {
    let kind = e
        .payload
        .get("change")
        .and_then(Value::as_str)
        .and_then(ChangeKind::parse)?;
    let record_id = e
        .payload
        .get("record_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok())?;
    Some(Raw {
        seq: e.seq,
        record_id,
        kind,
        rev: e.payload.get("rev").and_then(Value::as_i64).unwrap_or(0),
        before: e
            .private_payload
            .as_ref()
            .and_then(|p| p.get("before"))
            .filter(|b| b.is_object())
            .cloned(),
    })
}

#[derive(sqlx::FromRow)]
struct AwRow {
    principal: String,
    record_id: Option<Uuid>,
    field_key: Option<String>,
    updated_at: DateTime<Utc>,
}

/// 判定対象の行の表（after はレコードごとに 1 つ・before は outbox の旧値ごと）。
struct States<'a> {
    rows: &'a HashMap<Uuid, RecordRow>,
    list: Vec<RowState>,
    after_of: HashMap<Uuid, usize>,
}

impl States<'_> {
    fn after(&mut self, id: Uuid) -> Option<usize> {
        if let Some(i) = self.after_of.get(&id) {
            return Some(*i);
        }
        let r = self.rows.get(&id)?;
        self.list.push(RowState {
            id,
            owner: r.owner.clone(),
            data: r.data.0.clone(),
        });
        self.after_of.insert(id, self.list.len() - 1);
        Some(self.list.len() - 1)
    }

    fn before(&mut self, id: Uuid, b: &Value) -> Option<usize> {
        let owner = b.get("owner").and_then(Value::as_str)?.to_string();
        let data = b.get("data").cloned().unwrap_or_else(|| json!({}));
        self.list.push(RowState { id, owner, data });
        Some(self.list.len() - 1)
    }
}

impl DataStore {
    pub(super) async fn load_batch(
        &self,
        tenant_id: &str,
        table: &DataTable,
        subs: &[Sub],
        awareness_after: Option<DateTime<Utc>>,
    ) -> Result<Batch, DataError> {
        let st = |e: storage::StorageError| DataError::Internal(format!("stream: {e}"));
        let min_cursor = subs.iter().map(|s| s.cursor).min().unwrap_or(0);
        let mut conn = self
            .pools
            .for_tenant(tenant_id)
            .acquire()
            .await
            .map_err(map_db)?;
        let mut entries = storage::event::stream_entries_after(
            &mut conn,
            &self.stream_key(table.id),
            min_cursor,
            self.stream_config.max_batch,
        )
        .await
        .map_err(st)?;
        // 穴の手前まで（番号は連続のはず。穴はコミット待ちか保持期間の GC で、どちらも飛ばさない）。
        let first_seq = entries.first().map_or(0, |e| e.seq);
        if let Some(cut) = entries.windows(2).position(|w| w[1].seq != w[0].seq + 1) {
            entries.truncate(cut + 1);
        }
        let changes: Vec<Raw> = entries.iter().filter_map(parse_entry).collect();
        // awareness（短命・データを含まない）。
        let since = awareness_after.unwrap_or_else(Utc::now);
        let ttl = chrono::Duration::from_std(self.stream_config.awareness_ttl)
            .unwrap_or(chrono::Duration::seconds(30));
        let aw_rows: Vec<AwRow> = sqlx::query_as(
            "SELECT principal, record_id, field_key, updated_at FROM data_stream_awareness \
             WHERE tenant_id = $1 AND table_id = $2 AND updated_at > $3 AND updated_at > $4 \
             ORDER BY updated_at",
        )
        .bind(tenant_id)
        .bind(table.id)
        .bind(since)
        .bind(Utc::now() - ttl)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        let awareness_at = aw_rows.last().map(|r| r.updated_at).or(Some(since));
        let mut ids: Vec<Uuid> = changes
            .iter()
            .filter(|r| !matches!(r.kind, ChangeKind::Schema | ChangeKind::Refresh))
            .map(|r| r.record_id)
            .chain(aw_rows.iter().filter_map(|r| r.record_id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let rows = fetch_rows(&mut conn, tenant_id, table, &ids).await?;
        let mut st = States {
            rows: &rows,
            list: Vec::new(),
            after_of: HashMap::new(),
        };
        let parsed: Vec<Parsed> = changes
            .into_iter()
            .map(|r| {
                let after = match r.kind {
                    ChangeKind::Delete | ChangeKind::Schema | ChangeKind::Refresh => None,
                    _ => st.after(r.record_id),
                };
                let before = r.before.as_ref().and_then(|b| st.before(r.record_id, b));
                Parsed {
                    seq: r.seq,
                    record_id: r.record_id,
                    kind: r.kind,
                    rev: r.rev,
                    before,
                    after,
                }
            })
            .collect();
        let awareness: Vec<Aware> = aw_rows
            .into_iter()
            .map(|a| Aware {
                state: a.record_id.and_then(|id| st.after(id)),
                principal: a.principal,
                record_id: a.record_id,
                field: a.field_key,
                at: a.updated_at,
            })
            .collect();
        let states = st.list;
        Ok(Batch {
            parsed,
            states,
            rows,
            awareness,
            awareness_at,
            conn,
            first_seq,
        })
    }
}

/// 変更行の現在の本体（tenant / table スコープ・必要な行だけ）。
async fn fetch_rows(
    conn: &mut sqlx::PgConnection,
    tenant_id: &str,
    table: &DataTable,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, RecordRow>, DataError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let fetched: Vec<RecordRow> = sqlx::query_as(&format!(
        "SELECT {RECORD_COLS} FROM data_record r \
         WHERE r.tenant_int = $1 AND r.table_int = $2 AND r.tenant_id = $3 AND r.id = ANY($4)"
    ))
    .bind(table.keys.tenant_int)
    .bind(table.keys.table_int)
    .bind(tenant_id)
    .bind(ids)
    .fetch_all(conn)
    .await
    .map_err(map_db)?;
    Ok(fetched.into_iter().map(|r| (r.id, r)).collect())
}
