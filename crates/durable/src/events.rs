//! append-only イベントログ（`(キー, seq)` 単調 seq・exactly-once・fencing ゲート）。

use serde::de::DeserializeOwned;
use sqlx::types::Json;
use sqlx::PgExecutor;

use crate::spec::{assert_ident, bind_key, EventTableSpec, Key, RunTableSpec};

/// イベントを append-only で追記する（単調 seq・exactly-once）。
///
/// **fencing 一致（＝現リース保持ワーカー）時のみ**追記する（ゾンビ書込拒否）。
/// fencing 不一致は seq を返さず `None`（呼び出し側はリース喪失として停止する）。
/// Redis への publish は呼び出し側が seq 確定後に行う（DB=truth・Redis=best-effort）。
pub async fn append_event<'e>(
    exec: impl PgExecutor<'e>,
    run: &RunTableSpec,
    ev: &EventTableSpec,
    key: &Key<'_>,
    kind: &str,
    payload: &serde_json::Value,
    fencing_token: i64,
) -> Result<Option<i64>, sqlx::Error> {
    run.validate();
    ev.validate();
    let n = key.len();
    let sql = format!(
        "INSERT INTO {evt} ({key_cols}, {seq}, {kind_col}, {payload_col}) \
         SELECT {key_vals}, \
                coalesce((SELECT max({seq}) FROM {evt} WHERE {pred}), 0) + 1, \
                ${k}, ${p} \
         WHERE (SELECT {fencing} FROM {run_table} WHERE {pred}) = ${f} \
         RETURNING {seq}",
        evt = ev.table,
        key_cols = key.column_list(),
        seq = ev.seq_column,
        kind_col = ev.kind_column,
        payload_col = ev.payload_column,
        key_vals = key.placeholders(),
        pred = key.predicate(),
        k = n + 1,
        p = n + 2,
        fencing = run.fencing_column,
        run_table = run.table,
        f = n + 3,
    );
    bind_key!(sqlx::query_scalar::<_, i64>(&sql), key)
        .bind(kind)
        .bind(Json(payload))
        .bind(fencing_token)
        .fetch_optional(exec)
        .await
}

/// fencing を無視してイベントを追記する（強制失敗・sweeper 用の backstop）。
///
/// run 行のステータスが `allowed_statuses`（例: `["queued", "running"]`）のときのみ
/// 追記する（既に端末状態なら `None` で no-op）。
pub async fn append_event_unfenced<'e>(
    exec: impl PgExecutor<'e>,
    run: &RunTableSpec,
    ev: &EventTableSpec,
    key: &Key<'_>,
    kind: &str,
    payload: &serde_json::Value,
    allowed_statuses: &[&'static str],
) -> Result<Option<i64>, sqlx::Error> {
    run.validate();
    ev.validate();
    let statuses = allowed_statuses
        .iter()
        .map(|s| {
            assert_ident(s);
            format!("'{s}'")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let n = key.len();
    let sql = format!(
        "INSERT INTO {evt} ({key_cols}, {seq}, {kind_col}, {payload_col}) \
         SELECT {key_vals}, \
                coalesce((SELECT max({seq}) FROM {evt} WHERE {pred}), 0) + 1, \
                ${k}, ${p} \
         WHERE EXISTS (SELECT 1 FROM {run_table} \
                       WHERE {pred} AND {status} IN ({statuses})) \
         RETURNING {seq}",
        evt = ev.table,
        key_cols = key.column_list(),
        seq = ev.seq_column,
        kind_col = ev.kind_column,
        payload_col = ev.payload_column,
        key_vals = key.placeholders(),
        pred = key.predicate(),
        k = n + 1,
        p = n + 2,
        run_table = run.table,
        status = run.status_column,
    );
    bind_key!(sqlx::query_scalar::<_, i64>(&sql), key)
        .bind(kind)
        .bind(Json(payload))
        .fetch_optional(exec)
        .await
}

/// `from_seq` より後のイベントを seq 順に replay する（真実のソース・SSE の補填/復元）。
pub async fn replay_events<'e, T>(
    exec: impl PgExecutor<'e>,
    ev: &EventTableSpec,
    key: &Key<'_>,
    from_seq: i64,
) -> Result<Vec<(i64, T)>, sqlx::Error>
where
    T: DeserializeOwned + Send + Unpin + 'static,
{
    ev.validate();
    let n = key.len();
    let sql = format!(
        "SELECT {seq}, {payload} FROM {evt} \
         WHERE {pred} AND {seq} > ${s} ORDER BY {seq}",
        seq = ev.seq_column,
        payload = ev.payload_column,
        evt = ev.table,
        pred = key.predicate(),
        s = n + 1,
    );
    let rows: Vec<(i64, Json<T>)> = bind_key!(sqlx::query_as(&sql), key)
        .bind(from_seq)
        .fetch_all(exec)
        .await?;
    Ok(rows.into_iter().map(|(seq, p)| (seq, p.0)).collect())
}
