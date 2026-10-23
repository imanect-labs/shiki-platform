//! スナップショット読取（data-platform §3.3・Task 15.3 の契約を Task 15.6 の配信位置で実装）。
//!
//! keyset が決定的なのは静的な集合に対してで、ライブページング（既定）はページの合間の更新・削除で
//! 重複・欠落し得る。エクスポート・一括処理など一貫性が要る経路のために、カーソルを
//! **配信位置 `snapshot_seq`**（SSE の `delivery_seq` と同じ時間軸）に束縛して、その時点の集合に対して
//! 重複・欠落なく返す。
//!
//! # 仕組み
//!
//! - 位置は**内部の不変な位置（`record_int`）の順**（おおむね作成順だが、採番のキャッシュで厳密な
//!   作成順ではない）。並べ替えキーが更新で動くと位置が動き重複・欠落の源に
//!   なるので、スナップショット読取は `sort` を受け付けない（422）。
//! - ページごとに「`snapshot_seq` より後に変わった行」（ストリームの `seq > S` と、まだ採番されて
//!   いない outbox）を引き、**現在の本体からは除き**、その行の `S` 時点の値をリビジョンから復元して
//!   同じ位置（`record_int`）に差し込む。復元した値にも行述語・条件を**同じ SQL**（v1 の本体経路）で
//!   評価する。`S` より後に作られた行は出さず、`S` より後に消えた行は `S` 時点の値で出す。
//! - 保持期間（ストリームの GC）を過ぎた `S` は 410（`S` より後の変更を数えられない）。
//! - 派生値（ルックアップ等）・マスクは他の読取と同じく読取時に閲覧者本人の権限で計算する
//!   （派生値は `S` 時点に巻き戻さない）。

use std::collections::HashMap;

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use sqlx::types::Json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::bound::BoundQuery;
use super::ir::{DataQuery, QueryResult};
use super::planner::verify::{post_filter, PolicyEnv, RowView};
use super::sql::{bind_query as bind_q, bind_query_as, Bind, SqlBuf};
use crate::cursor::{CursorPayload, CursorScope};
use crate::field_key::{decode_record, encode_record};
use crate::index::SortPrecision;
use crate::model::{DataRecord, DataTable, FieldPatch};
use crate::policy::compile::compile_read_predicate;
use crate::policy::material::PolicyMaterial;
use crate::record::RecordRow;
use crate::store::DataStore;
use crate::stream::outbox::{ChangeKind, RECORD_CHANGED_EVENT};
use crate::{map_db, DataError};

/// 基準位置より後の変更として数える上限（超えたら 410＝取り直し）。
const MAX_CHANGED_SINCE: i64 = 100_000;

/// 基準位置より後に変わった行。
#[derive(Debug, Clone)]
struct Changed {
    /// 基準位置の後で最初の変更の `rev`（`S` 時点の `rev` はこれ − 1。0 なら `S` に無かった）。
    first_rev: i64,
    record_int: Option<i64>,
    /// 削除の旧 owner（本体が無いときの owner）。
    owner: Option<String>,
}

/// 版 1 つ（`rev`・差分・時刻）。
type Version = (i64, Vec<FieldPatch>, DateTime<Utc>);

