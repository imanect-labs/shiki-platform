//! WOPI ロック（30 分 TTL・lazy 解放・PIT-44）。
//!
//! ロックは「編集排他」ではなく **助言的**なもの（クラッシュ残留・期限切れ残留があり得る。
//! ロック存在＝セッション実在ではない）。認可の真実源にはせず、役割は 2 つ:
//! ① WOPI プロトコル準拠（Collabora のロック検証・競合は 409＋X-WOPI-Lock）
//! ② Task 11.8 の AI 編集が「編集セッション中か」を [`current_lock`] で判定し、
//! ロック中は提案バージョン保存へ迂回する（人間の未保存編集を上書きしない）。
//!
//! 期限切れは次アクセス時に無視・削除する（lazy 解放・掃除ジョブを持たない）。

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::OfficeError;

/// WOPI 準拠のロック TTL（30 分）。Collabora は期限内に REFRESH_LOCK で延長し続ける。
const LOCK_TTL: &str = "30 minutes";

/// 現在有効なロック（Task 11.8 の「セッション中か」判定にも使う公開型）。
#[derive(Debug, Clone)]
pub struct LockInfo {
    /// WOPI クライアント（Collabora）が発行したロック識別子。
    pub lock_id: String,
    /// ロックを取得した実行主体（FGA subject 文字列・情報表示/監査用）。
    pub locked_by: String,
    pub expires_at: DateTime<Utc>,
}

/// 現在有効なロックを返す（期限切れは無いものとして扱う＝lazy 解放）。
///
/// Task 11.8（AI Office 編集）が「編集セッション中か」の判定に使う公開関数。
pub async fn current_lock(
    pool: &PgPool,
    tenant_id: &str,
    file_id: Uuid,
) -> Result<Option<LockInfo>, OfficeError> {
    // lazy 解放: 期限切れ行はこのアクセスで削除する（掃除ジョブ不要）。
    sqlx::query(
        "DELETE FROM office_lock WHERE file_id = $1 AND tenant_id = $2 AND expires_at <= now()",
    )
    .bind(file_id)
    .bind(tenant_id)
    .execute(pool)
    .await?;
    let row: Option<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT lock_id, locked_by, expires_at FROM office_lock \
         WHERE file_id = $1 AND tenant_id = $2 AND expires_at > now()",
    )
    .bind(file_id)
    .bind(tenant_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(lock_id, locked_by, expires_at)| LockInfo {
        lock_id,
        locked_by,
        expires_at,
    }))
}

/// LOCK: ロックを取得する（同一 lock_id は延長・期限切れは奪取可）。
///
/// 競合（他者の有効ロックが存在）は [`OfficeError::LockConflict`]＝409 で現 lock_id を返す。
pub(crate) async fn lock(
    pool: &PgPool,
    tenant_id: &str,
    file_id: Uuid,
    lock_id: &str,
    locked_by: &str,
) -> Result<(), OfficeError> {
    // upsert: 未ロック（行なし）/ 期限切れ / 同一 lock_id（＝延長）のみ書き換える。
    // tenant_id 条件を UPDATE 側にも課し、隔離境界を SQL レベルで強制する。
    let updated: Option<(String,)> = sqlx::query_as(
        "INSERT INTO office_lock (file_id, lock_id, locked_by, tenant_id, expires_at) \
         VALUES ($1, $2, $3, $4, now() + $5::interval) \
         ON CONFLICT (file_id) DO UPDATE \
         SET lock_id = EXCLUDED.lock_id, locked_by = EXCLUDED.locked_by, \
             tenant_id = EXCLUDED.tenant_id, expires_at = EXCLUDED.expires_at \
         WHERE office_lock.expires_at <= now() \
            OR (office_lock.tenant_id = EXCLUDED.tenant_id \
                AND office_lock.lock_id = EXCLUDED.lock_id) \
         RETURNING lock_id",
    )
    .bind(file_id)
    .bind(lock_id)
    .bind(locked_by)
    .bind(tenant_id)
    .bind(LOCK_TTL)
    .fetch_optional(pool)
    .await?;
    if updated.is_some() {
        return Ok(());
    }
    conflict(pool, tenant_id, file_id).await
}

/// UNLOCK: lock_id が一致する有効ロックを解除する。不一致・無ロックは 409。
pub(crate) async fn unlock(
    pool: &PgPool,
    tenant_id: &str,
    file_id: Uuid,
    lock_id: &str,
) -> Result<(), OfficeError> {
    let deleted = sqlx::query(
        "DELETE FROM office_lock \
         WHERE file_id = $1 AND tenant_id = $2 AND lock_id = $3 AND expires_at > now()",
    )
    .bind(file_id)
    .bind(tenant_id)
    .bind(lock_id)
    .execute(pool)
    .await?
    .rows_affected();
    if deleted > 0 {
        return Ok(());
    }
    conflict(pool, tenant_id, file_id).await
}

/// REFRESH_LOCK: lock_id が一致する有効ロックの期限を延長する。不一致・無ロックは 409。
pub(crate) async fn refresh(
    pool: &PgPool,
    tenant_id: &str,
    file_id: Uuid,
    lock_id: &str,
) -> Result<(), OfficeError> {
    let updated = sqlx::query(
        "UPDATE office_lock SET expires_at = now() + $4::interval \
         WHERE file_id = $1 AND tenant_id = $2 AND lock_id = $3 AND expires_at > now()",
    )
    .bind(file_id)
    .bind(tenant_id)
    .bind(lock_id)
    .bind(LOCK_TTL)
    .execute(pool)
    .await?
    .rows_affected();
    if updated > 0 {
        return Ok(());
    }
    conflict(pool, tenant_id, file_id).await
}

/// PutFile の書込前検証: 有効ロックがあるなら X-WOPI-Lock の一致を要求する。
///
/// 未ロック時は許可（Collabora の初回保存互換・PutFile 側仕様）。期限切れロックは
/// [`current_lock`] が無視/削除するため、自然に「未ロック」として通る。
pub(crate) async fn check_write_lock(
    pool: &PgPool,
    tenant_id: &str,
    file_id: Uuid,
    provided: Option<&str>,
) -> Result<(), OfficeError> {
    match current_lock(pool, tenant_id, file_id).await? {
        None => Ok(()),
        Some(cur) if provided == Some(cur.lock_id.as_str()) => Ok(()),
        Some(cur) => Err(OfficeError::LockConflict {
            current_lock_id: cur.lock_id,
        }),
    }
}

/// 現在の（有効な）lock_id を添えて 409 を返す（WOPI 準拠・無ロック起因は空文字）。
async fn conflict(pool: &PgPool, tenant_id: &str, file_id: Uuid) -> Result<(), OfficeError> {
    let current = current_lock(pool, tenant_id, file_id)
        .await?
        .map(|l| l.lock_id)
        .unwrap_or_default();
    Err(OfficeError::LockConflict {
        current_lock_id: current,
    })
}
