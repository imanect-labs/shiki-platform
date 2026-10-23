//! 監査ログ基盤（Task 1.9）。
//!
//! 全データ操作と認可判定（who/what/object/decision/trace_id）を append-only に記録する。
//! 実データを変える操作の行は `prev_hash`/`entry_hash` でハッシュチェーンに連結し、
//! [`crate::audit_chain`] で先頭からたどって検証できる（#828）。鍵も外部アンカーも無いので、
//! DB に書き込める者がチェーンを丸ごと計算し直す改竄は検知できない（PIT-12）。
//! trace_id は OTel と共有し Langfuse 突合の土台とする。

use authz::AuthContext;
use chrono::{DateTime, SubsecRound, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};

pub use crate::audit_action::{AuditAction, AuditCategory};
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
    pub action: AuditAction,
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

    /// 同じ接続プールで監査ログを読む窓口（#634）。書き込みとは型を分ける。
    pub fn viewer(&self) -> crate::audit_view::AuditViewer {
        crate::audit_view::AuditViewer::new(self.db.clone())
    }

    /// 同じ接続プールで保持期間を扱う窓口（#831）。
    pub fn retention(&self) -> crate::audit_retention::AuditRetention {
        crate::audit_retention::AuditRetention::new(self.db.clone())
    }

    /// 同じ接続プールでハッシュチェーンを検証する窓口（#828）。
    pub fn chain_verifier(&self) -> crate::audit_chain::AuditChainVerifier {
        crate::audit_chain::AuditChainVerifier::new(self.db.clone())
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
    // チェーン対象のみ **tenant_id + org 単位**で直列化し、直前の **chained 行** に連結する
    // （未チェーンの読取/deny 行は跨いで無視する）。SaaS の共用プール Postgres でも
    // ハッシュチェーンをテナント内に閉じる（同一 org を複数テナントが共有しても越境連結しない・SAAS.1）。
    let prev_hash: Option<String> =
        if chained {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
                .bind(format!("{}|{}", ctx.tenant_id, ctx.org))
                .execute(&mut *conn)
                .await?;
            let last: Option<Option<String>> = sqlx::query_scalar(
                "SELECT entry_hash FROM audit_log \
             WHERE tenant_id = $1 AND org = $2 AND chained ORDER BY id DESC LIMIT 1",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .fetch_optional(&mut *conn)
            .await?;
            match last {
                Some(hash) => hash,
                // 連結行が 1 件も無い: 保持期間で全部消したチェーンなら、消した最後の行（境目）に
                // つなぐ（#831）。境目も無ければチェーンの先頭（prev なし）。
                None => sqlx::query_scalar(
                    "SELECT anchor_hash FROM audit_chain_anchor WHERE tenant_id = $1 AND org = $2",
                )
                .bind(&ctx.tenant_id)
                .bind(&ctx.org)
                .fetch_optional(&mut *conn)
                .await?,
            }
        } else {
            None
        };

    // metadata は serde_json の既定（キーを並べ替えた Map・空白なし）で文字列にする。jsonb から
    // 読み戻して同じ形に直せるので、検証時に同じ文字列を作り直せる。
    let metadata_str = entry.metadata.to_string();
    // 作成時刻もハッシュに入れる（#828）。DB の既定値ではなくここで決め、マイクロ秒に丸めて
    // そのまま保存する（timestamptz の精度と揃え、検証で同じ値を作り直せるようにする）。
    let created_at = Utc::now().trunc_subsecs(6);
    let entry_hash = entry_hash(&HashInput {
        version: HASH_VERSION,
        prev_hash: prev_hash.as_deref(),
        tenant_id: &ctx.tenant_id,
        org: &ctx.org,
        actor: &ctx.principal.id,
        action: entry.action.as_str(),
        object_type: entry.object_type,
        object_id: entry.object_id,
        decision: entry.decision.as_str(),
        trace_id: entry.trace_id,
        metadata: &metadata_str,
        created_at,
    });

    sqlx::query(
        "INSERT INTO audit_log \
         (tenant_id, org, actor, action, object_type, object_id, decision, trace_id, metadata, chained, prev_hash, entry_hash, created_at, hash_version) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, $11, $12, $13, $14)",
    )
    .bind(&ctx.tenant_id)
    .bind(&ctx.org)
    .bind(&ctx.principal.id)
    .bind(entry.action.as_str())
    .bind(entry.object_type)
    .bind(entry.object_id)
    .bind(entry.decision.as_str())
    .bind(entry.trace_id)
    .bind(&metadata_str)
    .bind(chained)
    .bind(prev_hash.as_deref())
    .bind(&entry_hash)
    .bind(created_at)
    .bind(HASH_VERSION)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// 新しく記録する行のハッシュの版（#828）。
///
/// - 1: `sha256(prev_hash | tenant_id | org | actor | action | object_type | object_id | decision | trace_id | metadata)`
///   （#828 より前の行。作成時刻を含まない）
/// - 2: 1 の末尾に `| v2 | created_at（UNIX マイクロ秒）` を足したもの
pub const HASH_VERSION: i16 = 2;

/// ハッシュの入力（記録と検証で同じものを使う）。
#[derive(Debug, Clone, Copy)]
pub struct HashInput<'a> {
    pub version: i16,
    pub prev_hash: Option<&'a str>,
    pub tenant_id: &'a str,
    pub org: &'a str,
    pub actor: &'a str,
    pub action: &'a str,
    pub object_type: &'a str,
    pub object_id: &'a str,
    pub decision: &'a str,
    pub trace_id: Option<&'a str>,
    /// metadata の文字列（serde_json の既定の表記）。
    pub metadata: &'a str,
    pub created_at: DateTime<Utc>,
}

/// `entry_hash`。決定的（テスト可能）。版 1 は作成時刻を含まない。
pub fn entry_hash(h: &HashInput<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(h.prev_hash.unwrap_or("").as_bytes());
    for field in [
        h.tenant_id,
        h.org,
        h.actor,
        h.action,
        h.object_type,
        h.object_id,
        h.decision,
        h.trace_id.unwrap_or(""),
        h.metadata,
    ] {
        hasher.update(b"|");
        hasher.update(field.as_bytes());
    }
    if h.version >= 2 {
        hasher.update(b"|v2|");
        hasher.update(h.created_at.timestamp_micros().to_string().as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(version: i16) -> HashInput<'static> {
        HashInput {
            version,
            prev_hash: Some("prev"),
            tenant_id: "default",
            org: "acme",
            actor: "alice",
            action: AuditAction::FileUploadFinalize.as_str(),
            object_type: "file",
            object_id: "f1",
            decision: Decision::Allow.as_str(),
            trace_id: Some("trace-1"),
            metadata: "{}",
            created_at: DateTime::from_timestamp_micros(1_792_000_000_123_456).unwrap_or_default(),
        }
    }

    #[test]
    fn entry_hash_is_deterministic() {
        assert_eq!(entry_hash(&input(2)), entry_hash(&input(2)));
        assert_eq!(entry_hash(&input(2)).len(), 64);
    }

    #[test]
    fn entry_hash_chains_on_prev_and_covers_fields() {
        let base = entry_hash(&input(2));
        let mut h = input(2);
        h.prev_hash = None;
        // prev が変われば hash も変わる（チェーンが効く）。
        assert_ne!(entry_hash(&h), base);
        let mut h = input(2);
        h.decision = Decision::Deny.as_str();
        assert_ne!(entry_hash(&h), base);
        let mut h = input(2);
        h.metadata = r#"{"k":1}"#;
        assert_ne!(entry_hash(&h), base);
    }

    #[test]
    fn v2_covers_created_at_but_v1_does_not() {
        let mut later = input(2);
        later.created_at += chrono::Duration::microseconds(1);
        assert_ne!(
            entry_hash(&later),
            entry_hash(&input(2)),
            "時刻の書き換えを検知する"
        );
        let mut later_v1 = input(1);
        later_v1.created_at += chrono::Duration::seconds(10);
        assert_eq!(
            entry_hash(&later_v1),
            entry_hash(&input(1)),
            "版 1 は時刻を含まない"
        );
        assert_ne!(entry_hash(&input(1)), entry_hash(&input(2)));
    }
}