#[derive(sqlx::FromRow)]
struct RevRow {
    record_id: Uuid,
    rev: i64,
    patch: Json<Vec<FieldPatch>>,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct Fetched {
    record_int: i64,
    #[sqlx(flatten)]
    row: RecordRow,
}

impl DataStore {
    /// スナップショット読取の 1 ページ（同じ REPEATABLE READ の Tx 上）。
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) async fn run_snapshot(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        table: &DataTable,
        bq: &BoundQuery,
        m: &PolicyMaterial,
        query: &DataQuery,
        scope: &CursorScope<'_>,
    ) -> Result<(QueryResult, Vec<DataRecord>), DataError> {
        if !query.sort.is_empty() || bq.is_aggregate() || query.with_count {
            return Err(DataError::Invalid(
                "スナップショット読取は内部の不変な位置の順で返します（sort・集計・件数は指定できません）".into(),
            ));
        }
        let st = |e: storage::StorageError| DataError::Internal(format!("stream: {e}"));
        let stream = self.stream_key(table.id);
        let bounds = storage::event::stream_bounds(&mut *conn, &stream)
            .await
            .map_err(st)?;
        let (snapshot_seq, after) = match &query.cursor {
            Some(tok) => {
                let p = self.cursor.open(scope, tok)?;
                let s = p
                    .snapshot_seq
                    .ok_or_else(|| DataError::Invalid("カーソルが不正です".into()))?;
                (s, p.record_int)
            }
            None => (bounds.head, 0),
        };
        if !bounds.resumable(snapshot_seq) {
            return Err(DataError::Gone(format!(
                "snapshot_seq {snapshot_seq} は保持期間を過ぎました"
            )));
        }
        let changed = self
            .changed_since(conn, &ctx.tenant_id, table.id, &stream, snapshot_seq)
            .await?;
        let changed_ids: Vec<Uuid> = changed.keys().copied().collect();
        let limit = i64::from(bq.limit);

        // ① 基準位置の後に変わっていない行（現在の本体＝S 時点の値）。
        let mut sql = SqlBuf::scoped(table.keys.tenant_int, table.keys.table_int);
        let policy = compile_read_predicate(&table.schema, m, &ctx.principal.id, "r", &mut sql)?;
        let w = super::v1::where_sql(&body_lowerer(bq), &ctx.tenant_id, &policy, &mut sql)?;
        let aph = sql.typed(Bind::I64(after));
        let cph = sql.typed(Bind::UuidArray(changed_ids.clone()));
        let lph = sql.typed(Bind::I64(limit + 1));
        let text = format!(
            "SELECT r.record_int, {} FROM data_record r \
             WHERE r.tenant_int = $1 AND r.table_int = $2 AND {w} AND r.record_int > {aph} \
               AND NOT (r.id = ANY({cph})) ORDER BY r.record_int LIMIT {lph}",
            crate::record::RECORD_COLS
        );
        let mut live: Vec<Fetched> = bind_query_as(sqlx::query_as::<_, Fetched>(&text), &sql.binds)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db)?;
        // このページが覆う位置（record_int）の範囲（満杯なら limit+1 件目の手前まで）。
        let upper = if live.len() as i64 > limit {
            live.last().map_or(i64::MAX, |r| r.record_int)
        } else {
            i64::MAX
        };

        // ② 範囲内の「後で変わった行」を S 時点の値へ戻し、同じ条件・行述語で評価する。
        let in_range: HashMap<Uuid, Changed> = changed
            .into_iter()
            .filter(|(_, c)| {
                c.first_rev > 1 && c.record_int.is_some_and(|r| r > after && r <= upper)
            })
            .collect();
        let restored = self.restore_at(conn, ctx, table, &in_range).await?;
        let matched = self
            .match_restored(conn, ctx, table, bq, m, &restored)
            .await?;

