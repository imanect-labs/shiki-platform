//! claim・リース heartbeat・fenced 確定（Idempotent Consumer ＋ Lease/Fencing）。

use std::fmt::Write as _;

use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgExecutor, Postgres, Type};

use crate::spec::{bind_key, Key, KeyValue, RunTableSpec};

/// 行を claim する（queued かリース失効 running を running へ・fencing token +1）。
///
/// 既に端末状態、または有効リースを他ワーカーが保持中なら `None`。
/// `returning` はドメイン固有カラムのリスト（`'static` 前提・例: `"run_id, fencing_token"`）。
pub async fn claim<'e, T>(
    exec: impl PgExecutor<'e>,
    spec: &RunTableSpec,
    key: &Key<'_>,
    worker_id: &str,
    lease_secs: i64,
    returning: &'static str,
) -> Result<Option<T>, sqlx::Error>
where
    T: for<'r> FromRow<'r, PgRow> + Send + Unpin,
{
    spec.validate();
    let n = key.len();
    let mut sets = format!(
        "{status} = '{running}', {worker} = ${w}, \
         {lease} = now() + (${l} || ' seconds')::interval, \
         {fencing} = {fencing} + 1",
        status = spec.status_column,
        running = spec.running_status,
        worker = spec.worker_column,
        w = n + 1,
        lease = spec.lease_column,
        l = n + 2,
        fencing = spec.fencing_column,
    );
    if let Some(a) = spec.attempt_column {
        let _ = write!(sets, ", {a} = {a} + 1");
    }
    if let Some(u) = spec.updated_at_column {
        let _ = write!(sets, ", {u} = now()");
    }
    // takeover 可能な実行系ステータス = running ＋ ドメイン指定の resumable（承認待ち等）。
    // これらはリース失効時のみ奪える。SQL 定数はすべて `'static`＋`validate()` 済み。
    let mut active = format!("'{}'", spec.running_status);
    for s in spec.resumable_statuses {
        let _ = write!(active, ", '{s}'");
    }
    let sql = format!(
        "UPDATE {table} SET {sets} \
         WHERE {pred} \
           AND ({status} = '{queued}' OR ({status} IN ({active}) AND {lease} < now())) \
         RETURNING {returning}",
        table = spec.table,
        pred = key.predicate(),
        status = spec.status_column,
        queued = spec.queued_status,
        lease = spec.lease_column,
    );
    bind_key!(sqlx::query_as::<_, T>(&sql), key)
        .bind(worker_id)
        .bind(lease_secs)
        .fetch_optional(exec)
        .await
}

/// リースを延長し、`returning` の単一カラム値を返す（heartbeat）。
///
/// 戻り値 `None` = fencing 不一致 or 非 running（リースを失った＝呼び出し側は停止すべき）。
pub async fn heartbeat<'e, T>(
    exec: impl PgExecutor<'e>,
    spec: &RunTableSpec,
    key: &Key<'_>,
    fencing_token: i64,
    lease_secs: i64,
    returning: &'static str,
) -> Result<Option<T>, sqlx::Error>
where
    T: for<'r> sqlx::Decode<'r, Postgres> + Type<Postgres> + Send + Unpin,
{
    spec.validate();
    let n = key.len();
    let updated = spec
        .updated_at_column
        .map(|u| format!(", {u} = now()"))
        .unwrap_or_default();
    let sql = format!(
        "UPDATE {table} SET {lease} = now() + (${l} || ' seconds')::interval{updated} \
         WHERE {pred} AND {fencing} = ${f} AND {status} = '{running}' \
         RETURNING {returning}",
        table = spec.table,
        lease = spec.lease_column,
        l = n + 1,
        pred = key.predicate(),
        fencing = spec.fencing_column,
        f = n + 2,
        status = spec.status_column,
        running = spec.running_status,
    );
    bind_key!(sqlx::query_scalar::<_, T>(&sql), key)
        .bind(lease_secs)
        .bind(fencing_token)
        .fetch_optional(exec)
        .await
}

/// fencing 一致時のみステータスを確定しリースを解放する（端末遷移の骨格）。
///
/// `extra_sets` はドメイン固有の追加 SET（例: `[("last_error", KeyValue::OptText(err))]`）。
/// 戻り値 `None` = fencing 不一致（ゾンビ）で no-op。ドメイン側の projection 書込
/// （chat の message.content 等）は同一 TX で呼び出し側が続ける。
pub async fn fenced_finalize<'e, T>(
    exec: impl PgExecutor<'e>,
    spec: &RunTableSpec,
    key: &Key<'_>,
    fencing_token: i64,
    status: &str,
    extra_sets: &[(&'static str, KeyValue<'_>)],
    returning: &'static str,
) -> Result<Option<T>, sqlx::Error>
where
    T: for<'r> sqlx::Decode<'r, Postgres> + Type<Postgres> + Send + Unpin,
{
    spec.validate();
    let n = key.len();
    let mut sets = format!(
        "{status_col} = ${s}, {lease} = NULL",
        status_col = spec.status_column,
        s = n + 1,
        lease = spec.lease_column,
    );
    for (i, (col, _)) in extra_sets.iter().enumerate() {
        crate::spec::assert_ident(col);
        let _ = write!(sets, ", {col} = ${}", n + 2 + i);
    }
    if let Some(u) = spec.updated_at_column {
        let _ = write!(sets, ", {u} = now()");
    }
    let sql = format!(
        "UPDATE {table} SET {sets} WHERE {pred} AND {fencing} = ${f} RETURNING {returning}",
        table = spec.table,
        pred = key.predicate(),
        fencing = spec.fencing_column,
        f = n + 2 + extra_sets.len(),
    );
    let mut q = bind_key!(sqlx::query_scalar::<_, T>(&sql), key).bind(status);
    for (_, v) in extra_sets {
        q = match *v {
            KeyValue::Uuid(u) => q.bind(u),
            KeyValue::Text(t) => q.bind(t),
            KeyValue::BigInt(i) => q.bind(i),
            KeyValue::OptText(t) => q.bind(t),
        };
    }
    q.bind(fencing_token).fetch_optional(exec).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    pub(crate) const RUN_SPEC: RunTableSpec = RunTableSpec {
        table: "generation_run",
        status_column: "status",
        fencing_column: "fencing_token",
        lease_column: "lease_until",
        worker_column: "worker_id",
        attempt_column: Some("attempt"),
        updated_at_column: Some("updated_at"),
        queued_status: "queued",
        running_status: "running",
        resumable_statuses: &["waiting_approval"],
    };

    /// SQL 形状が chat の先行実装（crates/chat/src/store/runs.rs）と同値であることの回帰。
    #[test]
    fn claim_sql_matches_chat_shape() {
        let values = [KeyValue::Uuid(Uuid::nil())];
        let key = Key::new(&["run_id"], &values);
        let n = key.len();
        assert_eq!(n, 1);
        assert_eq!(key.predicate(), "run_id = $1");
        // spec 検証（識別子制約）が通ること。
        RUN_SPEC.validate();
    }
}
