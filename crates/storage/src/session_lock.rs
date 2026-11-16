//! トランザクションを越えて持つ advisory lock（接続のセッションロック）。
//!
//! 外部（OpenFGA）への書込と、その巻き戻しまでを 1 つの区間で直列化するのに使う。トランザクションの
//! ロック（`pg_advisory_xact_lock`）は commit／rollback で外れるので、DB の失敗の後に行う巻き戻しが
//! 後続の操作と前後してしまう（古い判断で権限を戻す）。セッションのロックは解放するまで持つ。
//!
//! - 区間内の DB 操作は [`SessionLock::conn`] の**同じ接続**で行う。ロック用とは別の接続をプールから取ると、
//!   ロック待ちが接続を使い切ったときに、ロックを持つ側がトランザクションを開けず互いに待つ。
//! - ロックの取得・解放の途中も含め、解放を終えずに落とした（呼び出しが中断された）場合は接続ごと捨て、
//!   セッションの終了でロックを外す（ロックを持ったかもしれない接続をプールへ戻さない）。
//! - 鍵は呼び出し側が組み、テナントを含める（テナントを越えて互いを待たない）。

use sqlx::{pool::PoolConnection, PgConnection, PgPool, Postgres};

/// 解放するまで持つ advisory lock と、それを持つ接続。
pub struct SessionLock {
    conn: Option<PoolConnection<Postgres>>,
    key: String,
}

impl SessionLock {
    /// `key` のロックを取る（取れるまで待つ）。
    ///
    /// # Errors
    ///
    /// 接続の取得・ロックの問い合わせの失敗。
    pub async fn acquire(pool: &PgPool, key: String) -> Result<Self, sqlx::Error> {
        // 先にガードへ入れてから取得する（取得を待つ間に中断されても Drop が接続を捨てる）。
        let mut lock = Self {
            conn: Some(pool.acquire().await?),
            key,
        };
        if let Some(conn) = lock.conn.as_deref_mut() {
            sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
                .bind(lock.key.clone())
                .execute(conn)
                .await?;
        }
        Ok(lock)
    }

    /// ロックを持っている接続（区間内の DB 操作に使う）。解放後は `None`。
    pub fn conn(&mut self) -> Option<&mut PgConnection> {
        self.conn.as_deref_mut()
    }

    /// ロックを外して接続をプールへ返す。外せなかったら接続を閉じる（セッションの終了で外れる）。
    /// 外し終えるまで接続はガードの中に置く（解放を待つ間に中断されても Drop が接続を捨てる）。
    pub async fn release(mut self) {
        let unlocked: Result<bool, sqlx::Error> = match self.conn.as_deref_mut() {
            Some(conn) => {
                sqlx::query_scalar("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
                    .bind(self.key.clone())
                    .fetch_one(conn)
                    .await
            }
            None => return,
        };
        if matches!(unlocked, Ok(true)) {
            // 外せたので、接続はプールへ戻してよい（Drop で捨てない）。
            drop(self.conn.take());
        } else {
            tracing::warn!("session lock: ロックを外せないため接続を閉じる");
        }
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            // 解放を終えずに落とされた。ロックを持った接続をプールに戻さず、接続ごと閉じる。
            drop(conn.detach());
        }
    }
}
