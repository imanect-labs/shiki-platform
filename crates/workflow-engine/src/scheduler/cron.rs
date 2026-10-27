//! cron 評価（5 フィールド＋IANA tz・engine.md §5.2）。
//!
//! IR の cron は 5 フィールド（min hour dom month dow）で、**標準の読み方**をする（曜日は 0 と 7 が日曜、
//! 1 が月曜〜6 が土曜。名前 SUN〜SAT も書ける・BUG-95・D-81）。cron クレート（0.15）は 7 フィールド
//! （sec min hour dom month dow year）で、曜日を 1 が日曜〜7 が土曜と読む。そこで曜日の欄を標準の読み方で
//! 曜日の集合に直し、クレートの番号の列挙へ写してから、秒を `0`・年を `*` で補って解釈する。
//!
//! 保存時の検証（[`validate`]）・次の実行の見通し（[`next_occurrences`]・検証 API）・発火（[`occurrences_between`]・
//! [`latest_occurrence_between`]）は、どれもこのファイルの `parse_schedule` を通る（読み方を 1 か所で決める）。

use chrono::{DateTime, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;
use std::str::FromStr;

/// cron/tz のパースエラー（`Display` は調べる人向けの原文。利用者には [`CronError::user_message`] を出す）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CronError {
    #[error("不正な cron 式: {0}")]
    BadCron(String),
    /// 曜日の欄を標準の読み方で読めない（範囲の逆転・番号の外・L や # のような拡張）。
    #[error("曜日の欄を読めません: {0}")]
    BadDayOfWeek(String),
    #[error("不正なタイムゾーン: {0}")]
    BadTz(String),
}

impl CronError {
    /// 利用者に出す文言（付録 D の N4.3。英語の原文は埋め込まない）。
    #[must_use]
    pub fn user_message(&self) -> &'static str {
        match self {
            CronError::BadCron(_) => {
                "スケジュールの書き方が正しくありません。分 時 日 月 曜日 の 5 つを半角スペースで区切ってください。"
            }
            CronError::BadDayOfWeek(_) => {
                "スケジュールの曜日の書き方が正しくありません。曜日は 0〜7（0 と 7 が日曜日、1 が月曜日）か、SUN〜SAT の名前で書いてください。"
            }
            CronError::BadTz(_) => "時間帯を選び直してください。",
        }
    }

    /// 誤りのある欄（`cron` か `tz`）。保存時の誤りの位置（`/triggers/{i}/{欄}`）に使う。
    #[must_use]
    pub fn field(&self) -> &'static str {
        match self {
            CronError::BadCron(_) | CronError::BadDayOfWeek(_) => "cron",
            CronError::BadTz(_) => "tz",
        }
    }
}

/// 標準の曜日の名前（0＝日曜）。cron クレートが受け付けてきた長い名前も受ける（保存済みの式を壊さない）。
fn weekday_from_name(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "sun" | "sunday" => 0,
        "mon" | "monday" => 1,
        "tue" | "tues" | "tuesday" => 2,
        "wed" | "wednesday" => 3,
        "thu" | "thurs" | "thursday" => 4,
        "fri" | "friday" => 5,
        "sat" | "saturday" => 6,
        _ => return None,
    })
}

/// 曜日の欄の 1 つの値（数 0〜7 か名前）を標準の番号（0〜7）にする。7 は畳む前の日曜。
fn weekday_point(s: &str, field: &str) -> Result<u32, CronError> {
    let bad = || CronError::BadDayOfWeek(field.to_string());
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        let n: u32 = s.parse().map_err(|_| bad())?;
        return if n <= 7 { Ok(n) } else { Err(bad()) };
    }
    weekday_from_name(s).ok_or_else(bad)
}

