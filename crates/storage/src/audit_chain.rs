//! 監査ログのハッシュチェーンの検証（#828）。
//!
//! 実データを変える操作の行（`chained`）は、tenant_id ＋ org ごとに `prev_hash` で直前の行へ
//! 連結されている。ここでは id 順にたどり、各行について
//!
//! 1. `prev_hash` が直前の行の `entry_hash` と一致する（先頭は空）
//! 2. 保存された値から計算し直した `entry_hash` が保存値と一致する
//!
//! を確かめ、最初に崩れた行を返す。行の書き換え・削除・差し込みを検知できる。
//! **鍵も外部アンカーも無い**ので、DB に書き込める者がその行以降を全部計算し直す改竄は検知
//! できない（PIT-12）。読み取り・拒否の行（`chained = false`）は連結しないので対象外。
//!
//! テナント ID の変更（`tenant.retenant`）より前の行は、旧 tenant_id でハッシュされている。
//! 変更の記録（metadata の `from`）から、行ごとにハッシュに使った tenant_id を戻して検証する。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use utoipa::ToSchema;

use crate::audit::{entry_hash, AuditAction, HashInput};
use crate::error::StorageError;

/// 1 回の問い合わせで読む行数。
const BATCH: i64 = 1000;

/// 崩れ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChainBreak {
    /// 直前の行とつながっていない（行の削除・差し込み・prev_hash の書き換え）。
    PrevMismatch,
    /// 行の内容が記録時と違う（列の書き換え）。
    HashMismatch,
}

/// 最初に崩れた行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BrokenEntry {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub action: String,
    pub reason: ChainBreak,
}

/// 検証の結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ChainReport {
    /// 今回確かめた行数。
    pub checked: u64,
    /// そのうち作成時刻を含まない古い版（版 1）の行数。
    pub legacy: u64,
    /// 最後に確かめた行の id（続きを確かめるときに `after_id` に渡す）。
    pub last_id: Option<i64>,
    /// 最初に崩れた行（崩れていなければ `None`）。
    pub broken: Option<BrokenEntry>,
    /// チェーンの末尾まで確かめたか（`false` なら `last_id` から続きがある）。
    pub complete: bool,
}

/// テナント ID の変更の境目（この id より前の行は `from` でハッシュされている）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rename {
    id: i64,
    from: String,
}

/// 行 `id` のハッシュに使われた tenant_id（変更の記録より前なら、その時点の旧 ID）。
fn hashed_tenant<'a>(renames: &'a [Rename], id: i64, current: &'a str) -> &'a str {
    renames
        .iter()
        .find(|r| id < r.id)
        .map_or(current, |r| r.from.as_str())
}

/// 監査ログのハッシュチェーンを検証する（読むだけ）。
#[derive(Clone)]
pub struct AuditChainVerifier {
    db: PgPool,
}

impl AuditChainVerifier {
    pub fn new(db: PgPool) -> Self {
        AuditChainVerifier { db }
    }

