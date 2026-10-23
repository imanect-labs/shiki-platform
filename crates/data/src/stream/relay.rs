//! outbox → テーブルのストリームへの採番（data-platform §9.2・Task 15.6）。
//!
//! 台帳 consumer [`LEDGER_CONSUMER`] として outbox を claim し、`data.record.changed` を
//! **テーブルごとのストリーム**（consumer = [`stream_key`]）へ `delivery_seq` を払い出して永続化する。
//!
//! - **リレー間の排他**: バッチの先頭で台帳 consumer の採番行をロックする（`outbox_delivery_seq`
//!   の行ロック）。並行リレーは直列化され、同じレコードの変更は `rev` 順に採番される。
//! - **ストリームはテーブル単位**: 番号はテーブルの中で穴の無い連番。テナント横断・テーブル横断の
//!   連番にしない（他テナントの書込量が `Last-Event-ID` から読めてしまう・PIT-75 と同型）。
//! - 採番は**ストリームへの永続化**で、SSE の送信はこの後（[`super::hub`]）。
//! - **派生値の再配信**: 参照先レコードの変更で表示値が変わる参照元レコード（ルックアップ・ロール
//!   アップ・数式）と、リンクの付け替えで対称列が変わる参照先レコードに、`change = derived` の
//!   イベントを同じ Tx で積む（次のバッチで参照元のストリームへ採番される）。1 段だけ辿り、派生の
//!   イベントからは辿らない（連鎖させない）。影響が大きすぎるときはテーブル単位の `refresh` に畳む。

use std::collections::{BTreeMap, BTreeSet};

use authz::{AuthContext, Principal, PrincipalKind};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::PgConnection;
use uuid::Uuid;

use super::outbox::{emit_record_changes, ChangeKind, RecordChange, RECORD_CHANGED_EVENT};
use crate::field_key::json_key;
use crate::index::state::field_id_i16;
use crate::model::DataTable;
use crate::store::{DataStore, TableRow, TABLE_COLS};
use crate::{map_db, DataError};

/// 台帳 consumer（outbox の全イベントを 1 度ずつ見る・GC の判定対象）。
pub const LEDGER_CONSUMER: &str = "data-stream";
/// 1 バッチで claim するイベント数。
pub(crate) const RELAY_BATCH_LEN: usize = 2_000;
const RELAY_BATCH: i64 = RELAY_BATCH_LEN as i64;
/// 1 つの参照元テーブルへ個別に積む派生イベントの上限（超えたらテーブル単位の refresh に畳む）。
const DERIVED_FANOUT_CAP: usize = 1_000;

/// テーブルのストリーム名（購読ストリームの consumer・台帳 consumer の名前空間の下）。
pub(crate) fn stream_key(ledger: &str, table_id: Uuid) -> String {
    format!("{ledger}:{table_id}")
}

impl DataStore {
    /// このストアの台帳 consumer の下での、テーブルのストリーム名。
    pub(crate) fn stream_key(&self, table_id: Uuid) -> String {
        stream_key(&self.stream_config.consumer, table_id)
    }
}

/// 保守経路の主体（派生イベントの outbox の actor）。認可には使わない。
pub(crate) fn system_ctx(tenant_id: &str, org: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: "system:data-stream".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        org.to_string(),
        tenant_id.to_string(),
    )
}

/// claim したレコード変更 1 件（派生の再配信の材料）。
struct Source {
    /// 元の変更イベント（outbox の id・再配信を 1 回に限る鍵）。
    event_id: i64,
    record_id: Uuid,
    record_int: i64,
    /// 対称列を持つリンク列の旧値（保存形）。
    before: Option<Value>,
}

impl DataStore {
    /// outbox の未採番のレコード変更を 1 バッチ、テーブルのストリームへ採番する（全プール）。
    /// 処理した outbox の件数（レコード変更以外も含む）を返す。並行に呼んでよい（台帳 consumer の
    /// 行ロックで直列化される）。
    pub async fn relay_record_changes_once(&self) -> Result<usize, DataError> {
        let mut total = 0;
        for (_, pool) in self.pools.all() {
            let mut tx = pool.begin().await.map_err(map_db)?;
            total += self.relay_batch(&mut tx).await?;
            tx.commit().await.map_err(map_db)?;
        }
        Ok(total)
    }