/// 標準の曜日の欄を、曜日の集合（ビット 0＝日曜〜6＝土曜）にする。
///
/// 数・範囲（`1-5`・`MON-FRI`・`MON-5` の混在も）・列挙（`,`）・刻み（`*/2`・`1-5/2`・`1/2`）・`*`・`?` を扱う。
/// 刻みの始まりだけの `a/n` は `a` から 7 まで（cron クレートと web の cron-parser と同じ「最大まで」）。
/// 7 は日曜に畳む。範囲の逆転（`5-1`・`SAT-SUN`）・番号の外・`L` や `#` の拡張は写せないので誤り。
fn standard_weekdays(field: &str) -> Result<u8, CronError> {
    let bad = || CronError::BadDayOfWeek(field.to_string());
    let mut set: u8 = 0;
    for part in field.split(',') {
        let (base, step) = match part.split_once('/') {
            Some((b, s)) => {
                let step: u32 = s.parse().map_err(|_| bad())?;
                if !(1..=7).contains(&step) {
                    return Err(bad());
                }
                (b, Some(step))
            }
            None => (part, None),
        };
        let (lo, hi) = if base == "*" || base == "?" {
            (0, 7)
        } else if let Some((a, b)) = base.split_once('-') {
            let (a, b) = (weekday_point(a, field)?, weekday_point(b, field)?);
            if a > b {
                return Err(bad());
            }
            (a, b)
        } else {
            let a = weekday_point(base, field)?;
            // 刻みの無い 1 点はその曜日だけ。刻みつきの 1 点は最大（7）まで。
            (a, if step.is_some() { 7 } else { a })
        };
        let mut d = lo;
        while d <= hi {
            set |= 1 << (d % 7);
            d += step.unwrap_or(1);
        }
    }
    Ok(set)
}

/// 標準の曜日の欄を cron クレートの曜日の欄（1＝日曜〜7＝土曜の列挙）へ写す。全曜日なら `*`。
fn crate_day_of_week(field: &str) -> Result<String, CronError> {
    if field == "*" || field == "?" {
        return Ok("*".to_string());
    }
    let set = standard_weekdays(field)?;
    if set == 0b111_1111 {
        return Ok("*".to_string());
    }
    Ok((0..7u32)
        .filter(|d| set & (1 << d) != 0)
        .map(|d| (d + 1).to_string())
        .collect::<Vec<_>>()
        .join(","))
}

/// 5 フィールド cron（標準の読み方）を 7 フィールド（秒 0・年 *・曜日はクレートの番号）へ写して
/// [`cron::Schedule`] を作る。
fn parse_schedule(cron5: &str) -> Result<cron::Schedule, CronError> {
    let fields: Vec<&str> = cron5.split_whitespace().collect();
    let [minute, hour, dom, month, dow] = fields.as_slice() else {
        return Err(CronError::BadCron(format!(
            "5 フィールド必須（受信 {} フィールド）",
            fields.len()
        )));
    };
    let dow = crate_day_of_week(dow)?;
    let seven = format!("0 {minute} {hour} {dom} {month} {dow} *");
    cron::Schedule::from_str(&seven).map_err(|e| CronError::BadCron(format!("{e}")))
}

/// IANA タイムゾーンをパースする。
fn parse_tz(tz: &str) -> Result<Tz, CronError> {
    Tz::from_str(tz).map_err(|_| CronError::BadTz(tz.to_string()))
}

