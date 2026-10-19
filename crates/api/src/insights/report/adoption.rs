//! 利用者と部署（#751）: 部署×週の利用率・利用区分・初めて使った月ごとの継続・個人別。

use chrono::{Datelike, Duration, NaiveDate};
use serde::Serialize;
use sqlx::PgPool;
use utoipa::ToSchema;

use super::{
    check_scope, departments, dept_label, guarded, population, ratio, ReportError, Scope,
    DEPT_FILTER,
};
use crate::insights::{category_label, MIN_GROUP_SIZE};

/// 部署×週の表で見せる最大週数。
const MAX_WEEKS: usize = 12;
/// 継続の表で見せる月数（初回利用の月・経過月とも）。
const COHORT_MONTHS: usize = 6;

/// 部署 1 行。
#[derive(Debug, Serialize, ToSchema)]
pub struct DeptRow {
    pub role_id: String,
    pub label: String,
    pub members: i64,
    /// 5 人未満で数値を伏せた行か。
    pub hidden: bool,
    /// 週ごとの利用率（`weeks` の順）。
    pub weekly_rates: Vec<Option<f64>>,
    /// 定着率（期間の最後の 7 日 ÷ 最後の 30 日）。
    pub stickiness: Option<f64>,
    /// 1 人 1 週あたりの発話数（使った人だけで割る）。
    pub runs_per_user_week: Option<f64>,
    /// 主な用途（分類がオフ・会話なしは `null`）。
    pub top_use: Option<String>,
}

/// 利用区分（期間の終わり基準）。
#[derive(Debug, Serialize, ToSchema)]
pub struct Segments {
    pub population: i64,
    /// 直近 28 日に 16 日以上。
    pub heavy: i64,
    /// 4〜15 日。
    pub regular: i64,
    /// 1〜3 日。
    pub light: i64,
    /// 以前は使っていたが直近 28 日は使っていない（区分の合計が対象者数と一致するよう 28 日でそろえる）。
    pub dormant: i64,
    /// 一度も使っていない。
    pub never: i64,
}

/// 初めて使った月ごとの継続。
#[derive(Debug, Serialize, ToSchema)]
pub struct Cohort {
    /// 初めて使った月（`YYYY-MM`）。
    pub month: String,
    pub users: i64,
    /// 1〜6 か月後にも使っていた割合（未来の月・5 人未満は `null`）。
    pub retention: Vec<Option<f64>>,
}

/// 利用者と部署。
#[derive(Debug, Serialize, ToSchema)]
pub struct Adoption {
    pub weeks: Vec<NaiveDate>,
    pub departments: Vec<DeptRow>,
    pub segments: Segments,
    pub cohorts: Vec<Cohort>,
    /// 個人別の表示が有効か（無効なら個人別 API は 403）。
    pub individual_view: bool,
}

/// 個人別の 1 行（会話本文は含めない）。
#[derive(Debug, Serialize, ToSchema)]
pub struct PersonRow {
    pub user_id: String,
    pub name: String,
    pub email: String,
    pub active_days: i64,
    pub runs: i64,
    pub threads: i64,
    pub agent_runs: i64,
    pub searches: i64,
    pub skill_uses: i64,
    pub last_active: NaiveDate,
    pub top_use: Option<String>,
}

/// 期間の週（月曜始まり・新しい方から最大 12 週）。
fn weeks_in(s: &Scope) -> Vec<NaiveDate> {
    let monday = |d: NaiveDate| d - Duration::days(i64::from(d.weekday().num_days_from_monday()));
    let mut out = Vec::new();
    let mut w = monday(s.to);
    while w + Duration::days(6) >= s.from && out.len() < MAX_WEEKS {
        out.push(w);
        w -= Duration::days(7);
    }
    out.reverse();
    out
}

