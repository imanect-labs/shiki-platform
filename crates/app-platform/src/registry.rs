//! 汎用アーティファクト・レジストリ（Task 9.13a・不変 publish）。
//!
//! ミニアプリ（mini_app_code）を内部レジストリへ**不変 publish** する。`artifact_kind` 列を
//! 持つ汎用設計で、skill ストア等へ将来流用できる（新しい配布機構は作らない）。
//! 同一 (tenant, kind, name, version) の再 publish は 409。取り下げは yank（不変性は保つ）。

use authz::AuthContext;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgExecutor, PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{map_db, AppPlatformError};

/// レジストリ登録の 1 行。
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
pub struct RegistryEntry {
    pub id: Uuid,
    pub artifact_kind: String,
    pub name: String,
    pub version: String,
    pub artifact_id: Uuid,
    pub artifact_version: i64,
    pub manifest_digest: String,
    pub publisher: String,
    pub trust_tier: String,
    pub yanked: bool,
    pub created_at: DateTime<Utc>,
}

/// publish の入力。
#[derive(Debug, Clone)]
pub struct NewRegistryEntry<'a> {
    pub artifact_kind: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub artifact_id: Uuid,
    pub artifact_version: i64,
    pub manifest_digest: &'a str,
    pub trust_tier: &'a str,
    pub signature: Option<&'a [u8]>,
}

/// 汎用レジストリ（Postgres 上・不変 publish）。
#[derive(Clone)]
pub struct Registry {
    db: PgPool,
}

impl Registry {
    pub fn new(db: PgPool) -> Self {
        Registry { db }
    }

    /// レジストリへ不変 publish する（同一 name+version は 409）。
    ///
    /// artifact 本体（不変バージョン）は呼び出し側が既に作成済みで、その id/version と
    /// digest を受け取る。authz は artifact 側の owner タプルで担保される（レジストリ行
    /// 自体はテナントスコープの公開台帳）。監査を同じ Tx に載せるときは [`Self::publish_on`]（コードアプリ・GAP-69a）。
    pub async fn publish(
        &self,
        ctx: &AuthContext,
        entry: NewRegistryEntry<'_>,
    ) -> Result<RegistryEntry, AppPlatformError> {
        insert_entry(&self.db, ctx, &entry).await
    }

    /// 呼び出し側の Tx の中で不変 publish する（同一 name+version は 409）。
    ///
    /// 登録と監査（`app.registry.publish`・チェーンにつなぐ）を 1 つの Tx で書き、どちらかだけが残らないようにする
    /// （[`crate::MiniAppCodeStore::publish`]・GAP-69a）。Tx は [`Self::begin`] で開く。
    pub(crate) async fn publish_on(
        &self,
        conn: &mut PgConnection,
        ctx: &AuthContext,
        entry: NewRegistryEntry<'_>,
    ) -> Result<RegistryEntry, AppPlatformError> {
        insert_entry(conn, ctx, &entry).await
    }

