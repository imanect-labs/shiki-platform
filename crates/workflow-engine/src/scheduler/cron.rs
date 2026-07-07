//! cron 評価（5 フィールド＋IANA tz・engine.md §5.2）。
//!
//! IR の cron は 5 フィールド（min hour dom month dow）。cron クレートは 7 フィールド
//! （sec min hour dom month dow year）のため、秒を `0`・年を `*` で補って解釈する。

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use std::str::FromStr;

/// cron/tz のパースエラー。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CronError {
    #[error("不正な cron 式: {0}")]
    BadCron(String),
    #[error("不正なタイムゾーン: {0}")]
    BadTz(String),
}

/// 5 フィールド cron を 7 フィールド（秒 0・年 *）へ正規化して [`cron::Schedule`] を作る。
fn parse_schedule(cron5: &str) -> Result<cron::Schedule, CronError> {
    let fields: Vec<&str> = cron5.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(CronError::BadCron(format!(
            "5 フィールド必須（受信 {} フィールド）",
            fields.len()
        )));
    }
    let seven = format!("0 {cron5} *");
    cron::Schedule::from_str(&seven).map_err(|e| CronError::BadCron(format!("{e}")))
}

/// IANA タイムゾーンをパースする。
fn parse_tz(tz: &str) -> Result<Tz, CronError> {
    Tz::from_str(tz).map_err(|_| CronError::BadTz(tz.to_string()))
}

/// `(after, now]` 区間で発火すべき occurrence（UTC）を列挙する（watermark 前進用）。
///
/// tz で cron を評価し UTC へ変換する。区間の上限は `now`（含む）、下限は `after`（含まない）。
pub fn occurrences_between(
    cron5: &str,
    tz: &str,
    after: DateTime<Utc>,
    now: DateTime<Utc>,
    limit: usize,
) -> Result<Vec<DateTime<Utc>>, CronError> {
    let schedule = parse_schedule(cron5)?;
    let zone = parse_tz(tz)?;
    let after_tz = after.with_timezone(&zone);
    let mut out = Vec::new();
    for dt in schedule.after(&after_tz).take(limit.clamp(1, 10_000)) {
        let utc = dt.with_timezone(&Utc);
        if utc > now {
            break;
        }
        if utc > after {
            out.push(utc);
        }
    }
    Ok(out)
}

/// cron/tz が妥当か検証する（IR 保存時の軽い検証にも使える）。
pub fn validate(cron5: &str, tz: &str) -> Result<(), CronError> {
    parse_schedule(cron5)?;
    parse_tz(tz)?;
    Ok(())
}

/// 直近の次回発火時刻（UTC・catchup 判定の補助）。
pub fn next_after(
    cron5: &str,
    tz: &str,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, CronError> {
    let schedule = parse_schedule(cron5)?;
    let zone = parse_tz(tz)?;
    let after_tz = after.with_timezone(&zone);
    Ok(schedule
        .after(&after_tz)
        .next()
        .map(|d| d.with_timezone(&Utc)))
}

/// UTC の DateTime を組む（テスト補助）。
#[cfg(test)]
pub(crate) fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    use chrono::TimeZone;
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_5_fields() {
        assert!(validate("0 9 * * *", "Asia/Tokyo").is_ok());
        assert!(matches!(
            validate("0 9 * *", "UTC"),
            Err(CronError::BadCron(_))
        ));
        assert!(matches!(
            validate("0 9 * * *", "Bogus/Zone"),
            Err(CronError::BadTz(_))
        ));
    }

    #[test]
    fn daily_9am_tokyo_occurrence() {
        // 2026-07-07 09:00 JST = 2026-07-07 00:00 UTC。
        let after = utc(2026, 7, 6, 12, 0); // 前日 12:00 UTC
        let now = utc(2026, 7, 7, 6, 0); // 当日 06:00 UTC（09:00 JST=00:00 UTC は過ぎている）
        let occ = occurrences_between("0 9 * * *", "Asia/Tokyo", after, now, 100).unwrap();
        assert_eq!(occ.len(), 1);
        assert_eq!(occ[0], utc(2026, 7, 7, 0, 0));
    }

    #[test]
    fn no_occurrence_before_due() {
        let after = utc(2026, 7, 7, 1, 0);
        let now = utc(2026, 7, 7, 2, 0);
        // 09:00 JST=00:00 UTC は after より前なので、この区間には無い。
        let occ = occurrences_between("0 9 * * *", "Asia/Tokyo", after, now, 100).unwrap();
        assert!(occ.is_empty());
    }

    #[test]
    fn every_minute_multiple() {
        let after = utc(2026, 7, 7, 0, 0);
        let now = utc(2026, 7, 7, 0, 5);
        let occ = occurrences_between("* * * * *", "UTC", after, now, 100).unwrap();
        // 00:01..00:05 = 5 occurrences。
        assert_eq!(occ.len(), 5);
    }

    #[test]
    fn next_after_computes() {
        let n = next_after("0 0 * * *", "UTC", utc(2026, 7, 7, 5, 0)).unwrap();
        assert_eq!(n, Some(utc(2026, 7, 8, 0, 0)));
    }
}
