//! 並行制御カウンタ（3 階層・claim 時予約・engine.md §8.2）。
//!
//! claim 直後に `current_n < limit_n` を満たす行だけ +1 予約する。取れなければ **拒否ではなく
//! 順番待ち**（呼び出し側が ready+backoff に戻す）。完了時に -1 する。カウンタは global（テナント全体）・
//! workflow（ワークフロー単位）・node（ノード単位）の 3 階層で、全階層の予約に成功して初めて実行する。

use sqlx::PgPool;
use uuid::Uuid;

/// 並行スコープの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Global,
    Workflow,
    Node,
}

impl ScopeKind {
    const fn as_str(self) -> &'static str {
        match self {
            ScopeKind::Global => "global",
            ScopeKind::Workflow => "workflow",
            ScopeKind::Node => "node",
        }
    }
}

/// 予約するスコープ 1 件（種別＋キー＋上限）。
#[derive(Debug, Clone)]
pub struct Slot {
    pub kind: ScopeKind,
    pub key: String,
    pub limit: i32,
}

impl Slot {
    #[must_use]
    pub fn global(limit: i32) -> Self {
        Slot {
            kind: ScopeKind::Global,
            key: String::new(),
            limit,
        }
    }
    #[must_use]
    pub fn workflow(workflow_id: Uuid, limit: i32) -> Self {
        Slot {
            kind: ScopeKind::Workflow,
            key: workflow_id.to_string(),
            limit,
        }
    }
    /// node 階層は**ノード種（capability kind）単位**で共有する（engine.md §8）。同一種の複数ノードが
    /// 単一カウンタを共有し、per-kind 上限を正しく効かせる（従来は node_id 別で上限が破れていた）。
    #[must_use]
    pub fn node_kind(workflow_id: Uuid, node_kind: &str, limit: i32) -> Self {
        Slot {
            kind: ScopeKind::Node,
            key: format!("{workflow_id}|{node_kind}"),
            limit,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConcurrencyError {
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> ConcurrencyError {
    ConcurrencyError::Internal(format!("db: {e}"))
}

/// 並行カウンタのデータチョークポイント。
#[derive(Clone)]
pub struct ConcurrencyStore {
    db: PgPool,
}

impl ConcurrencyStore {
    pub fn new(db: PgPool) -> Self {
        ConcurrencyStore { db }
    }

    /// 全スロットを **all-or-nothing** で予約する（単一 TX）。取れれば true、超過で 1 つでも
    /// 取れなければ全て巻き戻して false（順番待ち）。
    pub async fn try_acquire(
        &self,
        tenant_id: &str,
        slots: &[Slot],
    ) -> Result<bool, ConcurrencyError> {
        // 行ロック順を安定化して**デッドロックを回避**する（呼び出し側が異なる順で同一スコープを
        // 予約しても Postgres で相互待ちにならない）。(kind, key) で整列＋重複排除。
        let mut ordered: Vec<&Slot> = slots.iter().collect();
        ordered.sort_by(|a, b| {
            (a.kind.as_str(), a.key.as_str()).cmp(&(b.kind.as_str(), b.key.as_str()))
        });
        ordered.dedup_by(|a, b| a.kind == b.kind && a.key == b.key);

        let mut tx = self.db.begin().await.map_err(map_db)?;
        for s in ordered {
            // 行を用意（初回）。limit は IR 由来の最新値で更新（current は保持）。
            sqlx::query(
                "INSERT INTO concurrency_counter (tenant_id, scope_kind, scope_key, limit_n) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (tenant_id, scope_kind, scope_key) DO UPDATE SET limit_n = $4",
            )
            .bind(tenant_id)
            .bind(s.kind.as_str())
            .bind(&s.key)
            .bind(s.limit)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            // current < limit の行だけ +1（取れなければ 0 行 → 予約失敗）。
            let updated = sqlx::query(
                "UPDATE concurrency_counter SET current_n = current_n + 1, updated_at = now() \
                 WHERE tenant_id = $1 AND scope_kind = $2 AND scope_key = $3 \
                   AND current_n < limit_n",
            )
            .bind(tenant_id)
            .bind(s.kind.as_str())
            .bind(&s.key)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
            if updated.rows_affected() == 0 {
                // 1 つでも取れなければ全巻き戻し（部分予約を残さない）。
                tx.rollback().await.map_err(map_db)?;
                return Ok(false);
            }
        }
        tx.commit().await.map_err(map_db)?;
        Ok(true)
    }

    /// カウンタを実 running 数へ突合する（scheduler tick・リーダーのみ・リーク回収）。
    ///
    /// リース失効 takeover やプロセス kill で release が漏れたぶんを、実際の running step 数から
    /// 再計算して回収する。node 階層は ir_snapshot からノード種を引く（running は少数前提）。
    pub async fn reconcile(&self) -> Result<u64, ConcurrencyError> {
        let mut fixed = 0u64;
        // global（tenant 全体）。
        fixed += sqlx::query(
            "UPDATE concurrency_counter c SET current_n = sub.actual, updated_at = now()              FROM (SELECT c2.tenant_id, c2.scope_key,                           (SELECT count(*) FROM step_execution s                            WHERE s.tenant_id = c2.tenant_id AND s.status = 'running') AS actual                    FROM concurrency_counter c2 WHERE c2.scope_kind = 'global') sub              WHERE c.scope_kind = 'global' AND c.tenant_id = sub.tenant_id                AND c.scope_key = sub.scope_key AND c.current_n <> sub.actual",
        )
        .execute(&self.db)
        .await
        .map_err(map_db)?
        .rows_affected();
        // workflow 単位。
        fixed += sqlx::query(
            "UPDATE concurrency_counter c SET current_n = sub.actual, updated_at = now()              FROM (SELECT c2.tenant_id, c2.scope_key,                           (SELECT count(*) FROM step_execution s                            JOIN workflow_run r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id                            WHERE s.tenant_id = c2.tenant_id AND s.status = 'running'                              AND r.workflow_id::text = c2.scope_key) AS actual                    FROM concurrency_counter c2 WHERE c2.scope_kind = 'workflow') sub              WHERE c.scope_kind = 'workflow' AND c.tenant_id = sub.tenant_id                AND c.scope_key = sub.scope_key AND c.current_n <> sub.actual",
        )
        .execute(&self.db)
        .await
        .map_err(map_db)?
        .rows_affected();
        // node 種単位（scope_key = '<workflow_id>|<node_kind>'・ノード種は ir_snapshot から導出）。
        fixed += sqlx::query(
            "UPDATE concurrency_counter c SET current_n = sub.actual, updated_at = now()              FROM (SELECT c2.tenant_id, c2.scope_key,                           (SELECT count(*) FROM step_execution s                            JOIN workflow_run r ON r.tenant_id = s.tenant_id AND r.run_id = s.run_id                            WHERE s.tenant_id = c2.tenant_id AND s.status = 'running'                              AND r.workflow_id::text = split_part(c2.scope_key, '|', 1)                              AND (SELECT n->>'type'                                   FROM jsonb_array_elements(r.ir_snapshot->'nodes') n                                   WHERE n->>'id' = s.node_id LIMIT 1)                                  = split_part(c2.scope_key, '|', 2)) AS actual                    FROM concurrency_counter c2 WHERE c2.scope_kind = 'node') sub              WHERE c.scope_kind = 'node' AND c.tenant_id = sub.tenant_id                AND c.scope_key = sub.scope_key AND c.current_n <> sub.actual",
        )
        .execute(&self.db)
        .await
        .map_err(map_db)?
        .rows_affected();
        Ok(fixed)
    }

    /// 予約を解放する（完了時・全スロット -1・0 未満にはしない）。
    pub async fn release(&self, tenant_id: &str, slots: &[Slot]) -> Result<(), ConcurrencyError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        for s in slots {
            sqlx::query(
                "UPDATE concurrency_counter SET current_n = greatest(current_n - 1, 0), \
                 updated_at = now() \
                 WHERE tenant_id = $1 AND scope_kind = $2 AND scope_key = $3",
            )
            .bind(tenant_id)
            .bind(s.kind.as_str())
            .bind(&s.key)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(())
    }
}