/// DST の終わりで同じ壁時計の時刻が 2 回ある（`LocalResult::Ambiguous`）とき、**後の方**か。
///
/// ir.md §6.1「二重時刻は先勝ち」: cron クレートは重なった時刻を両方返すので、後の方を捨てる
/// （捨てないと 02:30 のような毎日のジョブが DST 明けの日に 2 回走る・occurrence の UNIQUE は
/// UTC の時刻が違うので止めない）。
fn is_repeated_wall_time(dt: &DateTime<Tz>) -> bool {
    match dt.timezone().from_local_datetime(&dt.naive_local()) {
        LocalResult::Ambiguous(earliest, _) => *dt != earliest,
        _ => false,
    }
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
    for dt in schedule
        .after(&after_tz)
        .filter(|d| !is_repeated_wall_time(d))
        .take(limit.clamp(1, 10_000))
    {
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

/// `(after, now]` 区間の**最新** occurrence（UTC）を返す（catchup=skip 用・固定 cap 無し）。
///
/// 長時間ダウンで occurrence が多数あっても now 以下の最新を取りこぼさない。区間内反復は
/// 安全弁（100 万）で頭打ちにする（極端な設定でも暴走しない）。
pub fn latest_occurrence_between(
    cron5: &str,
    tz: &str,
    after: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, CronError> {
    let schedule = parse_schedule(cron5)?;
    let zone = parse_tz(tz)?;
    let after_tz = after.with_timezone(&zone);
    let mut latest = None;
    for dt in schedule
        .after(&after_tz)
        .filter(|d| !is_repeated_wall_time(d))
        .take(1_000_000)
    {
        let utc = dt.with_timezone(&Utc);
        if utc > now {
            break;
        }
        if utc > after {
            latest = Some(utc);
        }
    }
    Ok(latest)
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
        .find(|d| !is_repeated_wall_time(d))
        .map(|d| d.with_timezone(&Utc)))
}

/// `after` より後の実行の時刻を、近い順に `count` 個まで返す（UTC・画面の「次の実行」の見通し）。
///
/// 発火（[`occurrences_between`]）と同じ読み方・同じ二重時刻の扱いで数える（画面とサーバーで曜日がずれない）。
pub fn next_occurrences(
    cron5: &str,
    tz: &str,
    after: DateTime<Utc>,
    count: usize,
) -> Result<Vec<DateTime<Utc>>, CronError> {
    let schedule = parse_schedule(cron5)?;
    let zone = parse_tz(tz)?;
    let after_tz = after.with_timezone(&zone);
    Ok(schedule
        .after(&after_tz)
        .filter(|d| !is_repeated_wall_time(d))
        .take(count.clamp(1, 10))
        .map(|d| d.with_timezone(&Utc))
        .collect())
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

    /// DST の終わり（Europe/Berlin 2026-10-25）の 02:30 は 2 回あるが、先の 1 回だけ（ir.md §6.1）。
    #[test]
    fn repeated_wall_time_fires_once_at_dst_end() {
        let after = utc(2026, 10, 24, 22, 0); // 00:00 CEST
        let now = utc(2026, 10, 25, 4, 0);
        let occ = occurrences_between("30 2 * * *", "Europe/Berlin", after, now, 100).unwrap();
        assert_eq!(occ, vec![utc(2026, 10, 25, 0, 30)], "02:30 CEST（先）だけ");
        // 先の 02:30 の直後を watermark にした tick でも、後の 02:30 CET（01:30 UTC）は発火しない。
        let late =
            latest_occurrence_between("30 2 * * *", "Europe/Berlin", utc(2026, 10, 25, 0, 31), now)
                .unwrap();
        assert_eq!(late, None);
        assert_eq!(
            next_after("30 2 * * *", "Europe/Berlin", utc(2026, 10, 25, 0, 31)).unwrap(),
            Some(utc(2026, 10, 26, 1, 30)),
            "次は翌日の 02:30 CET"
        );
    }

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

    /// 日本時間の 2026-10-25（日）0:00 からの 1 週間に、式が実行する曜日（0＝日曜〜6＝土曜・実行の順）。
    fn weekdays(cron5: &str) -> Vec<u32> {
        use chrono::Datelike;
        let zone: Tz = "Asia/Tokyo".parse().unwrap();
        let after = utc(2026, 10, 24, 14, 59); // 10/24（土）23:59 JST
        let now = utc(2026, 10, 31, 14, 59); // 10/31（土）23:59 JST
        occurrences_between(cron5, "Asia/Tokyo", after, now, 100)
            .unwrap()
            .iter()
            .map(|t| t.with_timezone(&zone).weekday().num_days_from_sunday())
            .collect()
    }

    /// 曜日は標準の読み方（0 と 7 が日曜、1 が月曜〜6 が土曜・名前も）。cron クレートの番号（1＝日曜）で読まない（BUG-95）。
    #[test]
    fn day_of_week_uses_the_standard_numbering() {
        // 平日（AI の説明文の例・画面の「平日」）。クレートの番号で読むと日曜〜木曜になる。
        assert_eq!(weekdays("0 9 * * 1-5"), [1, 2, 3, 4, 5]);
        assert_eq!(weekdays("0 9 * * MON-FRI"), [1, 2, 3, 4, 5]);
        // 月曜（画面の「毎週・月曜日」）は月曜。日曜は 0 でも 7 でも SUN でも日曜で、保存できる。
        assert_eq!(weekdays("0 9 * * 1"), [1]);
        assert_eq!(weekdays("0 9 * * MON"), [1]);
        for sunday in ["0", "7", "SUN", "sun", "Sunday"] {
            assert_eq!(weekdays(&format!("0 9 * * {sunday}")), [0], "{sunday}");
            assert!(validate(&format!("0 9 * * {sunday}"), "Asia/Tokyo").is_ok());
        }
        assert_eq!(weekdays("0 9 * * 6"), [6]);
        // 列挙と名前の混在・範囲の端の名前。
        assert_eq!(weekdays("0 9 * * mon,3,FRI"), [1, 3, 5]);
        assert_eq!(weekdays("0 9 * * MON-3"), [1, 2, 3]);
        assert_eq!(weekdays("0 9 * * SAT,SUN"), [0, 6]);
        // 7 を含む範囲（金〜日）と全曜日。
        assert_eq!(weekdays("0 9 * * 5-7"), [0, 5, 6]);
        assert_eq!(weekdays("0 9 * * 0-6"), [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(weekdays("0 9 * * 0-7"), [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(weekdays("0 9 * * ?"), [0, 1, 2, 3, 4, 5, 6]);
        // 刻み: */2 は日・火・木・土。範囲の刻み。始まりだけの刻みは 7（日曜）まで（cron-parser と同じ）。
        assert_eq!(weekdays("0 9 * * */2"), [0, 2, 4, 6]);
        assert_eq!(weekdays("0 9 * * 1-5/2"), [1, 3, 5]);
        assert_eq!(weekdays("0 9 * * 1/2"), [0, 1, 3, 5]);
        assert_eq!(weekdays("0 9 * * MON/3"), [0, 1, 4]);
        // 曜日以外の欄はそのまま（毎時 30 分の平日）。
        let every = occurrences_between(
            "30 * * * 1-5",
            "UTC",
            utc(2026, 10, 25, 0, 0),
            utc(2026, 10, 26, 2, 0),
            100,
        )
        .unwrap();
        assert_eq!(
            every,
            [utc(2026, 10, 26, 0, 30), utc(2026, 10, 26, 1, 30)],
            "日曜は動かず、月曜 0:30 から"
        );
    }

    /// 標準の読み方で写せない曜日の欄は、保存時の誤りにする（黙ってクレートの読み方で動かさない）。
    #[test]
    fn unmappable_day_of_week_is_rejected() {
        for bad in [
            "5-1", "SAT-SUN", "8", "1-8", "*/0", "*/8", "1#2", "5L", "L", "MON-", ",", "1,,2",
            "XYZ", "1-5/x",
        ] {
            let err = validate(&format!("0 9 * * {bad}"), "UTC").unwrap_err();
            assert!(matches!(err, CronError::BadDayOfWeek(_)), "{bad}: {err:?}");
            assert_eq!(err.field(), "cron");
            assert!(err.user_message().contains("SUN〜SAT"), "{bad}");
        }
        let tz = validate("0 9 * * *", "Bogus/Zone").unwrap_err();
        assert_eq!(
            (tz.field(), tz.user_message()),
            ("tz", "時間帯を選び直してください。")
        );
        assert!(!CronError::BadCron("x".into())
            .user_message()
            .contains("cron"));
    }

    /// 画面の「次の実行」は発火と同じ関数で数える（曜日がずれない・BUG-95）。
    #[test]
    fn next_occurrences_match_firing() {
        // 2026-10-25（日）12:00 JST の後の「毎週・月曜日 9:00」。
        let after = utc(2026, 10, 25, 3, 0);
        let next = next_occurrences("0 9 * * MON", "Asia/Tokyo", after, 2).unwrap();
        assert_eq!(next, [utc(2026, 10, 26, 0, 0), utc(2026, 11, 2, 0, 0)]);
        let fired = occurrences_between(
            "0 9 * * MON",
            "Asia/Tokyo",
            after,
            utc(2026, 11, 2, 0, 0),
            10,
        )
        .unwrap();
        assert_eq!(fired, next);
        assert_eq!(
            next_occurrences("0 9 * * 1", "Asia/Tokyo", after, 2).unwrap(),
            next
        );
        assert!(matches!(
            next_occurrences("0 9 * * 5-1", "Asia/Tokyo", after, 2),
            Err(CronError::BadDayOfWeek(_))
        ));
    }
}