    /// `ctx` の tenant_id ＋ org のチェーンを、`after_id` の次から最大 `max_rows` 行確かめる。
    ///
    /// `after_id` を渡すと、その行（同じチェーンの連結行に限る）の `entry_hash` を直前の値として
    /// 続きから確かめる。無ければ先頭から確かめ、先頭の行の `prev_hash` は空でなければならない。
    pub async fn verify(
        &self,
        ctx: &AuthContext,
        after_id: Option<i64>,
        max_rows: u64,
    ) -> Result<ChainReport, StorageError> {
        let mut prev: Option<String> = match after_id {
            None => None,
            Some(id) => Some(
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT entry_hash FROM audit_log \
                     WHERE tenant_id = $1 AND org = $2 AND chained AND id = $3",
                )
                .bind(&ctx.tenant_id)
                .bind(&ctx.org)
                .bind(id)
                .fetch_optional(&self.db)
                .await?
                .flatten()
                .ok_or(StorageError::NotFound)?,
            ),
        };
        let renames = self.renames(ctx).await?;
        let mut report = ChainReport {
            checked: 0,
            legacy: 0,
            last_id: after_id,
            broken: None,
            complete: false,
        };
        let mut cursor = after_id.unwrap_or(0);
        loop {
            let rows = sqlx::query(
                "SELECT id, created_at, actor, action, object_type, object_id, decision, trace_id, \
                        metadata, prev_hash, entry_hash, hash_version \
                 FROM audit_log \
                 WHERE tenant_id = $1 AND org = $2 AND chained AND id > $3 \
                 ORDER BY id LIMIT $4",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(cursor)
            .bind(BATCH)
            .fetch_all(&self.db)
            .await?;
            let n = rows.len();
            for r in rows {
                let id: i64 = r.try_get("id")?;
                let created_at: DateTime<Utc> = r.try_get("created_at")?;
                let action: String = r.try_get("action")?;
                let metadata: serde_json::Value = r.try_get("metadata")?;
                let stored_prev: Option<String> = r.try_get("prev_hash")?;
                let stored: Option<String> = r.try_get("entry_hash")?;
                let version: i16 = r.try_get("hash_version")?;
                let actor: String = r.try_get("actor")?;
                let object_type: String = r.try_get("object_type")?;
                let object_id: String = r.try_get("object_id")?;
                let decision: String = r.try_get("decision")?;
                let trace_id: Option<String> = r.try_get("trace_id")?;
                let metadata_str = metadata.to_string();
                let computed = entry_hash(&HashInput {
                    version,
                    prev_hash: stored_prev.as_deref(),
                    tenant_id: hashed_tenant(&renames, id, &ctx.tenant_id),
                    org: &ctx.org,
                    actor: &actor,
                    action: &action,
                    object_type: &object_type,
                    object_id: &object_id,
                    decision: &decision,
                    trace_id: trace_id.as_deref(),
                    metadata: &metadata_str,
                    created_at,
                });
                let reason = if stored_prev != prev {
                    Some(ChainBreak::PrevMismatch)
                } else if stored.as_deref() != Some(computed.as_str()) {
                    Some(ChainBreak::HashMismatch)
                } else {
                    None
                };
                report.checked += 1;
                if version < 2 {
                    report.legacy += 1;
                }
                report.last_id = Some(id);
                if let Some(reason) = reason {
                    report.broken = Some(BrokenEntry {
                        id,
                        created_at,
                        action,
                        reason,
                    });
                    return Ok(report);
                }
                prev = stored;
                cursor = id;
                if report.checked >= max_rows {
                    // 続きがあるかは次の呼び出しで分かる（ちょうど末尾でも complete=false になる）。
                    return Ok(report);
                }
            }
            if n < BATCH as usize {
                report.complete = true;
                return Ok(report);
            }
        }
    }

    /// このチェーンでのテナント ID の変更（id 順）。
    async fn renames(&self, ctx: &AuthContext) -> Result<Vec<Rename>, StorageError> {
        let rows: Vec<(i64, Option<String>)> = sqlx::query_as(
            "SELECT id, metadata ->> 'from' FROM audit_log \
             WHERE tenant_id = $1 AND org = $2 AND chained AND action = $3 ORDER BY id",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(AuditAction::TenantRetenant.as_str())
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, from)| from.map(|from| Rename { id, from }))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_before_a_rename_use_the_old_tenant() {
        let renames = vec![
            Rename {
                id: 10,
                from: "a".into(),
            },
            Rename {
                id: 20,
                from: "b".into(),
            },
        ];
        assert_eq!(hashed_tenant(&renames, 5, "c"), "a");
        assert_eq!(
            hashed_tenant(&renames, 10, "c"),
            "b",
            "変更の記録そのものは新しい ID"
        );
        assert_eq!(hashed_tenant(&renames, 15, "c"), "b");
        assert_eq!(hashed_tenant(&renames, 20, "c"), "c");
        assert_eq!(hashed_tenant(&[], 1, "c"), "c");
    }
}
