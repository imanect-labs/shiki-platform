//! `SecretStore` — シークレットの単一チョークポイント（write-only / use-only）。
//!
//! **平文を読み返すメソッドは存在しない**。[`resolve`](SecretStore::resolve) だけが実行時に
//! 平文を返し（能力ゲートウェイ内から呼ぶ）、その解決イベントを毎回監査する。

use std::sync::Arc;

use authz::{AuthContext, AuthzClient, Consistency, Relation};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use storage::audit::{AuditEntry, AuditRecorder, Decision};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::binding::DestinationBinding;
use crate::key_provider::{KeyProvider, WrappedKey};
use crate::{crypto, map_db, SecretError};

/// 参照名の上限長。
const MAX_NAME_LEN: usize = 128;
/// 平文の上限（防御的・トークン/PEM 想定）。
const MAX_PLAINTEXT_BYTES: usize = 64 * 1024;

/// 新規シークレットの入力（平文はここでのみ受け、保存後は二度と読めない）。
#[derive(Debug, Clone)]
pub struct NewSecret {
    pub name: String,
    pub plaintext: Vec<u8>,
    /// 添付を許可する宛先ホスト（完全一致 or `*.suffix`）。
    pub allowed_hosts: Vec<String>,
}

/// シークレットのメタデータ（**平文を含まない**・一覧/表示用）。
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SecretMeta {
    pub id: Uuid,
    pub name: String,
    pub owner: String,
    pub allowed_hosts: Vec<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 実行時に解決した平文（能力ゲートウェイ内でのみ扱い、レダクタへ登録する）。
pub struct ResolvedSecret {
    pub id: Uuid,
    pub name: String,
    pub plaintext: Vec<u8>,
    pub binding: DestinationBinding,
}

/// シークレットのデータチョークポイント。
#[derive(Clone)]
pub struct SecretStore {
    db: PgPool,
    authz: Arc<dyn AuthzClient>,
    audit: AuditRecorder,
    key_provider: Arc<dyn KeyProvider>,
}

/// resolve 用の暗号素材行（平文は保持しない）。
#[derive(sqlx::FromRow)]
struct SecretCipherRow {
    id: Uuid,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
    encrypted_dek: Vec<u8>,
    dek_nonce: Vec<u8>,
    key_provider: String,
    allowed_hosts: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct SecretRow {
    id: Uuid,
    name: String,
    owner: String,
    allowed_hosts: Vec<String>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl SecretStore {
    pub fn new(
        db: PgPool,
        authz: Arc<dyn AuthzClient>,
        key_provider: Arc<dyn KeyProvider>,
    ) -> Self {
        let audit = AuditRecorder::new(db.clone());
        SecretStore {
            db,
            authz,
            audit,
            key_provider,
        }
    }

    /// シークレットを登録する（envelope 暗号化・作成者を owner タプルで付与）。
    pub async fn create(
        &self,
        ctx: &AuthContext,
        input: NewSecret,
        trace_id: Option<&str>,
    ) -> Result<SecretMeta, SecretError> {
        let name = validate_name(&input.name)?;
        if input.plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(SecretError::Invalid("plaintext が大きすぎます".into()));
        }
        // 自動レダクト不能な短い値（< MIN_LEN）は書込時に拒否する。登録できても解決時に
        // レダクタが取りこぼし、HTTP エラー/run 出力/ログへ平文が漏れ得るため（fail-closed）。
        if input.plaintext.len() < crate::redact::MIN_LEN {
            return Err(SecretError::Invalid(
                "plaintext が短すぎます（自動レダクト不能）".into(),
            ));
        }
        let hosts = normalize_hosts(&input.allowed_hosts)?;

        // envelope: DEK を作り平文を暗号化 → DEK をマスターキーで包む。
        let dek = crypto::KeyGuard(crypto::generate_key());
        let sealed = crypto::seal(&dek.0, &input.plaintext)?;
        let wrapped = self.key_provider.wrap(&dek.0).await?;

        let row: SecretRow = sqlx::query_as(
            "INSERT INTO secret \
             (tenant_id, org, name, owner, allowed_hosts, ciphertext, nonce, \
              encrypted_dek, dek_nonce, key_provider) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             RETURNING id, name, owner, allowed_hosts, version, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(name)
        .bind(&ctx.principal.id)
        .bind(&hosts)
        .bind(&sealed.ciphertext)
        .bind(&sealed.nonce)
        .bind(&wrapped.encrypted_dek)
        .bind(&wrapped.nonce)
        .bind(wrapped.provider_id.as_str())
        .fetch_one(&self.db)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                SecretError::Conflict(format!("name '{name}' は既に存在します"))
            }
            _ => map_db(e),
        })?;

        let id = row.id;
        let obj = ctx.ns().secret(&id.to_string());
        if let Err(e) = self
            .authz
            .write_tuple(&ctx.subject(), Relation::Owner, &obj)
            .await
        {
            let _ = sqlx::query("DELETE FROM secret WHERE tenant_id = $1 AND id = $2")
                .bind(&ctx.tenant_id)
                .bind(id)
                .execute(&self.db)
                .await;
            return Err(SecretError::Internal(format!("owner tuple: {e}")));
        }
        self.record_audit(
            ctx,
            "secret.create",
            &id.to_string(),
            trace_id,
            json!({ "name": name }),
        )
        .await?;
        Ok(to_meta(row))
    }

    /// 平文をローテーションする（owner 権限・新しい DEK で再暗号化・version +1）。
    pub async fn rotate(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        new_plaintext: Vec<u8>,
        trace_id: Option<&str>,
    ) -> Result<SecretMeta, SecretError> {
        if new_plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(SecretError::Invalid("plaintext が大きすぎます".into()));
        }
        if new_plaintext.len() < crate::redact::MIN_LEN {
            return Err(SecretError::Invalid(
                "plaintext が短すぎます（自動レダクト不能）".into(),
            ));
        }
        self.require(ctx, id, Relation::Owner, "secret.rotate", trace_id)
            .await?;
        let dek = crypto::KeyGuard(crypto::generate_key());
        let sealed = crypto::seal(&dek.0, &new_plaintext)?;
        let wrapped = self.key_provider.wrap(&dek.0).await?;
        let row: Option<SecretRow> = sqlx::query_as(
            "UPDATE secret SET ciphertext = $3, nonce = $4, encrypted_dek = $5, \
             dek_nonce = $6, key_provider = $7, version = version + 1, updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 \
             RETURNING id, name, owner, allowed_hosts, version, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(&sealed.ciphertext)
        .bind(&sealed.nonce)
        .bind(&wrapped.encrypted_dek)
        .bind(&wrapped.nonce)
        .bind(wrapped.provider_id.as_str())
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let row = row.ok_or(SecretError::NotFound)?;
        self.record_audit(
            ctx,
            "secret.rotate",
            &id.to_string(),
            trace_id,
            json!({ "version": row.version }),
        )
        .await?;
        Ok(to_meta(row))
    }

    /// 宛先束縛を更新する（owner 権限）。
    pub async fn update_binding(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        allowed_hosts: Vec<String>,
        trace_id: Option<&str>,
    ) -> Result<SecretMeta, SecretError> {
        self.require(ctx, id, Relation::Owner, "secret.binding.update", trace_id)
            .await?;
        let hosts = normalize_hosts(&allowed_hosts)?;
        let row: Option<SecretRow> = sqlx::query_as(
            "UPDATE secret SET allowed_hosts = $3, updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 \
             RETURNING id, name, owner, allowed_hosts, version, created_at, updated_at",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .bind(&hosts)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let row = row.ok_or(SecretError::NotFound)?;
        self.record_audit(
            ctx,
            "secret.binding.update",
            &id.to_string(),
            trace_id,
            json!({}),
        )
        .await?;
        Ok(to_meta(row))
    }

    /// 削除する（owner 権限・FGA タプルも撤去）。
    pub async fn delete(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), SecretError> {
        let obj = self
            .require(ctx, id, Relation::Owner, "secret.delete", trace_id)
            .await?;
        let deleted = sqlx::query("DELETE FROM secret WHERE tenant_id = $1 AND id = $2")
            .bind(&ctx.tenant_id)
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(map_db)?;
        if deleted.rows_affected() == 0 {
            return Err(SecretError::NotFound);
        }
        let _ = self.authz.delete_object_tuples(&obj).await;
        self.record_audit(ctx, "secret.delete", &id.to_string(), trace_id, json!({}))
            .await
    }

    /// 自分が owner のシークレット一覧（**参照名のみ**・平文は返さない）。
    pub async fn list_mine(&self, ctx: &AuthContext) -> Result<Vec<SecretMeta>, SecretError> {
        let rows: Vec<SecretRow> = sqlx::query_as(
            "SELECT id, name, owner, allowed_hosts, version, created_at, updated_at \
             FROM secret WHERE tenant_id = $1 AND org = $2 AND owner = $3 ORDER BY name",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(&ctx.principal.id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows.into_iter().map(to_meta).collect())
    }

    /// メタデータを取得する（can_use 権限・**平文は含まない**）。
    pub async fn get_meta(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<SecretMeta, SecretError> {
        self.require(ctx, id, Relation::CanUse, "secret.get_meta", trace_id)
            .await?;
        let row: Option<SecretRow> = sqlx::query_as(
            "SELECT id, name, owner, allowed_hosts, version, created_at, updated_at \
             FROM secret WHERE tenant_id = $1 AND id = $2",
        )
        .bind(&ctx.tenant_id)
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        row.map(to_meta).ok_or(SecretError::NotFound)
    }

    /// **実行時解決**（can_use 権限・平文を返す唯一の経路・毎回監査）。
    ///
    /// 能力ゲートウェイ（http.request / script の http）内からのみ呼ぶ。`destination_host` を渡すと
    /// **平文を復号する前に宛先束縛（allowed_hosts）で検証**し、許可外ホストは平文を出さずに
    /// `DestinationDenied` で失敗させる（fail-closed をこのチョークポイントに寄せる・PIT-36）。
    /// 返した平文はレダクタへ登録して使う。
    pub async fn resolve(
        &self,
        ctx: &AuthContext,
        name: &str,
        destination_host: Option<&str>,
        trace_id: Option<&str>,
    ) -> Result<ResolvedSecret, SecretError> {
        // 参照名 → 暗号化された素材（tenant＋org スコープ）。同一 tenant 内でも別 org の
        // シークレットを注入させない（org 境界・ctx.org でスコープ）。
        let row: Option<SecretCipherRow> = sqlx::query_as(
            "SELECT id, ciphertext, nonce, encrypted_dek, dek_nonce, key_provider, allowed_hosts \
                 FROM secret WHERE tenant_id = $1 AND org = $2 AND name = $3",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        let Some(row) = row else {
            return Err(SecretError::NotFound);
        };
        let id = row.id;

        // can_use 権限を確認（無ければ解決拒否＋監査 deny）。
        self.require(ctx, id, Relation::CanUse, "secret.resolve", trace_id)
            .await?;

        // 宛先束縛を**復号前に**検証する（許可外ホストへ平文を一切出さない・監査 deny）。
        if let Some(host) = destination_host {
            let binding = DestinationBinding::new(row.allowed_hosts.clone());
            if !binding.allows(host) {
                let _ = self
                    .record_audit(
                        ctx,
                        "secret.destination_denied",
                        &id.to_string(),
                        trace_id,
                        json!({ "name": name, "host": host }),
                    )
                    .await;
                return Err(SecretError::DestinationDenied(host.to_string()));
            }
        }

        // DEK を解いて平文を復号。
        let wrapped = WrappedKey {
            encrypted_dek: row.encrypted_dek,
            nonce: row.dek_nonce,
            provider_id: row.key_provider,
        };
        let dek = self.key_provider.unwrap(&wrapped).await?;
        let plaintext = crypto::open(&dek.0, &row.nonce, &row.ciphertext)?;

        // 解決イベントを毎回監査（run 相関は trace_id）。
        self.record_audit(
            ctx,
            "secret.resolve",
            &id.to_string(),
            trace_id,
            json!({ "name": name }),
        )
        .await?;

        Ok(ResolvedSecret {
            id,
            name: name.to_string(),
            plaintext,
            binding: DestinationBinding::new(row.allowed_hosts),
        })
    }

    /// secret への relation を要求する（不足は監査 deny＋Forbidden・剥奪即時反映）。
    async fn require(
        &self,
        ctx: &AuthContext,
        id: Uuid,
        relation: Relation,
        action: &str,
        trace_id: Option<&str>,
    ) -> Result<authz::FgaObject, SecretError> {
        let obj = ctx.ns().secret(&id.to_string());
        let ok = self
            .authz
            .check(
                &ctx.subject(),
                relation,
                &obj,
                Consistency::HigherConsistency,
            )
            .await
            .map_err(|e| SecretError::Internal(e.to_string()))?;
        if !ok {
            let _ = self
                .audit
                .record(
                    ctx,
                    AuditEntry {
                        action,
                        object_type: "secret",
                        object_id: &id.to_string(),
                        decision: Decision::Deny,
                        trace_id,
                        metadata: json!({ "relation": relation.as_str() }),
                    },
                )
                .await;
            return Err(SecretError::Forbidden);
        }
        Ok(obj)
    }

    async fn record_audit(
        &self,
        ctx: &AuthContext,
        action: &str,
        object_id: &str,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), SecretError> {
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action,
                    object_type: "secret",
                    object_id,
                    decision: Decision::Allow,
                    trace_id,
                    metadata,
                },
            )
            .await
            .map_err(|e| SecretError::Internal(format!("audit: {e}")))
    }
}

fn to_meta(row: SecretRow) -> SecretMeta {
    SecretMeta {
        id: row.id,
        name: row.name,
        owner: row.owner,
        allowed_hosts: row.allowed_hosts,
        version: row.version,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn validate_name(name: &str) -> Result<&str, SecretError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(SecretError::Invalid("name が空です".into()));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(SecretError::Invalid("name が長すぎます".into()));
    }
    Ok(name)
}

/// 宛先ホストを正規化・検証する（小文字化・空要素除去・重複排除）。
fn normalize_hosts(hosts: &[String]) -> Result<Vec<String>, SecretError> {
    let mut out: Vec<String> = Vec::new();
    for h in hosts {
        let h = h.trim().to_ascii_lowercase();
        if h.is_empty() {
            continue;
        }
        // 明らかに不正なホスト（スキーム/パス/空白を含む）は拒否。
        if h.contains('/') || h.contains(' ') || h.contains(':') {
            return Err(SecretError::Invalid(format!("不正な宛先ホスト: {h}")));
        }
        if !out.contains(&h) {
            out.push(h);
        }
    }
    Ok(out)
}