        // ③ 位置の順に混ぜて 1 ページ分。
        let mut page: Vec<(i64, RowOut)> = live
            .drain(..)
            .map(|f| (f.record_int, RowOut::Live(f.row)))
            .chain(
                restored
                    .into_iter()
                    .filter(|r| matched.contains(&r.record_int))
                    .map(|r| (r.record_int, RowOut::Restored(r))),
            )
            .collect();
        page.sort_by_key(|(r, _)| *r);
        let more = page.len() as i64 > limit;
        page.truncate(usize::try_from(limit.max(0)).unwrap_or(0));
        let next = if more {
            page.last().map(|(r, _)| CursorPayload {
                sort_values: Vec::new(),
                record_int: *r,
                segment: 1,
                snapshot_seq: Some(snapshot_seq),
                join: None,
            })
        } else {
            None
        };
        // 事後検査（REST と同じ独立した評価器・不一致は落として警報）。
        let shared: std::collections::HashSet<Uuid> = m.shared_record_ids.iter().copied().collect();
        let env = PolicyEnv {
            schema: &table.schema,
            material: m,
            user: &ctx.principal.id,
            shared: &shared,
        };
        let mut rows: Vec<RowOut> = page.into_iter().map(|(_, r)| r).collect();
        post_filter(&env, &mut rows, RowOut::view, table.id);
        let items: Vec<DataRecord> = rows.into_iter().map(|r| r.into_record(table)).collect();
        let result = QueryResult {
            items: None,
            groups: None,
            next_cursor: match next {
                Some(p) => Some(self.cursor.seal(scope, &p)?),
                None => None,
            },
            count: None,
            sort_precision: SortPrecision::Exact,
            sort_degraded: false,
            shares_truncated: m.shares_truncated,
            suppressed: false,
            snapshot_seq: Some(snapshot_seq),
        };
        Ok((result, items))
    }

    /// 基準位置 `since` より後の変更（採番済み＋未採番）をレコードごとにまとめる。
    async fn changed_since(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        table_id: Uuid,
        stream: &str,
        since: i64,
    ) -> Result<HashMap<Uuid, Changed>, DataError> {
        let st = |e: storage::StorageError| DataError::Internal(format!("stream: {e}"));
        let streamed =
            storage::event::stream_entries_after(&mut *conn, stream, since, MAX_CHANGED_SINCE + 1)
                .await
                .map_err(st)?;
        if streamed.len() as i64 > MAX_CHANGED_SINCE {
            return Err(DataError::Gone(
                "基準位置より後の変更が多すぎます。取り直してください".into(),
            ));
        }
        let pending = storage::event::undelivered_table_events(
            &mut *conn,
            &self.stream_config.consumer,
            tenant_id,
            RECORD_CHANGED_EVENT,
            &table_id.to_string(),
        )
        .await
        .map_err(st)?;
        let mut out: HashMap<Uuid, Changed> = HashMap::new();
        let all = streamed
            .into_iter()
            .map(|e| (e.payload, e.private_payload))
            .chain(pending);
        for (payload, private) in all {
            let kind = payload
                .get("change")
                .and_then(Value::as_str)
                .and_then(ChangeKind::parse);
            // 派生値だけの再配信・スキーマ・取り直しは本体を変えない。
            if !matches!(
                kind,
                Some(ChangeKind::Create | ChangeKind::Update | ChangeKind::Delete)
            ) {
                continue;
            }
            let (Some(id), Some(rev)) = (
                payload
                    .get("record_id")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<Uuid>().ok()),
                payload.get("rev").and_then(Value::as_i64),
            ) else {
                continue;
            };
            let record_int = private
                .as_ref()
                .and_then(|p| p.get("record_int"))
                .and_then(Value::as_i64);
            let owner = private
                .as_ref()
                .and_then(|p| p.pointer("/before/owner"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let e = out.entry(id).or_insert(Changed {
                first_rev: rev,
                record_int,
                owner: None,
            });
            e.first_rev = e.first_rev.min(rev);
            e.record_int = e.record_int.or(record_int);
            if e.owner.is_none() {
                e.owner = owner;
            }
        }
        Ok(out)
    }

    /// 後で変わった行を S 時点（`first_rev - 1`）の値へ戻す（現在の本体＋リビジョンの逆適用）。
    async fn restore_at(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        table: &DataTable,
        changed: &HashMap<Uuid, Changed>,
    ) -> Result<Vec<Restored>, DataError> {
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = changed.keys().copied().collect();
        let from: Vec<i64> = ids.iter().map(|i| changed[i].first_rev - 1).collect();
        let current: Vec<Fetched> = sqlx::query_as(&format!(
            "SELECT r.record_int, {} FROM data_record r \
             WHERE r.tenant_int = $1 AND r.table_int = $2 AND r.tenant_id = $3 AND r.id = ANY($4)",
            crate::record::RECORD_COLS
        ))
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(&ctx.tenant_id)
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        let current: HashMap<Uuid, Fetched> = current.into_iter().map(|f| (f.row.id, f)).collect();
        // S 時点の版（updated_at）以降と、作成の版（created_at）だけを引く。
        let revs: Vec<RevRow> = sqlx::query_as(
            "SELECT v.record_id, v.rev, v.patch, v.created_at FROM data_record_revision v \
             JOIN unnest($3::uuid[], $4::int8[]) AS u(id, from_rev) ON v.record_id = u.id \
             WHERE v.tenant_id = $1 AND v.table_id = $2 AND (v.rev >= u.from_rev OR v.rev = 1) \
             ORDER BY v.record_id, v.rev DESC",
        )
        .bind(&ctx.tenant_id)
        .bind(table.id)
        .bind(&ids)
        .bind(&from)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db)?;
        let mut by_id: HashMap<Uuid, Vec<Version>> = HashMap::new();
        for r in revs {
            by_id
                .entry(r.record_id)
                .or_default()
                .push((r.rev, r.patch.0, r.created_at));
        }
        let mut out = Vec::new();
        for (id, c) in changed {
            let at_rev = c.first_rev - 1;
            let Some(history) = by_id.get(id) else {
                continue;
            };
            let cur = current.get(id);
            let (Some(record_int), Some(owner)) = (
                cur.map(|f| f.record_int).or(c.record_int),
                cur.map(|f| f.row.owner.clone()).or_else(|| c.owner.clone()),
            ) else {
                continue;
            };
            // 現在の値（削除済みなら空）から、S より後の版の差分を新しい順に巻き戻す。
            let mut data: Map<String, Value> = cur
                .map(|f| decode_record(&table.schema, &f.row.data.0))
                .unwrap_or_default();
            let mut complete = true;
            let mut expect = cur.map_or(i64::MAX, |f| f.row.rev);
            for (rev, patch, _) in history.iter().filter(|(r, _, _)| *r > at_rev) {
                if expect != i64::MAX && *rev != expect {
                    complete = false; // 版が欠けている（復元できない）
                    break;
                }
                for p in patch {
                    if p.old.is_null() {
                        data.remove(&p.field);
                    } else {
                        data.insert(p.field.clone(), p.old.clone());
                    }
                }
                expect = rev - 1;
            }
            if !complete {
                tracing::warn!(table_id = %table.id, "スナップショット読取: リビジョンが欠けた行を復元できません");
                continue;
            }
            let updated_at = history.iter().find(|(r, _, _)| *r == at_rev).map(|h| h.2);
            let created_at = history.iter().find(|(r, _, _)| *r == 1).map(|h| h.2);
            let (Some(updated_at), Some(created_at)) = (updated_at, created_at) else {
                continue;
            };
            out.push(Restored {
                id: *id,
                record_int,
                owner,
                rev: at_rev,
                stored: Value::Object(encode_record(&table.schema, &data)),
                created_at,
                updated_at,
            });
        }
        Ok(out)
    }

    /// 復元した行に、条件と行述語を本体経路と同じ SQL で評価する（一致した `record_int`）。
    async fn match_restored(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        table: &DataTable,
        bq: &BoundQuery,
        m: &PolicyMaterial,
        rows: &[Restored],
    ) -> Result<std::collections::HashSet<i64>, DataError> {
        if rows.is_empty() {
            return Ok(std::collections::HashSet::new());
        }
        let set: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "record_int": r.record_int,
                    "tenant_id": ctx.tenant_id,
                    "id": r.id,
                    "owner": r.owner,
                    "data": r.stored,
                    "created_at": r.created_at,
                    "updated_at": r.updated_at,
                })
            })
            .collect();
        let mut sql = SqlBuf {
            binds: vec![Bind::Text(Value::Array(set).to_string())],
            table: None,
            aliases: 0,
        };
        let policy = compile_read_predicate(&table.schema, m, &ctx.principal.id, "r", &mut sql)?;
        let w = super::v1::where_sql(&body_lowerer(bq), &ctx.tenant_id, &policy, &mut sql)?;
        let text = format!(
            "SELECT r.record_int FROM jsonb_to_recordset($1::jsonb) AS r(record_int int8, \
               tenant_id text, id uuid, owner text, data jsonb, created_at timestamptz, \
               updated_at timestamptz) WHERE {w}"
        );
        let got = bind_q(sqlx::query(&text), &sql.binds)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db)?;
        got.iter()
            .map(|r| sqlx::Row::try_get::<i64, _>(r, 0).map_err(map_db))
            .collect()
    }
}

