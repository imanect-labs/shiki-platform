//! リーダーリース（単一行 CAS・engine.md §5.1）。
//!
//! 複数インスタンスのうち 1 つだけがスケジューラループを回す（多重発火防止）。`scheduler_lease`
//! の単一行（id=1）を CAS で奪い合い、保持者だけが tick を実行する。失効で別インスタンスが引き継ぐ。

use sqlx::PgPool;

/// リーダーリースのハンドル。
#[derive(Clone)]
pub struct LeaderLease {
    db: PgPool,
    owner: String,
    lease_secs: i64,
}

impl LeaderLease {
    pub fn new(db: PgPool, owner: impl Into<String>, lease_secs: i64) -> Self {
        LeaderLease {
            db,
            owner: owner.into(),
            lease_secs,
        }
    }

    /// リーダーを獲得・更新する（自分が保持者か・空き/失効なら奪取）。true=自分がリーダー。
    pub async fn acquire_or_renew(&self) -> Result<bool, sqlx::Error> {
        // 単一 UPSERT: 行が無ければ作る。あれば owner が自分 or 失効時のみ奪う。
        let acquired: Option<String> = sqlx::query_scalar(
            "INSERT INTO scheduler_lease (id, owner, expires_at) \
             VALUES (1, $1, now() + ($2 || ' seconds')::interval) \
             ON CONFLICT (id) DO UPDATE SET owner = $1, \
                 expires_at = now() + ($2 || ' seconds')::interval \
             WHERE scheduler_lease.owner = $1 OR scheduler_lease.expires_at < now() \
             RETURNING owner",
        )
        .bind(&self.owner)
        .bind(self.lease_secs)
        .fetch_optional(&self.db)
        .await?;
        Ok(acquired.as_deref() == Some(self.owner.as_str()))
    }

    /// リーダーを明け渡す（自分が保持者のときのみ）。
    pub async fn release(&self) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM scheduler_lease WHERE id = 1 AND owner = $1")
            .bind(&self.owner)
            .execute(&self.db)
            .await?;
        Ok(())
    }
}
