//! `tenant → pool` 解決の単一チョークポイント（Phase 15 Task 15.1・data-platform §12 第 2 段）。
//!
//! フルプール（全テナントが 1 つの Postgres を共有）の現状では解決先は常に既定プール 1 本だが、
//! **継ぎ目を後から入れるのは高い**（全ストアのコンストラクタと全クエリ経路を触り直すことになる）
//! ので、データプレーンのストアは接続プールを直接持たず、本ルータから引く形に先に揃える。
//!
//! # 解決の規則
//!
//! - [`PoolRouter::for_tenant`] — テナントの行（`data_table` / `data_record` 等）を読み書きするプール。
//!   第 2 段のプール分割では `tenant_placement.pool_id` を引いて解決する（現状は既定プール固定）。
//! - [`PoolRouter::registry`] — テナント横断で一意でなければならない採番（`tenant_int` /
//!   `table_int`）と配置レジストリ（`tenant_placement`）を置くプール。分割後も 1 か所に固定する
//!   （各プールが独立にシーケンスを持つとシャード跨ぎで衝突するため・docs/data/shard-safety.md）。
//! - [`PoolRouter::all`] — テナント横断の保守処理（移行ジョブ等）が**各プールへ問い合わせて合算**
//!   するための列挙。単一クエリでテナント横断の集計を書かない（シャード安全の禁止事項）。

use sqlx::PgPool;

/// 既定プールの識別子（`tenant_placement.pool_id` の既定値と一致させる）。
pub const DEFAULT_POOL_ID: &str = "default";

/// `tenant → pool` の解決器。
///
/// `Clone` は内部プールの参照カウントを増やすだけ（`PgPool` 自体が `Arc` 包み）。
#[derive(Clone, Debug)]
pub struct PoolRouter {
    default: PgPool,
}

impl PoolRouter {
    /// 単一プール構成（フルプール・第 1 段）のルータを作る。
    pub fn single(pool: PgPool) -> Self {
        PoolRouter { default: pool }
    }

    /// テナントの行を読み書きするプール。
    ///
    /// 第 2 段（プール分割）では `tenant_placement.pool_id` の解決に差し替える。呼び出し側は
    /// **リクエストの `AuthContext.tenant_id` から必ずこれで引く**（プールを保持・使い回さない）。
    pub fn for_tenant(&self, _tenant_id: &str) -> &PgPool {
        &self.default
    }

    /// 配置レジストリと横断一意の採番を置くプール（分割後も 1 か所に固定）。
    pub fn registry(&self) -> &PgPool {
        &self.default
    }

    /// 全プール（テナント横断の保守処理は各プールへ問い合わせて合算する）。
    pub fn all(&self) -> impl Iterator<Item = (&str, &PgPool)> {
        std::iter::once((DEFAULT_POOL_ID, &self.default))
    }
}

impl From<PgPool> for PoolRouter {
    fn from(pool: PgPool) -> Self {
        PoolRouter::single(pool)
    }
}