    async fn relay_batch(&self, tx: &mut PgConnection) -> Result<usize, DataError> {
        let st = |e: storage::StorageError| DataError::Internal(format!("outbox: {e}"));
        // 台帳 consumer の採番行をロック（並行リレーの排他・コミットまで保持）。
        let ledger = self.stream_config.consumer.as_str();
        storage::event::lock_consumer_seq(&mut *tx, ledger, None)
            .await
            .map_err(st)?;
        let events = storage::event::claim_undelivered(&mut *tx, ledger, RELAY_BATCH)
            .await
            .map_err(st)?;
        if events.is_empty() {
            return Ok(0);
        }
        let is_change = |e: &storage::OutboxEvent| {
            e.payload.get("event_type").and_then(Value::as_str) == Some(RECORD_CHANGED_EVENT)
        };
        let change_ids: Vec<i64> = events
            .iter()
            .filter(|e| is_change(e))
            .map(|e| e.id)
            .collect();
        let privates = storage::event::private_payloads(&mut *tx, &change_ids)
            .await
            .map_err(st)?;
        let mut streams: BTreeMap<(String, Uuid), Vec<i64>> = BTreeMap::new();
        let mut sources: BTreeMap<(String, Uuid), Vec<Source>> = BTreeMap::new();
        for e in &events {
            if !is_change(e) {
                continue;
            }
            let Some(table_id) = uuid_of(&e.payload, "table_id") else {
                continue;
            };
            streams
                .entry((e.tenant_id.clone(), table_id))
                .or_default()
                .push(e.id);
            let kind = e
                .payload
                .get("change")
                .and_then(Value::as_str)
                .and_then(ChangeKind::parse);
            if matches!(
                kind,
                Some(ChangeKind::Create | ChangeKind::Update | ChangeKind::Delete)
            ) {
                let private = privates.get(&e.id);
                let record_int = private
                    .and_then(|p| p.get("record_int"))
                    .and_then(Value::as_i64);
                if let (Some(record_id), Some(record_int)) =
                    (uuid_of(&e.payload, "record_id"), record_int)
                {
                    sources
                        .entry((e.tenant_id.clone(), table_id))
                        .or_default()
                        .push(Source {
                            event_id: e.id,
                            record_id,
                            record_int,
                            before: private
                                .and_then(|p| p.pointer("/before/data"))
                                .filter(|v| v.is_object())
                                .cloned(),
                        });
                }
            }
        }
        // ストリームの採番行はテーブル順にロックする（並行リレーとのデッドロックを避ける）。
        for ((tenant_id, table_id), ids) in &streams {
            storage::event::mark_delivered_seq(
                &mut *tx,
                &stream_key(ledger, *table_id),
                tenant_id,
                ids,
                self.stream_config.retention,
            )
            .await
            .map_err(st)?;
        }
        let all: Vec<i64> = events.iter().map(|e| e.id).collect();
        storage::event::mark_delivered(&mut *tx, ledger, &all)
            .await
            .map_err(st)?;
        for ((tenant_id, table_id), srcs) in sources {
            // 元の変更 1 件につき再配信は 1 回だけ（別の台帳 consumer のリレーも同じ変更を見る）。
            let srcs = claim_fanout(&mut *tx, &tenant_id, srcs).await?;
            if srcs.is_empty() {
                continue;
            }
            // 失敗しても採番は止めない（派生の再配信は表示の鮮度の問題で、権限ではない）。Tx を
            // 壊さないよう savepoint の中で行い、失敗したら巻き戻して再配信の台帳も戻す。
            let mut sp = sqlx::Connection::begin(&mut *tx).await.map_err(map_db)?;
            match self
                .fanout_derived(&mut sp, &tenant_id, table_id, &srcs)
                .await
            {
                Ok(()) => sp.commit().await.map_err(map_db)?,
                Err(e) => {
                    sp.rollback().await.map_err(map_db)?;
                    tracing::warn!(error = %e, %table_id, "派生値の再配信イベントの発行に失敗");
                }
            }
        }
        Ok(events.len())
    }