    /// レジストリの DB の Tx を開く（[`Self::publish_on`] と監査を同じ Tx で書く）。
    pub(crate) async fn begin(&self) -> Result<Transaction<'static, Postgres>, AppPlatformError> {
        self.db.begin().await.map_err(map_db)
    }

    /// name の最新（未 yank）エントリを引く（インストール時の解決）。
    pub async fn latest(
        &self,
        ctx: &AuthContext,
        artifact_kind: &str,
        name: &str,
    ) -> Result<Option<RegistryEntry>, AppPlatformError> {
        let row: Option<RegistryEntry> = sqlx::query_as(
            "SELECT id, artifact_kind, name, version, artifact_id, artifact_version, \
                    manifest_digest, publisher, trust_tier, yanked, created_at \
             FROM registry_entry \
             WHERE tenant_id = $1 AND artifact_kind = $2 AND name = $3 AND NOT yanked \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(ctx.tenant_id())
        .bind(artifact_kind)
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row)
    }

    /// 特定 version のエントリを引く。
    pub async fn get(
        &self,
        ctx: &AuthContext,
        artifact_kind: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<RegistryEntry>, AppPlatformError> {
        let row: Option<RegistryEntry> = sqlx::query_as(
            "SELECT id, artifact_kind, name, version, artifact_id, artifact_version, \
                    manifest_digest, publisher, trust_tier, yanked, created_at \
             FROM registry_entry \
             WHERE tenant_id = $1 AND artifact_kind = $2 AND name = $3 AND version = $4",
        )
        .bind(ctx.tenant_id())
        .bind(artifact_kind)
        .bind(name)
        .bind(version)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row)
    }

    /// 呼ぶ人の組織が publish したエントリの一覧（新しい順・インストール UI 用）。
    ///
    /// テナントと組織の両方で絞る（組織はテナントの中の隔離の境界・PIT-45 の単一定義・BUG-197。skill の掲載
    /// [`crate::SkillInstallService`] と同じ境界）。名前と版で引く [`Self::latest`]・[`Self::get`] はテナントで
    /// 絞り、インストールは artifact の owner の確かめを通す。
    pub async fn list(
        &self,
        ctx: &AuthContext,
        artifact_kind: &str,
        limit: i64,
    ) -> Result<Vec<RegistryEntry>, AppPlatformError> {
        let rows: Vec<RegistryEntry> = sqlx::query_as(
            "SELECT id, artifact_kind, name, version, artifact_id, artifact_version, \
                    manifest_digest, publisher, trust_tier, yanked, created_at \
             FROM registry_entry \
             WHERE tenant_id = $1 AND org = $2 AND artifact_kind = $3 \
             ORDER BY created_at DESC LIMIT $4",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(artifact_kind)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows)
    }

    /// 呼ぶ人の組織が publish し、今もインストールできるエントリの一覧（新しい順・コードアプリのインストール UI 用）。
    ///
    /// [`Self::list`] と同じくテナントと組織で絞り、取り下げたもの（yank）と、実体の artifact を消したものを除く
    /// （組織の公開アプリの一覧から外す・D-203・GAP-69a）。除くものを SQL で落とすので、取り下げたもの・消したものが
    /// 件数の上限（200）を食わない。skill の一覧（`GET /skills/registry`）は [`Self::list`] のまま（D-203 で skill の登録は
    /// 扱わない）。
    pub async fn list_installable(
        &self,
        ctx: &AuthContext,
        artifact_kind: &str,
        limit: i64,
    ) -> Result<Vec<RegistryEntry>, AppPlatformError> {
        let rows: Vec<RegistryEntry> = sqlx::query_as(
            "SELECT r.id, r.artifact_kind, r.name, r.version, r.artifact_id, r.artifact_version, \
                    r.manifest_digest, r.publisher, r.trust_tier, r.yanked, r.created_at \
             FROM registry_entry r \
             JOIN artifact a ON a.tenant_id = r.tenant_id AND a.id = r.artifact_id \
             WHERE r.tenant_id = $1 AND r.org = $2 AND r.artifact_kind = $3 AND NOT r.yanked \
               AND a.deleted_at IS NULL \
             ORDER BY r.created_at DESC LIMIT $4",
        )
        .bind(ctx.tenant_id())
        .bind(ctx.org())
        .bind(artifact_kind)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(map_db)?;
        Ok(rows)
    }

    /// エントリの署名（publish 時添付・first-party インストール検証用）。
    pub async fn signature_of(
        &self,
        ctx: &AuthContext,
        artifact_kind: &str,
        name: &str,
        version: &str,
    ) -> Result<Option<Vec<u8>>, AppPlatformError> {
        let row: Option<(Option<Vec<u8>>,)> = sqlx::query_as(
            "SELECT signature FROM registry_entry \
             WHERE tenant_id = $1 AND artifact_kind = $2 AND name = $3 AND version = $4",
        )
        .bind(ctx.tenant_id())
        .bind(artifact_kind)
        .bind(name)
        .bind(version)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.and_then(|(s,)| s))
    }

    /// エントリを yank する（新規インストールを止める・行は不変で残す）。
    pub async fn yank(&self, ctx: &AuthContext, id: Uuid) -> Result<(), AppPlatformError> {
        let updated =
            sqlx::query("UPDATE registry_entry SET yanked = true WHERE tenant_id = $1 AND id = $2")
                .bind(ctx.tenant_id())
                .bind(id)
                .execute(&self.db)
                .await
                .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(AppPlatformError::NotFound);
        }
        Ok(())
    }
}

/// registry_entry へ 1 行を書く（[`Registry::publish`]・[`Registry::publish_on`] の本体・同一 name+version は 409）。
async fn insert_entry<'e>(
    exec: impl PgExecutor<'e>,
    ctx: &AuthContext,
    entry: &NewRegistryEntry<'_>,
) -> Result<RegistryEntry, AppPlatformError> {
    sqlx::query_as(
        "INSERT INTO registry_entry \
         (tenant_id, org, artifact_kind, name, version, artifact_id, artifact_version, \
          manifest_digest, publisher, trust_tier, signature) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         RETURNING id, artifact_kind, name, version, artifact_id, artifact_version, \
                   manifest_digest, publisher, trust_tier, yanked, created_at",
    )
    .bind(ctx.tenant_id())
    .bind(ctx.org())
    .bind(entry.artifact_kind)
    .bind(entry.name)
    .bind(entry.version)
    .bind(entry.artifact_id)
    .bind(entry.artifact_version)
    .bind(entry.manifest_digest)
    .bind(&ctx.principal().id)
    .bind(entry.trust_tier)
    .bind(entry.signature)
    .fetch_one(exec)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppPlatformError::Conflict(format!(
                "{}@{} は既に publish 済みです（不変）",
                entry.name, entry.version
            ))
        }
        _ => map_db(e),
    })
}
