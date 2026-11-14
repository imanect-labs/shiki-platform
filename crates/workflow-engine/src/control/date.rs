//! 日付の計算（「日付を計算」・`transform.date` の本体・**純関数**・GAP-15）。
//!
//! 基準の日時を時間帯の暦の日付に直してから、日・週・月・年でずらし、ずらした日を含む期間の最初の日と最後の日を
//! 求める。時刻を足し引きしないので、夏時間の切り替わりで 1 日が 23・25 時間になる日をまたいでも、日付はずれない
//! （24 時間を引くと前日の 23 時になり、日付が 2 日前になる誤りを作らない）。
//!
//! - 月・年でずらした先にその日が無ければ、その月の末日にする（1 月 31 日の 1 か月後は 2 月 28 日か 29 日、
//!   5 月 31 日の 1 か月後は 6 月 30 日）。
//! - 週の初めは月曜か日曜。期間が週なら、週の初めの日から 7 日。
//! - 期間の始まりの日時（`start_at`）と次の期間の始まりの日時（`next_start_at`）は、その日の時間帯の最初の時刻
//!   （ふつうは 0 時。夏時間の切り替わりで 0 時が無い日は、その日の最初にある時刻）。期間はこの 2 つの間
//!   （`start_at` 以上で `next_start_at` より前）。
//! - 日付は 1〜9999 年に収める（外れたら誤り）。

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, NaiveTime, SecondsFormat, TimeZone};
use chrono_tz::Tz;
use serde_json::{json, Value};

use crate::ir::params::{DateUnit, WeekStart};

/// 計算した日付の範囲（年）。
const MIN_YEAR: i32 = 1;
const MAX_YEAR: i32 = 9999;

/// 計算の結果（step の出力の形・カタログの output_fields と同じ項目）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateCalc {
    /// ずらした日。
    pub date: NaiveDate,
    /// 期間の最初の日。
    pub start: NaiveDate,
    /// 期間の最後の日。
    pub end: NaiveDate,
    /// 期間の始まりの日時（時間帯の時差つき）。
    pub start_at: DateTime<Tz>,
    /// 次の期間の始まりの日時（期間はこの日時より前まで）。
    pub next_start_at: DateTime<Tz>,
}

impl DateCalc {
    /// step の出力（日付は `YYYY-MM-DD`・日時は時差つきの RFC 3339・曜日は 1＝月曜〜7＝日曜）。
    #[must_use]
    pub fn to_value(&self) -> Value {
        let day = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
        let at = |t: &DateTime<Tz>| t.to_rfc3339_opts(SecondsFormat::Secs, false);
        json!({
            "date": day(self.date),
            "start": day(self.start),
            "end": day(self.end),
            "start_at": at(&self.start_at),
            "next_start_at": at(&self.next_start_at),
            "year": self.date.year(),
            "month": self.date.month(),
            "day": self.date.day(),
            "weekday": self.date.weekday().number_from_monday(),
        })
    }
}

/// 基準の日時を時間帯の日付にする（`YYYY-MM-DD` はその日付のまま、時差つきの日時はその時間帯の日付）。
///
/// 文字列でない値・どちらの形でもない文字列は `None`（時差の無い日時は、どの時間帯の時刻か分からないので読まない）。
#[must_use]
pub fn base_date(v: &Value, tz: Tz) -> Option<NaiveDate> {
    let s = v.as_str()?.trim();
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return in_range(d);
    }
    let t = DateTime::parse_from_rfc3339(s).ok()?;
    in_range(t.with_timezone(&tz).date_naive())
}

fn in_range(d: NaiveDate) -> Option<NaiveDate> {
    (MIN_YEAR..=MAX_YEAR).contains(&d.year()).then_some(d)
}

/// 日付をずらす（月・年でずらした先にその日が無ければ月末・範囲を外れたら `None`）。
#[must_use]
pub fn shift(date: NaiveDate, amount: i64, unit: DateUnit) -> Option<NaiveDate> {
    let shifted = match unit {
        DateUnit::Day => add_days(date, amount),
        DateUnit::Week => add_days(date, amount.checked_mul(7)?),
        DateUnit::Month => add_months(date, amount),
        DateUnit::Year => add_months(date, amount.checked_mul(12)?),
    }?;
    in_range(shifted)
}

