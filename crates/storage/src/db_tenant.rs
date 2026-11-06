//! テナント文脈付きの DB 実行器（#1067・RLS 段階導入の第 2 段）。
//!
//! migration 0114 は、テナント表に RLS（`shiki_tenant_isolation`）を付けた。ポリシーは
//! トランザクションのテナント文脈（`app.tenant_id`）を見て、設定されていればその `tenant_id` の行だけを
//! 読み書きさせる。本モジュールは、その文脈を設定したトランザクションを開く**唯一の入口**。
//!
//! - テナントの行を読み書きする処理は [`begin_tenant_tx`] を使う。`authz::TenantId`（検証済み）だけを受け取る。
//! - 全テナントを横断する保守処理（outbox のリレーと GC・期限切れの失効等）は [`begin_cross_tenant_tx`] を
//!   使う。`shiki_maintenance`（BYPASSRLS）に切り替える。
//!
//! 戻り値は別々の型（[`TenantTx`] / [`CrossTenantTx`]）にしてある。第 3 段で「テナント文脈付きの
//! トランザクションでしか呼べない」関数を型で表せるようにするため。どちらも `PgConnection` に deref
//! するので、`sqlx::query(..).execute(&mut *tx)` の形でそのまま使える。
//!
//! 設定はどれも `SET LOCAL` 相当（トランザクション限り）なので、コミットでもロールバックでも（drop でも）
//! 元に戻り、プールに返した接続へテナントやロールが残らない。セッション単位の `SET` は使わない
//! （プールの次の利用者へ文脈が漏れる）。
//!
//! # 移行モード
//!
//! 今のポリシーは文脈が未設定なら通す（既存の約 920 か所の SQL はまだ文脈を設定していない）。
//! 第 3 段でクレートごとに本実行器へ移し、全経路を移したら未設定を拒否する厳格モードへ切り替える。
//! 移した経路は、アプリの `WHERE tenant_id = …` を書き忘れても DB が他テナントの行を返さない。

use std::ops::{Deref, DerefMut};

use authz::TenantId;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

/// RLS のポリシーが読むテナント文脈の設定名（migration 0114 の `shiki_rls_tenant()` と一致させる）。
pub const TENANT_SETTING: &str = "app.tenant_id";

/// テナント横断の保守処理が使うロール（migration 0114 が作る・BYPASSRLS）。
pub const MAINTENANCE_ROLE: &str = "shiki_maintenance";

/// 横断のトランザクションで `application_name` に付ける接頭辞（`pg_stat_activity`・サーバログで追える）。
pub const CROSS_TENANT_APP_NAME_PREFIX: &str = "shiki:cross-tenant:";

/// テナント文脈を設定したトランザクション（[`begin_tenant_tx`] が返す）。
pub struct TenantTx {
    tx: Transaction<'static, Postgres>,
    tenant: TenantId,
}

impl TenantTx {
    /// このトランザクションのテナント。
    #[must_use]
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// コミットする（文脈はここで消える）。
    ///
    /// # Errors
    ///
    /// `COMMIT` に失敗したとき。
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.tx.commit().await
    }

    /// ロールバックする（文脈はここで消える）。
    ///
    /// # Errors
    ///
    /// `ROLLBACK` に失敗したとき。
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        self.tx.rollback().await
    }
}

impl Deref for TenantTx {
    type Target = PgConnection;
    fn deref(&self) -> &PgConnection {
        &self.tx
    }
}

impl DerefMut for TenantTx {
    fn deref_mut(&mut self) -> &mut PgConnection {
        &mut self.tx
    }
}

/// テナント文脈を設定したトランザクションを開く。
///
/// このトランザクションの中では、RLS の付いたテナント表は `tenant` の行だけが見え、
/// 他テナントの `tenant_id` を持つ行は INSERT / UPDATE できない（`WITH CHECK` 違反の 42501）。
///
/// # Errors
///
/// 接続の取得・`BEGIN`・文脈の設定に失敗したとき。
pub async fn begin_tenant_tx(pool: &PgPool, tenant: &TenantId) -> Result<TenantTx, sqlx::Error> {
    let mut tx = pool.begin().await?;
    set_tenant(&mut tx, tenant).await?;
    Ok(TenantTx {
        tx,
        tenant: tenant.clone(),
    })
}

/// すでに開いているトランザクションにテナント文脈を設定する。
///
/// 自前で `begin` した処理を移すとき用。トランザクションの外では効かないので、型で
/// トランザクションに限っている。セーブポイント（入れ子の `Transaction`）の中で設定してそれを
/// ロールバックすると、文脈も外側の値に戻ることに注意（外側で設定しておく）。
///
/// # Errors
///
/// 文脈の設定に失敗したとき。
pub async fn set_tenant(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &TenantId,
) -> Result<(), sqlx::Error> {
    // set_config(.., true) は SET LOCAL と同じ（トランザクション限り）。値はバインドで渡す。
    sqlx::query("SELECT pg_catalog.set_config($1, $2, true)")
        .bind(TENANT_SETTING)
        .bind(tenant.as_str())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// テナント横断の保守処理のトランザクション（[`begin_cross_tenant_tx`] が返す）。
pub struct CrossTenantTx {
    tx: Transaction<'static, Postgres>,
    purpose: &'static str,
}

impl CrossTenantTx {
    /// 何のための横断か（[`begin_cross_tenant_tx`] に渡した値）。
    #[must_use]
    pub fn purpose(&self) -> &'static str {
        self.purpose
    }

    /// コミットする（ロールはここで元に戻る）。
    ///
    /// # Errors
    ///
    /// `COMMIT` に失敗したとき。
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.tx.commit().await
    }

    /// ロールバックする（ロールはここで元に戻る）。
    ///
    /// # Errors
    ///
    /// `ROLLBACK` に失敗したとき。
    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        self.tx.rollback().await
    }
}

impl Deref for CrossTenantTx {
    type Target = PgConnection;
    fn deref(&self) -> &PgConnection {
        &self.tx
    }
}

impl DerefMut for CrossTenantTx {
    fn deref_mut(&mut self) -> &mut PgConnection {
        &mut self.tx
    }
}

/// 全テナントを横断する保守処理のトランザクションを開く。
///
/// `purpose` は何のための横断かを示す固定の文字列（例 `"outbox_gc"`）。横断は RLS を迂回するので、
/// 呼び出し元を grep で洗い出せるよう、横断はこの関数経由に限る。`purpose` はこのトランザクションの
/// `application_name`（`shiki:cross-tenant:<purpose>`）にも載るので、`pg_stat_activity` や
/// `log_line_prefix` の `%a` で横断中のクエリを追える。
///
/// # Errors
///
/// 接続の取得・`BEGIN`・ロールの切り替えに失敗したとき（`shiki_maintenance` が無い・メンバでない等）。
pub async fn begin_cross_tenant_tx(
    pool: &PgPool,
    purpose: &'static str,
) -> Result<CrossTenantTx, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // ロールの切り替え（SET LOCAL ROLE と同じ）と application_name を 1 往復で設定する。
    sqlx::query(
        "SELECT pg_catalog.set_config('role', $1, true), \
                pg_catalog.set_config('application_name', $2, true)",
    )
    .bind(MAINTENANCE_ROLE)
    .bind(format!("{CROSS_TENANT_APP_NAME_PREFIX}{purpose}"))
    .execute(&mut *tx)
    .await?;
    tracing::debug!(purpose, "テナント横断のトランザクションを開いた");
    Ok(CrossTenantTx { tx, purpose })
}
