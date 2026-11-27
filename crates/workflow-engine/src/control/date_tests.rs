//! 日付の計算（`control::date`）の単体テスト（月末・週の初め・夏時間・範囲・GAP-15）。

#![allow(clippy::unwrap_used)]

use chrono::NaiveDate;
use chrono_tz::Tz;
use serde_json::{json, Value};

use super::*;

const TOKYO: Tz = chrono_tz::Asia::Tokyo;
const NEW_YORK: Tz = chrono_tz::America::New_York;

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// 計算して出力の形にする（基準は日付か日時の文字列）。
fn run(
    base: &str,
    shift_by: Option<(i64, DateUnit)>,
    unit: DateUnit,
    week_start: WeekStart,
    tz: Tz,
) -> Value {
    let base = base_date(&json!(base), tz).unwrap();
    calc(base, shift_by, unit, week_start, tz)
        .unwrap()
        .to_value()
}

#[test]
fn month_shift_clamps_to_the_month_end() {
    let m = |base: &str, n: i64| shift(d(base), n, DateUnit::Month).unwrap();
    // 2 月（平年・うるう年）・30 日の月へずらすと月末。
    assert_eq!(m("2026-01-31", 1), d("2026-02-28"));
    assert_eq!(m("2028-01-31", 1), d("2028-02-29"));
    assert_eq!(m("2026-03-31", -1), d("2026-02-28"));
    assert_eq!(m("2026-05-31", 1), d("2026-06-30"));
    assert_eq!(m("2026-03-31", 1), d("2026-04-30"));
    // 月末でない日はそのまま。年をまたぐ。
    assert_eq!(m("2026-11-15", 3), d("2027-02-15"));
    assert_eq!(m("2026-01-15", -1), d("2025-12-15"));
    // うるう日の 1 年後・1 年前は 2 月 28 日。
    assert_eq!(
        shift(d("2028-02-29"), 1, DateUnit::Year).unwrap(),
        d("2029-02-28")
    );
    assert_eq!(
        shift(d("2028-02-29"), -1, DateUnit::Year).unwrap(),
        d("2027-02-28")
    );
}

