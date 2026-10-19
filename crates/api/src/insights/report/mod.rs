//! 管理画面（`/org-admin/insights/*`）の集計（#751）。
//!
//! 材料は [`super::rollup`] のロールアップ・用途ラベル・評価。範囲は常に **tenant_id ＋ org**。
//!
//! 少人数の区分から個人が分からないよう（PIT-17）:
//! - 部署・コホートなど人の集まりは [`super::MIN_GROUP_SIZE`] 人未満なら数値を伏せる（`None`）。
//! - 部署での絞り込みは「部署まるごと」だけ（任意の条件の組み合わせは受けない＝差分で個人を
//!   割り出す経路を作らない）。絞り込んだ部署が少人数なら集計自体を断る。

pub mod adoption;
pub mod impact;
pub mod overview;
pub mod usecases;

use chrono::{Duration, NaiveDate};
use sqlx::PgPool;

use super::MIN_GROUP_SIZE;

/// 集計の範囲（日付は日本時間・両端含む）。
#[derive(Debug, Clone)]
pub struct Scope {
    pub tenant_id: String,
    pub org: String,
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// 部署（groups 由来のロール ID）で絞る。
    pub dept: Option<String>,
}

impl Scope {
    /// 同じ長さの直前の期間（前期比用）。
    #[must_use]
    pub fn previous(&self) -> Scope {
        let len = (self.to - self.from).num_days() + 1;
        Scope {
            from: self.from - Duration::days(len),
            to: self.from - Duration::days(1),
            ..self.clone()
        }
    }
}

/// 集計のエラー。
#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("この部署は {MIN_GROUP_SIZE} 人未満のため集計できません")]
    GroupTooSmall,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// 部署での絞り込み（`$5` = dept・NULL は全体）。user_id 列を持つ表に `AND` で足す。
pub(crate) const DEPT_FILTER: &str = "($5::text IS NULL OR user_id IN ( \
    SELECT r.user_id FROM directory_user_role r \
    WHERE r.tenant_id = $1 AND r.org = $2 AND r.role_id = $5 AND r.source = 'group'))";

/// 少人数の区分は伏せる。
pub(crate) fn guarded(members: i64, value: f64) -> Option<f64> {
    (members >= MIN_GROUP_SIZE).then_some(value)
}

/// 比率（分母 0 は `None`）。
#[allow(clippy::cast_precision_loss)]
pub(crate) fn ratio(n: i64, d: i64) -> Option<f64> {
    (d > 0).then(|| n as f64 / d as f64)
}

/// 対象者数（ディレクトリの在籍者・部署で絞れる）。
pub(crate) async fn population(db: &PgPool, s: &Scope) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM directory_user \
         WHERE tenant_id = $1 AND org = $2 AND $3::date IS NOT NULL AND $4::date IS NOT NULL \
           AND {DEPT_FILTER}"
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_one(db)
    .await
}

/// 部署で絞るなら、それが部署（groups 由来）で、少人数でないことを確かめる（PIT-17）。
/// roles 由来のロール（admin 等）は部署ではないので絞り込みに使えない（人数 0 扱いで断る）。
pub async fn check_scope(db: &PgPool, s: &Scope) -> Result<(), ReportError> {
    let Some(dept) = &s.dept else {
        return Ok(());
    };
    let members: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT user_id) FROM directory_user_role \
         WHERE tenant_id = $1 AND org = $2 AND role_id = $3 AND source = 'group'",
    )
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(dept)
    .fetch_one(db)
    .await?;
    if members < MIN_GROUP_SIZE {
        return Err(ReportError::GroupTooSmall);
    }
    Ok(())
}

/// 期間内の利用者数（ロールアップに行がある人）。
pub(crate) async fn active_users(db: &PgPool, s: &Scope) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT count(DISTINCT user_id) FROM ai_activity_daily \
         WHERE tenant_id = $1 AND org = $2 AND day BETWEEN $3 AND $4 AND {DEPT_FILTER}"
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_one(db)
    .await
}

/// 部署（groups 由来のロール）と人数。
pub(crate) async fn departments(
    db: &PgPool,
    tenant_id: &str,
    org: &str,
) -> Result<Vec<(String, String, i64)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT r.role_id, COALESCE(dr.display_name, r.role_id), count(DISTINCT r.user_id) \
         FROM directory_user_role r \
         LEFT JOIN directory_role dr ON dr.tenant_id = r.tenant_id AND dr.role_id = r.role_id \
         WHERE r.tenant_id = $1 AND r.org = $2 AND r.source = 'group' \
         GROUP BY 1, 2 ORDER BY 2",
    )
    .bind(tenant_id)
    .bind(org)
    .fetch_all(db)
    .await
}

/// 部署の表示名を短くする（`acme/sales` → `sales`。表示名が設定されていればそれ）。
pub(crate) fn dept_label(role_id: &str, display: &str) -> String {
    if display != role_id {
        return display.to_string();
    }
    role_id.rsplit('/').next().unwrap_or(role_id).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previous_period_has_the_same_length() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let s = Scope {
            tenant_id: "t".into(),
            org: "o".into(),
            from: d("2026-10-01"),
            to: d("2026-10-10"),
            dept: None,
        };
        let p = s.previous();
        assert_eq!((p.from, p.to), (d("2026-09-21"), d("2026-09-30")));
    }

    #[test]
    fn small_groups_are_hidden() {
        assert_eq!(guarded(4, 0.5), None);
        assert_eq!(guarded(5, 0.5), Some(0.5));
        assert_eq!(ratio(1, 0), None);
        assert_eq!(ratio(1, 4), Some(0.25));
    }

    #[test]
    fn dept_labels_use_the_last_path_segment() {
        assert_eq!(dept_label("acme/sales", "acme/sales"), "sales");
        assert_eq!(dept_label("acme/sales", "営業部"), "営業部");
    }
}