    /// 参照先テーブル `table_id` のレコード変更から、派生値・対称列が変わるレコードへ `derived` を積む。
    async fn fanout_derived(
        &self,
        tx: &mut PgConnection,
        tenant_id: &str,
        table_id: Uuid,
        srcs: &[Source],
    ) -> Result<(), DataError> {
        let Some(src_row) = table_row_with_org(tx, tenant_id, table_id).await? else {
            return Ok(());
        };
        let (src, src_org) = src_row;
        let ints: Vec<i64> = srcs.iter().map(|s| s.record_int).collect();
        // ① この表を参照先にする派生フィールド（ルックアップ・ロールアップ・数式）を持つ表の行。
        let filter = serde_json::json!([{ "ref_table": table_id }]);
        let referrers: Vec<(TableRow, String)> = sqlx::query_as::<_, TableRow>(&format!(
            "SELECT {TABLE_COLS} FROM data_table \
             WHERE tenant_id = $1 AND deleted_at IS NULL AND schema -> 'fields' @> $2::jsonb"
        ))
        .bind(tenant_id)
        .bind(Json(&filter))
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db)?
        .into_iter()
        .map(|r| (r, src_org.clone()))
        .collect();
        for (row, org) in referrers {
            let x = row.into_table();
            let links: Vec<String> = crate::derived::derived_links(&x)
                .into_iter()
                .filter(|(_, rt)| *rt == table_id)
                .map(|(k, _)| k)
                .collect();
            let mut affected: BTreeSet<i64> = BTreeSet::new();
            for key in links {
                let Some(l) = x.schema.field(&key) else {
                    continue;
                };
                let sym = l.link.as_ref().and_then(|d| d.symmetric_field);
                let got: Vec<i64> = match sym {
                    // 対称列あり: 参照先の逆方向行（参照先のパーティション・主キーの前方一致）。
                    Some(sym) => sqlx::query_scalar(
                        "SELECT DISTINCT dst_record FROM data_link \
                         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 \
                           AND src_record = ANY($4::int8[]) AND dst_table_int = $5 LIMIT $6",
                    )
                    .bind(src.keys.tenant_int)
                    .bind(src.keys.table_int)
                    .bind(field_id_i16(sym)?)
                    .bind(&ints)
                    .bind(x.keys.table_int)
                    .bind((DERIVED_FANOUT_CAP + 1) as i64)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(map_db)?,
                    None => sqlx::query_scalar(
                        "SELECT DISTINCT src_record FROM data_link \
                         WHERE tenant_int = $1 AND table_int = $2 AND field_id = $3 \
                           AND dst_table_int = $4 AND dst_record = ANY($5::int8[]) LIMIT $6",
                    )
                    .bind(x.keys.tenant_int)
                    .bind(x.keys.table_int)
                    .bind(field_id_i16(l.id)?)
                    .bind(src.keys.table_int)
                    .bind(&ints)
                    .bind((DERIVED_FANOUT_CAP + 1) as i64)
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(map_db)?,
                };
                affected.extend(got);
            }
            emit_derived(tx, tenant_id, &org, &x, RecordKeys::Ints(&affected)).await?;
        }
        // ② この表のリンク列（対称列あり）の付け替え: 旧・新の参照先の対称列が変わる。
        for f in &src.schema.fields {
            let (Some(link), Some(rt)) = (&f.link, f.ref_table) else {
                continue;
            };
            if link.reverse || link.symmetric_field.is_none() {
                continue;
            }
            let jk = json_key(f.id);
            let mut targets: BTreeSet<Uuid> = BTreeSet::new();
            for s in srcs {
                if let Some(b) = s.before.as_ref().and_then(|b| b.get(&jk)) {
                    targets.extend(uuids(b));
                }
            }
            let ids: Vec<Uuid> = srcs.iter().map(|s| s.record_id).collect();
            let now: Vec<Json<Value>> = sqlx::query_scalar(
                "SELECT data FROM data_record \
                 WHERE tenant_int = $1 AND table_int = $2 AND tenant_id = $3 AND id = ANY($4)",
            )
            .bind(src.keys.tenant_int)
            .bind(src.keys.table_int)
            .bind(tenant_id)
            .bind(&ids)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_db)?;
            for d in &now {
                if let Some(v) = d.0.get(&jk) {
                    targets.extend(uuids(v));
                }
            }
            if targets.is_empty() {
                continue;
            }
            let Some((y, y_org)) = table_row_with_org(tx, tenant_id, rt).await? else {
                continue;
            };
            let ids: Vec<Uuid> = targets.into_iter().collect();
            emit_derived(tx, tenant_id, &y_org, &y, RecordKeys::Ids(&ids)).await?;
        }
        Ok(())
    }
}