async fn dept_rows(
    db: &PgPool,
    s: &Scope,
    weeks: &[NaiveDate],
) -> Result<Vec<DeptRow>, sqlx::Error> {
    let from = weeks.first().copied().unwrap_or(s.from);
    let per_week: Vec<(String, NaiveDate, i64, i64)> = sqlx::query_as(
        "SELECT r.role_id, date_trunc('week', a.day)::date, count(DISTINCT a.user_id), \
                COALESCE(sum(a.runs), 0)::bigint \
         FROM directory_user_role r \
         JOIN ai_activity_daily a ON a.tenant_id = r.tenant_id AND a.org = r.org \
          AND a.user_id = r.user_id AND a.day BETWEEN $3 AND $4 \
         WHERE r.tenant_id = $1 AND r.org = $2 AND r.source = 'group' GROUP BY 1, 2",
    )
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(from)
    .bind(s.to)
    .fetch_all(db)
    .await?;
    let window = |days: i64| async move {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT r.role_id, count(DISTINCT a.user_id) \
             FROM directory_user_role r \
             JOIN ai_activity_daily a ON a.tenant_id = r.tenant_id AND a.org = r.org \
              AND a.user_id = r.user_id AND a.day > $3::date - $4::int AND a.day <= $3 \
             WHERE r.tenant_id = $1 AND r.org = $2 AND r.source = 'group' GROUP BY 1",
        )
        .bind(&s.tenant_id)
        .bind(&s.org)
        .bind(s.to)
        .bind(i32::try_from(days).unwrap_or(30))
        .fetch_all(db)
        .await?;
        Ok::<_, sqlx::Error>(rows)
    };
    let w7 = window(7).await?;
    let w30 = window(30).await?;
    let top: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (role_id) role_id, category FROM ( \
           SELECT r.role_id, ti.category, count(*) AS n \
           FROM thread_insight ti \
           JOIN directory_user_role r ON r.tenant_id = ti.tenant_id AND r.org = ti.org \
            AND r.user_id = ti.owner AND r.source = 'group' \
           WHERE ti.tenant_id = $1 AND ti.org = $2 \
             AND ti.classified_at >= ($3::date)::timestamp AT TIME ZONE 'Asia/Tokyo' \
           GROUP BY 1, 2) x \
         ORDER BY role_id, n DESC",
    )
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(from)
    .fetch_all(db)
    .await?;
    let get = |rows: &[(String, i64)], id: &str| {
        rows.iter().find(|(r, _)| r == id).map_or(0, |(_, n)| *n)
    };
    Ok(departments(db, &s.tenant_id, &s.org)
        .await?
        .into_iter()
        .map(|(role_id, display, members)| {
            let hidden = members < MIN_GROUP_SIZE;
            let mine: Vec<&(String, NaiveDate, i64, i64)> = per_week
                .iter()
                .filter(|(r, _, _, _)| *r == role_id)
                .collect();
            let weekly_rates = weeks
                .iter()
                .map(|w| {
                    let n = mine.iter().find(|(_, wk, _, _)| wk == w).map_or(0, |x| x.2);
                    ratio(n, members).and_then(|r| guarded(members, r))
                })
                .collect();
            let user_weeks: i64 = mine.iter().map(|x| x.2).sum();
            let runs: i64 = mine.iter().map(|x| x.3).sum();
            DeptRow {
                label: dept_label(&role_id, &display),
                members,
                hidden,
                weekly_rates,
                stickiness: ratio(get(&w7, &role_id), get(&w30, &role_id))
                    .and_then(|r| guarded(members, r)),
                runs_per_user_week: ratio(runs, user_weeks).and_then(|r| guarded(members, r)),
                top_use: if hidden {
                    None
                } else {
                    top.iter()
                        .find(|(r, _)| *r == role_id)
                        .map(|(_, c)| category_label(c).to_string())
                },
                role_id,
            }
        })
        .collect())
}

async fn segments(db: &PgPool, s: &Scope, population: i64) -> Result<Segments, sqlx::Error> {
    let (ever, heavy, regular, light, recent): (i64, i64, i64, i64, i64) =
        sqlx::query_as(&format!(
            "WITH u AS ( \
           SELECT user_id, \
                  count(DISTINCT day) FILTER (WHERE day > $4 - 28) AS d28, \
                  bool_or(day > $4 - 28) AS recent \
           FROM ai_activity_daily \
           WHERE tenant_id = $1 AND org = $2 AND day <= $4 AND $3::date IS NOT NULL \
             AND {DEPT_FILTER} \
           GROUP BY user_id) \
         SELECT count(*), count(*) FILTER (WHERE d28 >= 16), \
                count(*) FILTER (WHERE d28 BETWEEN 4 AND 15), \
                count(*) FILTER (WHERE d28 BETWEEN 1 AND 3), count(*) FILTER (WHERE recent) \
         FROM u"
        ))
        .bind(&s.tenant_id)
        .bind(&s.org)
        .bind(s.from)
        .bind(s.to)
        .bind(s.dept.as_deref())
        .fetch_one(db)
        .await?;
    Ok(Segments {
        population,
        heavy,
        regular,
        light,
        dormant: ever - recent,
        never: (population - ever).max(0),
    })
}