/// S 時点へ戻した行。
struct Restored {
    id: Uuid,
    record_int: i64,
    owner: String,
    rev: i64,
    /// 保存形（`"f{id}"`）。
    stored: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

enum RowOut {
    Live(RecordRow),
    Restored(Restored),
}

impl RowOut {
    fn view(&self) -> RowView<'_> {
        match self {
            RowOut::Live(r) => RowView {
                id: r.id,
                owner: &r.owner,
                data: &r.data.0,
            },
            RowOut::Restored(r) => RowView {
                id: r.id,
                owner: &r.owner,
                data: &r.stored,
            },
        }
    }

    fn into_record(self, table: &DataTable) -> DataRecord {
        match self {
            RowOut::Live(r) => r.into_record(&table.schema),
            RowOut::Restored(r) => DataRecord {
                id: r.id,
                table_id: table.id,
                data: Value::Object(decode_record(&table.schema, &r.stored)),
                rev: r.rev,
                owner: r.owner,
                created_at: r.created_at,
                updated_at: r.updated_at,
            },
        }
    }
}

/// スナップショット読取の本体経路の降下（リンクの半結合は受け付けないので参照先は持たない）。
fn body_lowerer(bq: &BoundQuery) -> super::planner::lower::Lowerer<'_> {
    static NO_LINKS: std::sync::LazyLock<std::collections::HashMap<Uuid, i64>> =
        std::sync::LazyLock::new(std::collections::HashMap::new);
    super::planner::lower::Lowerer {
        q: bq,
        links: &NO_LINKS,
        scopes: &[],
    }
}
