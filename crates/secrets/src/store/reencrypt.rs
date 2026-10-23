//! 旧版（AAD なし・`enc_version = 0`）の暗号文を現行版（行束縛）へ再暗号化する（#814）。
//!
//! 経路は 2 つ:
//! - **遅延**: [`SecretStore::resolve`](super::SecretStore::resolve) が旧版行を復号した直後に
//!   [`upgrade_row`] で書き戻す（使われているシークレットは初回利用で移行される）。
//! - **一括**: [`reencrypt_legacy_secrets`]（`shiki-admin secrets-reencrypt`）。使われない行も移す。
//!
//! どちらも **DEK は元の行のものを再利用**し（新しい nonce で平文を包み直すだけ・KMS の wrap 不要）、
//! `encrypted_dek` は書き換えない。UPDATE は「旧版のまま・nonce が読んだ時点と同じ」行に限る
//! 条件付きなので、並行したローテーション（新 nonce・新版で書く）を古い平文で上書きしない。
//! 行単位なので途中で止めても既に移した行・未移行の行の双方が復号可能なまま（冪等・再実行で収束）。

use sqlx::PgPool;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::key_provider::{KeyProvider, WrappedKey};
use crate::row_aad::{EncVersion, RowBinding};
use crate::{map_db, SecretError};

/// 一括再暗号化の結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReencryptReport {
    /// 走査した旧版行。
    pub scanned: u64,
    /// 現行版へ書き換えた行。
    pub upgraded: u64,
    /// 並行更新（ローテーション等）で既に旧版でなくなっていた行。
    pub skipped: u64,
    /// 復号できなかった行（鍵不一致・改竄）。**書き換えず残す**（ログに tenant/id のみ）。
    pub failed: u64,
}

/// 旧版行 1 件を現行版へ書き戻す。書き換えたら `true`（既に旧版でなければ `false`）。
pub(super) async fn upgrade_row(
    db: &PgPool,
    binding: RowBinding<'_>,
    dek: &[u8],
    plaintext: &[u8],
    old_nonce: &[u8],
) -> Result<bool, SecretError> {
    let sealed = binding.seal(dek, plaintext)?;
    let done = sqlx::query(
        "UPDATE secret SET ciphertext = $5, nonce = $6, enc_version = $7 \
         WHERE tenant_id = $1 AND id = $2 AND org = $3 AND enc_version = 0 AND nonce = $4",
    )
    .bind(binding.tenant_id)
    .bind(binding.id)
    .bind(binding.org)
    .bind(old_nonce)
    .bind(&sealed.ciphertext)
    .bind(&sealed.nonce)
    .bind(EncVersion::CURRENT.as_db())
    .execute(db)
    .await
    .map_err(map_db)?;
    Ok(done.rows_affected() == 1)
}

/// 旧版行の件数（dry-run 用）。
pub async fn count_legacy_secrets(db: &PgPool) -> Result<i64, SecretError> {
    sqlx::query_scalar("SELECT count(*) FROM secret WHERE enc_version = 0")
        .fetch_one(db)
        .await
        .map_err(map_db)
}

#[derive(sqlx::FromRow)]
struct LegacyRow {
    tenant_id: String,
    id: Uuid,
    org: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
    encrypted_dek: Vec<u8>,
    dek_nonce: Vec<u8>,
    key_provider: String,
}

/// 全テナントの旧版行を現行版へ再暗号化する（運用 CLI 専用・平文は返さずメモリ上でゼロ化）。
///
/// `batch_size` 件ずつ `(tenant_id, id)` の keyset で進むため、復号できず残した行を再走査しない。
pub async fn reencrypt_legacy_secrets(
    db: &PgPool,
    key_provider: &dyn KeyProvider,
    batch_size: i64,
) -> Result<ReencryptReport, SecretError> {
    let mut report = ReencryptReport::default();
    let mut after: Option<(String, Uuid)> = None;
    loop {
        let rows: Vec<LegacyRow> = sqlx::query_as(
            "SELECT tenant_id, id, org, ciphertext, nonce, encrypted_dek, dek_nonce, key_provider \
             FROM secret WHERE enc_version = 0 \
               AND ($1::text IS NULL OR (tenant_id, id) > ($1, $2)) \
             ORDER BY tenant_id, id LIMIT $3",
        )
        .bind(after.as_ref().map(|(t, _)| t.as_str()))
        .bind(after.as_ref().map(|(_, id)| *id))
        .bind(batch_size.max(1))
        .fetch_all(db)
        .await
        .map_err(map_db)?;
        let Some(last) = rows.last() else { break };
        after = Some((last.tenant_id.clone(), last.id));
        for row in &rows {
            report.scanned += 1;
            match reencrypt_one(db, key_provider, row).await {
                Ok(true) => report.upgraded += 1,
                Ok(false) => report.skipped += 1,
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(tenant_id = %row.tenant_id, secret_id = %row.id, error = %e, "旧版シークレットを再暗号化できません（行は変更しません）");
                }
            }
        }
    }
    Ok(report)
}

async fn reencrypt_one(
    db: &PgPool,
    key_provider: &dyn KeyProvider,
    row: &LegacyRow,
) -> Result<bool, SecretError> {
    let binding = RowBinding {
        tenant_id: &row.tenant_id,
        org: &row.org,
        id: row.id,
    };
    let dek = key_provider
        .unwrap(&WrappedKey {
            encrypted_dek: row.encrypted_dek.clone(),
            nonce: row.dek_nonce.clone(),
            provider_id: row.key_provider.clone(),
        })
        .await?;
    let mut plaintext = binding.open(
        &dek.0,
        EncVersion::Legacy.as_db(),
        &row.nonce,
        &row.ciphertext,
    )?;
    let upgraded = upgrade_row(db, binding, &dek.0, &plaintext, &row.nonce).await;
    plaintext.zeroize();
    upgraded
}