fn add_days(date: NaiveDate, n: i64) -> Option<NaiveDate> {
    let days = Days::new(n.unsigned_abs());
    if n >= 0 {
        date.checked_add_days(days)
    } else {
        date.checked_sub_days(days)
    }
}

fn add_months(date: NaiveDate, n: i64) -> Option<NaiveDate> {
    // chrono は、ずらした先の月にその日が無ければ月末にする。
    let months = Months::new(u32::try_from(n.unsigned_abs()).ok()?);
    if n >= 0 {
        date.checked_add_months(months)
    } else {
        date.checked_sub_months(months)
    }
}

/// 日付を含む期間の最初の日と、次の期間の最初の日（期間の最後の日の翌日）。
fn period(
    date: NaiveDate,
    unit: DateUnit,
    week_start: WeekStart,
) -> Option<(NaiveDate, NaiveDate)> {
    match unit {
        DateUnit::Day => Some((date, date.succ_opt()?)),
        DateUnit::Week => {
            let back = match week_start {
                WeekStart::Monday => date.weekday().num_days_from_monday(),
                WeekStart::Sunday => date.weekday().num_days_from_sunday(),
            };
            let start = date.checked_sub_days(Days::new(u64::from(back)))?;
            Some((start, start.checked_add_days(Days::new(7))?))
        }
        DateUnit::Month => {
            let start = date.with_day(1)?;
            Some((start, start.checked_add_months(Months::new(1))?))
        }
        DateUnit::Year => {
            let start = NaiveDate::from_ymd_opt(date.year(), 1, 1)?;
            Some((
                start,
                NaiveDate::from_ymd_opt(date.year().checked_add(1)?, 1, 1)?,
            ))
        }
    }
}

/// その日の時間帯の最初の時刻（ふつうは 0 時。夏時間の切り替わりで 0 時が無い日は、その後の最初にある時刻。
/// 0 時が 2 回ある日は早い方）。
#[must_use]
pub fn day_start(tz: Tz, date: NaiveDate) -> Option<DateTime<Tz>> {
    // 夏時間で飛ばされる時刻は長くて 2 時間ほど。15 分ずつ 1 日の中を進めて、最初にある時刻を探す。
    (0..96).find_map(|quarter: u32| {
        let t = NaiveTime::from_hms_opt(quarter / 4, (quarter % 4) * 15, 0)?;
        tz.from_local_datetime(&date.and_time(t)).earliest()
    })
}

/// 日付の計算の誤り（利用者向けの文・step の permanent な失敗にする）。
pub const OUT_OF_RANGE: &str =
    "計算した日付が 1〜9999 年の範囲を外れました。「ずらす数」を見直してください。";

/// 基準の日付をずらし、期間の最初と最後の日と、期間の始まりと次の期間の始まりの日時を求める。
pub fn calc(
    base: NaiveDate,
    shift_by: Option<(i64, DateUnit)>,
    unit: DateUnit,
    week_start: WeekStart,
    tz: Tz,
) -> Result<DateCalc, &'static str> {
    let date = match shift_by {
        Some((amount, by)) => shift(base, amount, by).ok_or(OUT_OF_RANGE)?,
        None => base,
    };
    let (start, next) = period(date, unit, week_start).ok_or(OUT_OF_RANGE)?;
    let start = in_range(start).ok_or(OUT_OF_RANGE)?;
    let next = in_range(next).ok_or(OUT_OF_RANGE)?;
    let end = next.pred_opt().ok_or(OUT_OF_RANGE)?;
    Ok(DateCalc {
        date,
        start,
        end,
        start_at: day_start(tz, start).ok_or(OUT_OF_RANGE)?,
        next_start_at: day_start(tz, next).ok_or(OUT_OF_RANGE)?,
    })
}

#[cfg(test)]
#[path = "date_tests.rs"]
mod tests;