#[test]
fn month_period_ends_on_the_last_day() {
    let end = |base: &str| {
        run(base, None, DateUnit::Month, WeekStart::Monday, TOKYO)["end"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(end("2026-02-10"), "2026-02-28");
    assert_eq!(end("2028-02-10"), "2028-02-29");
    assert_eq!(end("2026-04-30"), "2026-04-30");
    assert_eq!(end("2026-12-01"), "2026-12-31");
    // 先月（3 月 31 日から 1 か月前）は 2 月 1 日〜28 日。
    let o = run(
        "2026-03-31",
        Some((-1, DateUnit::Month)),
        DateUnit::Month,
        WeekStart::Monday,
        TOKYO,
    );
    assert_eq!(
        (o["start"].as_str(), o["end"].as_str()),
        (Some("2026-02-01"), Some("2026-02-28"))
    );
    assert_eq!(o["start_at"], "2026-02-01T00:00:00+09:00");
    assert_eq!(o["next_start_at"], "2026-03-01T00:00:00+09:00");
    assert_eq!(
        (o["year"].as_i64(), o["month"].as_i64()),
        (Some(2026), Some(2))
    );
}

#[test]
fn week_starts_on_monday_or_sunday() {
    // 2026-11-11 は水曜。
    let week = |base: &str, ws: WeekStart| {
        let o = run(base, None, DateUnit::Week, ws, TOKYO);
        (
            o["start"].as_str().unwrap().to_string(),
            o["end"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(
        week("2026-11-11", WeekStart::Monday),
        ("2026-11-09".into(), "2026-11-15".into())
    );
    assert_eq!(
        week("2026-11-11", WeekStart::Sunday),
        ("2026-11-08".into(), "2026-11-14".into())
    );
    // 週の初めの日はその週の最初の日。日曜は、月曜始まりでは週の最後の日。
    assert_eq!(week("2026-11-09", WeekStart::Monday).0, "2026-11-09");
    assert_eq!(week("2026-11-15", WeekStart::Monday).0, "2026-11-09");
    assert_eq!(week("2026-11-15", WeekStart::Sunday).0, "2026-11-15");
    // 先週（月曜 9:00 の実行から）: 前の月曜〜日曜。年をまたぐ週。
    let o = run(
        "2026-11-09T09:00:00+09:00",
        Some((-1, DateUnit::Week)),
        DateUnit::Week,
        WeekStart::Monday,
        TOKYO,
    );
    assert_eq!(
        (o["start"].as_str(), o["end"].as_str()),
        (Some("2026-11-02"), Some("2026-11-08"))
    );
    assert_eq!(o["weekday"], 1);
    assert_eq!(
        week("2027-01-01", WeekStart::Monday),
        ("2026-12-28".into(), "2027-01-03".into())
    );
}

#[test]
fn day_and_year_periods() {
    let o = run(
        "2026-03-01",
        Some((-1, DateUnit::Day)),
        DateUnit::Day,
        WeekStart::Monday,
        TOKYO,
    );
    // 前日（3 月 1 日の前日は 2 月 28 日）。期間が日なら最初と最後の日は同じ日。
    assert_eq!(
        (o["date"].as_str(), o["start"].as_str(), o["end"].as_str()),
        (Some("2026-02-28"), Some("2026-02-28"), Some("2026-02-28"))
    );
    assert_eq!(o["next_start_at"], "2026-03-01T00:00:00+09:00");
    assert_eq!(o["weekday"], 6);
    let o = run("2026-07-15", None, DateUnit::Year, WeekStart::Monday, TOKYO);
    assert_eq!(
        (o["start"].as_str(), o["end"].as_str()),
        (Some("2026-01-01"), Some("2026-12-31"))
    );
}

/// 基準の日時は時間帯の日付として読む（UTC の 15 時は日本時間の翌日）。時差の無い日時と日付でない値は読まない。
#[test]
fn base_is_read_as_a_date_in_the_zone() {
    assert_eq!(
        base_date(&json!("2026-11-01T15:30:00Z"), TOKYO),
        Some(d("2026-11-02"))
    );
    assert_eq!(
        base_date(&json!("2026-11-01T15:30:00Z"), NEW_YORK),
        Some(d("2026-11-01"))
    );
    assert_eq!(
        base_date(&json!(" 2026-11-02 "), NEW_YORK),
        Some(d("2026-11-02"))
    );
    assert_eq!(base_date(&json!("2026-11-02T09:00:00"), TOKYO), None);
    assert_eq!(base_date(&json!("2026/11/02"), TOKYO), None);
    assert_eq!(base_date(&json!(20_261_102), TOKYO), None);
    assert_eq!(base_date(&Value::Null, TOKYO), None);
}

/// 夏時間の始まり（ニューヨークは 2026-03-08 の 2 時が 3 時になる）をまたいでも、前日は暦の前日。
/// 時刻から 24 時間を引くと 3 月 7 日の 23:30 になり、日付が 2 日前にずれる。
#[test]
fn daylight_saving_does_not_shift_the_date() {
    let o = run(
        "2026-03-09T00:30:00-04:00",
        Some((-1, DateUnit::Day)),
        DateUnit::Day,
        WeekStart::Monday,
        NEW_YORK,
    );
    assert_eq!(o["date"], "2026-03-08");
    // 3 月 8 日は 23 時間の日（始まりは標準時、次の日の始まりは夏時間）。
    assert_eq!(o["start_at"], "2026-03-08T00:00:00-05:00");
    assert_eq!(o["next_start_at"], "2026-03-09T00:00:00-04:00");
    // 夏時間の終わり（2026-11-01 の 2 時が 1 時に戻る）は 25 時間の日。
    let o = run(
        "2026-11-01T23:30:00-05:00",
        None,
        DateUnit::Day,
        WeekStart::Monday,
        NEW_YORK,
    );
    assert_eq!(o["date"], "2026-11-01");
    assert_eq!(o["start_at"], "2026-11-01T00:00:00-04:00");
    assert_eq!(o["next_start_at"], "2026-11-02T00:00:00-05:00");
    // 夏時間をまたぐ週（日曜始まり）と月も、暦の日付で区切る。
    let o = run(
        "2026-03-11",
        None,
        DateUnit::Week,
        WeekStart::Sunday,
        NEW_YORK,
    );
    assert_eq!(
        (o["start"].as_str(), o["end"].as_str()),
        (Some("2026-03-08"), Some("2026-03-14"))
    );
    let o = run(
        "2026-03-31T23:00:00-04:00",
        Some((1, DateUnit::Week)),
        DateUnit::Month,
        WeekStart::Monday,
        NEW_YORK,
    );
    assert_eq!(o["date"], "2026-04-07");
    assert_eq!(o["start_at"], "2026-04-01T00:00:00-04:00");
}

/// 0 時が無い日（サンティアゴは 2026-09-06 の 0 時が 1 時になる）の始まりは、その日の最初にある時刻。
#[test]
fn a_day_without_midnight_starts_at_its_first_time() {
    let santiago: Tz = chrono_tz::America::Santiago;
    let start = day_start(santiago, d("2026-09-06")).unwrap();
    assert_eq!(start.to_rfc3339(), "2026-09-06T01:00:00-03:00");
    let prev = day_start(santiago, d("2026-09-05")).unwrap();
    assert_eq!(prev.to_rfc3339(), "2026-09-05T00:00:00-04:00");
}

#[test]
fn dates_stay_within_years_1_to_9999() {
    assert_eq!(
        calc(
            d("9999-12-30"),
            Some((5, DateUnit::Day)),
            DateUnit::Day,
            WeekStart::Monday,
            TOKYO
        ),
        Err(OUT_OF_RANGE)
    );
    // 最後の日を含む期間の次の始まりが 10000 年になるものも範囲の外。
    assert_eq!(
        calc(
            d("9999-12-31"),
            None,
            DateUnit::Day,
            WeekStart::Monday,
            TOKYO
        ),
        Err(OUT_OF_RANGE)
    );
    assert_eq!(
        calc(
            d("0001-01-02"),
            Some((-1, DateUnit::Year)),
            DateUnit::Day,
            WeekStart::Monday,
            TOKYO
        ),
        Err(OUT_OF_RANGE)
    );
    assert_eq!(base_date(&json!("0000-12-31"), TOKYO), None);
    assert!(calc(
        d("2026-01-01"),
        Some((10_000, DateUnit::Day)),
        DateUnit::Day,
        WeekStart::Monday,
        TOKYO
    )
    .is_ok());
}