/// 月の初日から n か月後の初日。
fn add_months(d: NaiveDate, n: i32) -> NaiveDate {
    let total = d.year() * 12 + i32::try_from(d.month0()).unwrap_or(0) + n;
    NaiveDate::from_ymd_opt(
        total.div_euclid(12),
        u32::try_from(total.rem_euclid(12)).unwrap_or(0) + 1,
        1,
    )
    .unwrap_or(d)
}

async fn cohorts(db: &PgPool, s: &Scope) -> Result<Vec<Cohort>, sqlx::Error> {
    let end_month = NaiveDate::from_ymd_opt(s.to.year(), s.to.month(), 1).unwrap_or(s.to);
    let first_month = add_months(end_month, -(i32::try_from(COHORT_MONTHS).unwrap_or(6) - 1));
    // (初回月, 使った月, 人数)
    let rows: Vec<(NaiveDate, NaiveDate, i64)> = sqlx::query_as(&format!(
        "WITH f AS ( \
           SELECT user_id, date_trunc('month', min(day))::date AS m0 \
           FROM ai_activity_daily \
           WHERE tenant_id = $1 AND org = $2 AND day <= $4 AND {DEPT_FILTER} \
           GROUP BY user_id), \
         act AS ( \
           SELECT DISTINCT user_id, date_trunc('month', day)::date AS m \
           FROM ai_activity_daily \
           WHERE tenant_id = $1 AND org = $2 AND day <= $4 AND day >= $3) \
         SELECT f.m0, act.m, count(*) FROM f JOIN act USING (user_id) \
         WHERE f.m0 >= $3 GROUP BY 1, 2"
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(first_month)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_all(db)
    .await?;
    // 集計の遡りが途中の月から始まっていると、その月には以前から使っていた人も「初めて使った」
    // として入り膨らむ。集計より前にも利用の記録がある（＝遡りで切れている）なら、その月は出さない。
    let truncated_month: Option<NaiveDate> = sqlx::query_scalar(
        "SELECT date_trunc('month', e.d)::date FROM ( \
           SELECT min(day) AS d FROM ai_activity_daily WHERE tenant_id = $1 AND org = $2) e \
         WHERE EXISTS (SELECT 1 FROM llm_usage l \
                        WHERE l.tenant_id = $1 AND l.org = $2 \
                          AND l.created_at < (e.d)::timestamp AT TIME ZONE 'Asia/Tokyo')",
    )
    .bind(&s.tenant_id)
    .bind(&s.org)
    .fetch_optional(db)
    .await?
    .flatten();
    let mut out = Vec::new();
    for i in 0..COHORT_MONTHS {
        let m0 = add_months(first_month, i32::try_from(i).unwrap_or(0));
        if truncated_month.is_some_and(|t| m0 <= t) {
            continue;
        }
        let users = rows
            .iter()
            .find(|(c, m, _)| *c == m0 && *m == m0)
            .map_or(0, |r| r.2);
        if users == 0 {
            continue;
        }
        let retention = (1..=COHORT_MONTHS)
            .map(|k| {
                let mk = add_months(m0, i32::try_from(k).unwrap_or(0));
                if mk > end_month {
                    return None;
                }
                let n = rows
                    .iter()
                    .find(|(c, m, _)| *c == m0 && *m == mk)
                    .map_or(0, |r| r.2);
                ratio(n, users).and_then(|r| guarded(users, r))
            })
            .collect();
        out.push(Cohort {
            month: m0.format("%Y-%m").to_string(),
            users,
            retention,
        });
    }
    Ok(out)
}

/// 利用者と部署を組み立てる。
pub async fn adoption(
    db: &PgPool,
    s: &Scope,
    individual_view: bool,
) -> Result<Adoption, ReportError> {
    check_scope(db, s).await?;
    let weeks = weeks_in(s);
    let population = population(db, s).await?;
    let departments = if s.dept.is_some() {
        Vec::new()
    } else {
        dept_rows(db, s, &weeks).await?
    };
    Ok(Adoption {
        departments,
        segments: segments(db, s, population).await?,
        cohorts: cohorts(db, s).await?,
        weeks,
        individual_view,
    })
}

/// 個人別（呼び出し側が設定と監査を済ませてから呼ぶ）。
pub async fn people(db: &PgPool, s: &Scope) -> Result<Vec<PersonRow>, ReportError> {
    check_scope(db, s).await?;
    type Row = (
        String,
        Option<String>,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        NaiveDate,
        Option<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT a.user_id, MAX(du.display_name), MAX(du.email), count(DISTINCT a.day), \
                COALESCE(sum(a.runs), 0)::bigint, \
                (SELECT count(DISTINCT g.thread_id) FROM generation_run g \
                  WHERE g.tenant_id = a.tenant_id AND g.org = a.org AND g.actor = a.user_id \
                    AND g.created_at >= ($3::date)::timestamp AT TIME ZONE 'Asia/Tokyo' \
                    AND g.created_at < ($4::date + 1)::timestamp AT TIME ZONE 'Asia/Tokyo'), \
                COALESCE(sum(a.agent_runs), 0)::bigint, \
                COALESCE(sum(a.doc_searches + a.web_searches), 0)::bigint, \
                COALESCE(sum(a.skill_uses), 0)::bigint, max(a.day), \
                (SELECT ti.category FROM thread_insight ti \
                  WHERE ti.tenant_id = a.tenant_id AND ti.org = a.org AND ti.owner = a.user_id \
                  GROUP BY ti.category ORDER BY count(*) DESC LIMIT 1) \
         FROM ai_activity_daily a \
         LEFT JOIN directory_user du ON du.tenant_id = a.tenant_id AND du.user_id = a.user_id \
         WHERE a.tenant_id = $1 AND a.org = $2 AND a.day BETWEEN $3 AND $4 AND {} \
         GROUP BY a.tenant_id, a.org, a.user_id ORDER BY 4 DESC, 5 DESC LIMIT 500",
        DEPT_FILTER.replace("user_id IN", "a.user_id IN")
    ))
    .bind(&s.tenant_id)
    .bind(&s.org)
    .bind(s.from)
    .bind(s.to)
    .bind(s.dept.as_deref())
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                user_id,
                name,
                email,
                active_days,
                runs,
                threads,
                agent_runs,
                searches,
                skill_uses,
                last,
                top,
            )| {
                PersonRow {
                    name: name.unwrap_or_else(|| user_id.clone()),
                    email: email.unwrap_or_default(),
                    user_id,
                    active_days,
                    runs,
                    threads,
                    agent_runs,
                    searches,
                    skill_uses,
                    last_active: last,
                    top_use: top.map(|c| category_label(&c).to_string()),
                }
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn weeks_are_mondays_capped_at_twelve() {
        let s = Scope {
            tenant_id: "t".into(),
            org: "o".into(),
            from: d("2026-07-01"),
            to: d("2026-10-19"),
            dept: None,
        };
        let w = weeks_in(&s);
        assert_eq!(w.len(), MAX_WEEKS);
        assert_eq!(*w.last().unwrap(), d("2026-10-19")); // 月曜
        assert!(w.iter().all(|x| x.weekday() == chrono::Weekday::Mon));
        let short = Scope {
            from: d("2026-10-10"),
            ..s
        };
        assert_eq!(
            weeks_in(&short),
            vec![d("2026-10-05"), d("2026-10-12"), d("2026-10-19")]
        );
    }

    #[test]
    fn months_add_across_years() {
        assert_eq!(add_months(d("2026-11-01"), 3), d("2027-02-01"));
        assert_eq!(add_months(d("2026-02-01"), -5), d("2025-09-01"));
    }
}
