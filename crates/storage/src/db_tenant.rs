//! テナント文脈付きの DB 実行器（#1067・RLS 段階導入の第 2 段）。
//!
//! migration 0114 は、テナント表に RLS（`shiki_tenant_isolation`）を付けた。ポリシーは
//! トランザクションのテナント文脈（`app.tenant_id`）を見て、設定されていればその `tenant_id` の行だけを
//! 読み書きさせる。本モジュールは、その文脈を設定したトランザクションを開く**唯一の入口**。
//!
//! - テナントの行を読み書きする処理は [`begin_tenant_tx`] を使う。`authz::TenantId`（検証済み）だけを受け取る。
//! - 全テナントを横断する保守処理（outbox のリレーと GC・期限切れの失効等）は [`begin_cross_tenant_tx`] を
//!   使う。`shiki_maintenance`（BYPASSRLS）に `SET LOCAL ROLE` する。
//!
//! どちらも `SET LOCAL` 相当（トランザクション限り）なので、コミットでもロールバックでも元に戻り、
//! プールに返した接続へテナントやロールが残らない。セッション単位の `SET` は使わない
//! （プールの次の利用者へ文脈が漏れる）。
//!
//! # 移行モード
//!
//! 今のポリシーは文脈が未設定なら通す（既存の約 920 か所の SQL はまだ文脈を設定していない）。
//! 第 3 段でクレートごとに本実行器へ移し、全経路を移したら未設定を拒否する厳格モードへ切り替える。
//! 移した経路は、アプリの `WHERE tenant_id = …` を書き忘れても DB が他テナントの行を返さない。

use authz::TenantId;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

/// RLS のポリシーが読むテナント文脈の設定名（migration 0114 の `shiki_rls_tenant()` と一致させる）。
pub const TENANT_SETTING: &str = "app.tenant_id";

/// テナント横断の保守処理が使うロール（migration 0114 が作る・BYPASSRLS）。
pub const MAINTENANCE_ROLE: &str = "shiki_maintenance";

/// テナント文脈を設定したトランザクションを開く。
///
/// このトランザクションの中では、RLS の付いたテナント表は `tenant` の行だけが見え、
/// 他テナントの `tenant_id` を持つ行は INSERT / UPDATE できない（`WITH CHECK` 違反の 42501）。
///
/// # Errors
///
/// 接続の取得・`BEGIN`・文脈の設定に失敗したとき。
pub async fn begin_tenant_tx(
    pool: &PgPool,
    tenant: &TenantId,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    set_tenant(&mut tx, tenant).await?;
    Ok(tx)
}

/// すでに開いているトランザクションにテナント文脈を設定する。
///
/// 自前で `begin` した処理を移すとき用。トランザクションの外（自動コミットの接続）で呼んでも、
/// その文の終わりで戻るだけで効かない。必ずトランザクションの中で呼ぶこと。
///
/// # Errors
///
/// 文脈の設定に失敗したとき。
pub async fn set_tenant(conn: &mut PgConnection, tenant: &TenantId) -> Result<(), sqlx::Error> {
    // set_config(.., true) は SET LOCAL と同じ（トランザクション限り）。値はバインドで渡す。
    sqlx::query("SELECT pg_catalog.set_config($1, $2, true)")
        .bind(TENANT_SETTING)
        .bind(tenant.as_str())
        .execute(conn)
        .await?;
    Ok(())
}

/// 全テナントを横断する保守処理のトランザクションを開く。
///
/// `purpose` は何のための横断かを示す固定の文字列（ログに残す）。横断は RLS を迂回するので、
/// 呼び出し元を grep で洗い出せるよう、横断はこの関数経由に限る。
///
/// # Errors
///
/// 接続の取得・`BEGIN`・ロールの切り替えに失敗したとき（`shiki_maintenance` が無い・メンバでない等）。
pub async fn begin_cross_tenant_tx(
    pool: &PgPool,
    purpose: &'static str,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // ロール名は定数（識別子はバインドできない）。
    sqlx::query("SET LOCAL ROLE shiki_maintenance")
        .execute(&mut *tx)
        .await?;
    tracing::debug!(purpose, "テナント横断のトランザクションを開いた");
    Ok(tx)
}