/// 再配信の台帳に元の変更を書けたもの（＝この呼び出しが再配信を担うもの）だけを返す。
///
/// 一意制約で直列化される: 並行の別リレーが同じ変更を先に書いていれば、そのコミットを待って
/// 衝突として外れる（先のリレーがロールバックしたらこちらが担う）。
async fn claim_fanout(
    tx: &mut PgConnection,
    tenant_id: &str,
    srcs: Vec<Source>,
) -> Result<Vec<Source>, DataError> {
    let ids: Vec<i64> = srcs.iter().map(|s| s.event_id).collect();
    let mine: Vec<i64> = sqlx::query_scalar(
        "INSERT INTO data_stream_fanout (event_id, tenant_id) \
         SELECT u.id, $2 FROM unnest($1::int8[]) AS u(id) \
         ON CONFLICT (event_id) DO NOTHING RETURNING event_id",
    )
    .bind(&ids)
    .bind(tenant_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db)?;
    Ok(srcs
        .into_iter()
        .filter(|s| mine.contains(&s.event_id))
        .collect())
}

enum RecordKeys<'a> {
    Ints(&'a BTreeSet<i64>),
    Ids(&'a [Uuid]),
}

/// `derived` を積む（多すぎればテーブル単位の `refresh` 1 件に畳む）。
async fn emit_derived(
    tx: &mut PgConnection,
    tenant_id: &str,
    org: &str,
    table: &DataTable,
    keys: RecordKeys<'_>,
) -> Result<(), DataError> {
    let (ints, ids): (Vec<i64>, Vec<Uuid>) = match keys {
        RecordKeys::Ints(s) => (s.iter().copied().collect(), Vec::new()),
        RecordKeys::Ids(s) => (Vec::new(), s.to_vec()),
    };
    if ints.is_empty() && ids.is_empty() {
        return Ok(());
    }
    let ctx = system_ctx(tenant_id, org);
    if ints.len() > DERIVED_FANOUT_CAP || ids.len() > DERIVED_FANOUT_CAP {
        return super::outbox::emit_table_refresh(tx, &ctx, table).await;
    }
    let rows: Vec<(Uuid, i64, i64)> = sqlx::query_as(
        "SELECT id, record_int, rev FROM data_record \
         WHERE tenant_int = $1 AND table_int = $2 AND tenant_id = $3 \
           AND (record_int = ANY($4::int8[]) OR id = ANY($5::uuid[])) \
         ORDER BY record_int",
    )
    .bind(table.keys.tenant_int)
    .bind(table.keys.table_int)
    .bind(tenant_id)
    .bind(&ints)
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db)?;
    let changes: Vec<RecordChange<'_>> = rows
        .into_iter()
        .map(|(record_id, record_int, rev)| RecordChange {
            record_id,
            record_int,
            rev,
            kind: ChangeKind::Derived,
            before: None,
            after: None,
        })
        .collect();
    emit_record_changes(tx, &ctx, table, &changes, None).await
}

/// 生存テーブル行と org（保守経路・tenant スコープ）。
pub(crate) async fn table_row_with_org(
    conn: &mut PgConnection,
    tenant_id: &str,
    id: Uuid,
) -> Result<Option<(DataTable, String)>, DataError> {
    let org: Option<String> = sqlx::query_scalar(
        "SELECT org FROM data_table WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(tenant_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    let Some(org) = org else { return Ok(None) };
    let row: Option<TableRow> = sqlx::query_as(&format!(
        "SELECT {TABLE_COLS} FROM data_table WHERE tenant_id = $1 AND id = $2 AND deleted_at IS NULL"
    ))
    .bind(tenant_id)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db)?;
    Ok(row.map(|r| (r.into_table(), org)))
}

fn uuid_of(v: &Value, key: &str) -> Option<Uuid> {
    v.get(key)?.as_str()?.parse().ok()
}

/// リンクの保存値（UUID 配列・移行前の単一文字列）から UUID を取り出す。
fn uuids(v: &Value) -> Vec<Uuid> {
    match v {
        Value::String(s) => s.parse().ok().into_iter().collect(),
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|s| s.parse().ok())
            .collect(),
        _ => Vec::new(),
    }
}
