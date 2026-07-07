//! effect journal（内部能力の副作用を高々 1 回・PIT-31・engine.md §7.3）。
//!
//! 冪等キー＋op_digest（`sha256(api＋正規化パラメータ)`）で副作用を UNIQUE 記録する。
//! 同一キーで再実行が来たら記録済み結果を no-op で返す。キー衝突かつ digest 不一致は
//! **permanent エラー**（同じ論理操作で違うことをしようとしている＝バグ or 攻撃）。

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

/// journal 参照の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalDecision {
    /// **予約に成功**。副作用を実行してよい（実行後 [`EffectJournal::record`]）。
    Proceed,
    /// 記録済み。副作用は実行せず記録結果を返す（no-op）。
    AlreadyDone(Value),
    /// 別ワーカーが予約済みでまだ結果未確定（実行中）。副作用を**実行しない**（二重送信を防ぐ）。
    InProgress,
    /// 同一キーで別の操作（digest 不一致）。permanent エラーにする。
    DigestMismatch,
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("内部エラー: {0}")]
    Internal(String),
}

#[allow(clippy::needless_pass_by_value)]
fn map_db(e: sqlx::Error) -> JournalError {
    JournalError::Internal(format!("db: {e}"))
}

/// api 名＋パラメータから決定的な op_digest を作る（キー正規化のため JSON をソート出力）。
#[must_use]
pub fn op_digest(api: &str, params: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api.as_bytes());
    hasher.update(b"\0");
    // serde_json は BTreeMap 化しないが、Value::Object は挿入順。安定化のため再帰ソートする。
    hasher.update(canonical_json(params).as_bytes());
    format!("{:x}", hasher.finalize())
}

/// JSON を鍵ソートして正規化文字列にする（digest 安定化）。
fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| format!("{:?}:{}", k, canonical_json(&map[*k])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(arr) => {
            let inner: Vec<String> = arr.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// effect journal のデータチョークポイント。
#[derive(Clone)]
pub struct EffectJournal {
    db: PgPool,
}

impl EffectJournal {
    pub fn new(db: PgPool) -> Self {
        EffectJournal { db }
    }

    /// 副作用実行の前に**アトミックに予約**する（外部副作用の at-most-once の要）。
    ///
    /// `INSERT ... ON CONFLICT DO NOTHING` で行を占有する。勝者だけ `Proceed`（副作用を実行）。
    /// 競合時は既存行を見て、digest 不一致=`DigestMismatch`・結果あり=`AlreadyDone`・結果未確定=
    /// `InProgress`（別ワーカー実行中＝二重送信しない）。リース takeover で 2 ワーカーが同 step に
    /// 入っても、この予約が 1 つに絞るため副作用は高々 1 回（PIT-31）。
    pub async fn check(
        &self,
        tenant_id: &str,
        idempotency_key: &str,
        digest: &str,
    ) -> Result<JournalDecision, JournalError> {
        // 予約 INSERT（result_summary=NULL）。勝てば 1 行返る。
        let won: Option<bool> = sqlx::query_scalar(
            "INSERT INTO effect_journal (tenant_id, idempotency_key, op_digest) \
             VALUES ($1, $2, $3) ON CONFLICT (tenant_id, idempotency_key) DO NOTHING \
             RETURNING true",
        )
        .bind(tenant_id)
        .bind(idempotency_key)
        .bind(digest)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        if won.is_some() {
            return Ok(JournalDecision::Proceed);
        }
        // 競合: 既存行の digest と結果を見る。
        let row: Option<(String, Option<Value>)> = sqlx::query_as(
            "SELECT op_digest, result_summary FROM effect_journal \
             WHERE tenant_id = $1 AND idempotency_key = $2",
        )
        .bind(tenant_id)
        .bind(idempotency_key)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        match row {
            Some((d, _)) if d != digest => Ok(JournalDecision::DigestMismatch),
            Some((_, Some(summary))) => Ok(JournalDecision::AlreadyDone(summary)),
            Some((_, None)) => Ok(JournalDecision::InProgress),
            None => Ok(JournalDecision::Proceed), // 直前に削除された稀ケースは再挑戦。
        }
    }

    /// 副作用実行の**後**に予約行へ結果を書く（レダクト済みを渡すこと）。
    ///
    /// 予約（check の Proceed）に対応する UPDATE。行が無ければ（予約無し呼び出し）INSERT で補う。
    pub async fn record(
        &self,
        tenant_id: &str,
        idempotency_key: &str,
        digest: &str,
        result_summary: &Value,
    ) -> Result<(), JournalError> {
        sqlx::query(
            "INSERT INTO effect_journal (tenant_id, idempotency_key, op_digest, result_summary) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (tenant_id, idempotency_key) \
               DO UPDATE SET result_summary = COALESCE(effect_journal.result_summary, $4)",
        )
        .bind(tenant_id)
        .bind(idempotency_key)
        .bind(digest)
        .bind(result_summary)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        Ok(())
    }

    /// DB トランザクション内で「予約→（呼び出し側が副作用）→結果更新」する版のための予約 INSERT。
    ///
    /// storage.write のように副作用と journal を**同一 TX**で書ける内部能力向け。UNIQUE で占有し、
    /// 取れれば true（呼び出し側が副作用を続行）、取れなければ false（既記録）。
    pub async fn reserve_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        tenant_id: &str,
        idempotency_key: &str,
        digest: &str,
    ) -> Result<bool, JournalError> {
        let inserted: Option<bool> = sqlx::query_scalar(
            "INSERT INTO effect_journal (tenant_id, idempotency_key, op_digest) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING true",
        )
        .bind(tenant_id)
        .bind(idempotency_key)
        .bind(digest)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_db)?;
        Ok(inserted.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn digest_is_key_order_independent() {
        let a = op_digest("storage.write", &json!({ "path": "/a", "bytes": 1 }));
        let b = op_digest("storage.write", &json!({ "bytes": 1, "path": "/a" }));
        assert_eq!(a, b, "鍵順に依存しない");
        let c = op_digest("storage.write", &json!({ "path": "/b", "bytes": 1 }));
        assert_ne!(a, c, "内容が違えば digest も違う");
    }

    #[test]
    fn digest_distinguishes_api() {
        let a = op_digest("storage.write", &json!({ "x": 1 }));
        let b = op_digest("storage.read", &json!({ "x": 1 }));
        assert_ne!(a, b);
    }
}
