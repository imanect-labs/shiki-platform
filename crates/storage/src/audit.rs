//! 監査ログ基盤（Task 1.9）。
//!
//! 全データ操作と認可判定（who/what/object/decision/trace_id）を append-only に記録する。
//! `prev_hash`/`entry_hash` でハッシュチェーンの種を蒔くが、**改竄耐性は主張しない**
//! （アプリ経路の追記のみ・PIT-12）。trace_id は OTel と共有し Langfuse 突合の土台とする。

use authz::AuthContext;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};

use crate::error::StorageError;

/// 認可判定の結果（記録用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Deny => "deny",
        }
    }
}

/// 1 件の監査エントリ（記録対象の正規化フィールド）。
pub struct AuditEntry<'a> {
    pub action: &'a str,
    pub object_type: &'a str,
    pub object_id: &'a str,
    pub decision: Decision,
    pub trace_id: Option<&'a str>,
    pub metadata: Value,
}

/// 監査レコーダ。txn 内（書込系）でも単独（読取/deny 系）でも記録できる。
#[derive(Clone)]
pub struct AuditRecorder {
    db: PgPool,
}

impl AuditRecorder {
    pub fn new(db: PgPool) -> Self {
        AuditRecorder { db }
    }

    /// 専用トランザクションで 1 件記録する（読取/deny 経路用）。**チェーンしない**:
    /// 読取（URL 発行・メタ取得）や deny まで per-org の単一ロックに乗せると、読取主体の org の
    /// スループットが監査 insert の直列実行で頭打ちになるため、これらは prev_hash 連結も
    /// advisory ロックも行わず並行記録する（改竄チェーンは実データ変更操作のみ・PIT-12 は honest）。
    pub async fn record(
        &self,
        ctx: &AuthContext,
        entry: AuditEntry<'_>,
    ) -> Result<(), StorageError> {
        let mut tx = self.db.begin().await?;
        record_on(&mut tx, ctx, entry, Chain::No).await?;
        tx.commit().await?;
        Ok(())
    }
}

/// 監査エントリをハッシュチェーンに連結するか。実データ変更操作のみ `Yes`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    Yes,
    No,
}

/// 既存のトランザクション上で 1 件記録する（書込系は同一 txn で原子的に残す）。
///
/// `chain == Yes` のときのみ per-org の advisory xact ロックを取り、`prev_hash` で直前エントリに
/// 連結する（ハッシュチェーン）。これは finalize/update/delete/restore など**実データ変更操作**に
/// 限定し、読取/URL 発行/deny は `Chain::No` で並行記録してロック競合を避ける。
pub async fn record_on(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    entry: AuditEntry<'_>,
    chain: Chain,
) -> Result<(), StorageError> {
    let chained = chain == Chain::Yes;
    // チェーン対象のみ org 単位で直列化し、直前の **chained 行** に連結する
    // （未チェーンの読取/deny 行は跨いで無視する）。
    let prev_hash: Option<String> = if chained {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(&ctx.org)
            .execute(&mut *conn)
            .await?;
        sqlx::query_scalar(
            "SELECT entry_hash FROM audit_log \
             WHERE org = $1 AND chained ORDER BY id DESC LIMIT 1",
        )
        .bind(&ctx.org)
        .fetch_optional(&mut *conn)
        .await?
        .flatten()
    } else {
        None
    };

    let metadata_str = entry.metadata.to_string();
    let entry_hash = compute_entry_hash(prev_hash.as_deref(), ctx, &entry, &metadata_str);

    sqlx::query(
        "INSERT INTO audit_log \
         (tenant_id, org, actor, action, object_type, object_id, decision, trace_id, metadata, chained, prev_hash, entry_hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, $11, $12)",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(&ctx.principal.id)
    .bind(entry.action)
    .bind(entry.object_type)
    .bind(entry.object_id)
    .bind(entry.decision.as_str())
    .bind(entry.trace_id)
    .bind(&metadata_str)
    .bind(chained)
    .bind(prev_hash.as_deref())
    .bind(&entry_hash)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// `entry_hash = sha256(prev_hash || 正規化フィールド)`。決定的（テスト可能）。
fn compute_entry_hash(
    prev_hash: Option<&str>,
    ctx: &AuthContext,
    entry: &AuditEntry<'_>,
    metadata_str: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.unwrap_or("").as_bytes());
    for field in [
        ctx.tenant_id.as_str(),
        ctx.org.as_str(),
        ctx.principal.id.as_str(),
        entry.action,
        entry.object_type,
        entry.object_id,
        entry.decision.as_str(),
        entry.trace_id.unwrap_or(""),
        metadata_str,
    ] {
        hasher.update(b"|");
        hasher.update(field.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use authz::Principal;
    use serde_json::json;

    fn ctx() -> AuthContext {
        AuthContext::new(
            Principal {
                id: "alice".into(),
                email: None,
                groups: vec![],
                dept: None,
                tenant_id: None,
            },
            "acme".into(),
            "default".into(),
        )
    }

    fn entry<'a>(action: &'a str) -> AuditEntry<'a> {
        AuditEntry {
            action,
            object_type: "file",
            object_id: "f1",
            decision: Decision::Allow,
            trace_id: Some("trace-1"),
            metadata: json!({}),
        }
    }

    #[test]
    fn entry_hash_is_deterministic() {
        let c = ctx();
        let h1 = compute_entry_hash(Some("prev"), &c, &entry("file.upload.finalize"), "{}");
        let h2 = compute_entry_hash(Some("prev"), &c, &entry("file.upload.finalize"), "{}");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn entry_hash_chains_on_prev() {
        let c = ctx();
        let a = compute_entry_hash(None, &c, &entry("file.delete"), "{}");
        let b = compute_entry_hash(Some(&a), &c, &entry("file.delete"), "{}");
        // prev が変われば hash も変わる（チェーンが効く）。
        assert_ne!(a, b);
    }

    #[test]
    fn entry_hash_changes_with_decision() {
        let c = ctx();
        let allow = entry("file.download_url.issue");
        let mut deny = entry("file.download_url.issue");
        deny.decision = Decision::Deny;
        assert_ne!(
            compute_entry_hash(None, &c, &allow, "{}"),
            compute_entry_hash(None, &c, &deny, "{}")
        );
    }
}
