//! 実行の予定の日時（`workflow_run.scheduled_at`・`$from run` の `scheduled_at` の源）の由来と、手動の実行で試す値の
//! 読み方（GAP-15・GAP-88・GAP-89・GAP-90）。
//!
//! run の行の予定の日時は、次の 3 つのどれかで入る（ほかの run は空で、実行情報では始めた日時と同じ値になる）。
//!
//! - スケジュールの発火: その回の予定の時刻（止まっていた後の発火や、多重度で待たされた run でも変わらない）。
//! - 手動の実行で指定した日時: 実行のダイアログで試す値（D-93 のきっかけの内容と同じく、手動の実行だけが受ける）と、
//!   実行履歴の「同じ入力でもう一度実行」が元の run から引き継ぐ値。
//! - 「別のワークフローを実行」で始めた子の run: 始めた run（親）の予定の日時を引き継ぐ（親に無ければ空）。
//!
//! 由来は列に持たず、run の行のきっかけの種類と親の run の有無から導く（[`SCHEDULED_FROM`]）。

use chrono::{DateTime, Datelike, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::vocab::RUN_INFO_TIMEZONE;

/// run の予定の日時の由来（実行履歴の一覧と詳細が、どの予定の分の実行か・試した値かを言い分ける・GAP-89）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledAtSource {
    /// スケジュールの予定の時刻（スケジュールで動いた実行）。
    Schedule,
    /// 手動の実行で指定した日時（実行のダイアログで試した値か、「同じ入力でもう一度実行」で元の実行から引き継いだ値）。
    Specified,
    /// 「別のワークフローを実行」で始めた実行が、始めた実行から引き継いだ日時。
    Parent,
}

impl ScheduledAtSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ScheduledAtSource::Schedule => "schedule",
            ScheduledAtSource::Specified => "specified",
            ScheduledAtSource::Parent => "parent",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "schedule" => ScheduledAtSource::Schedule,
            "specified" => ScheduledAtSource::Specified,
            "parent" => ScheduledAtSource::Parent,
            _ => return None,
        })
    }
}

/// run の予定の日時の由来（別名 `r` の `workflow_run` に対する式・[`ScheduledAtSource::as_str`] の値か NULL）。
///
/// 予定の日時を書くのは、スケジュールの発火（きっかけは `schedule`）、手動の実行で指定した値（親の run が無い）、
/// 子の run の引き継ぎ（親の run がある）だけ。イベントの run は予定の日時を持たない（親の run の印 `cause_run_id`
/// を持つことはある）。
pub(crate) const SCHEDULED_FROM: &str = "CASE \
     WHEN r.scheduled_at IS NULL THEN NULL \
     WHEN r.trigger_kind = 'schedule' THEN 'schedule' \
     WHEN r.cause_run_id IS NOT NULL THEN 'parent' \
     ELSE 'specified' END";

/// 試す予定の日時が読めないときの文（実行のダイアログが欄の下に出す・API の 400 の message）。
pub const TRIAL_SCHEDULED_AT_INVALID: &str =
    "予定の日時は、2026-11-09T09:00 のような日時で入れてください。時差を書かなければ日本時間として読みます。";

/// 手動の実行で試す予定の日時を読む（実行のダイアログの「予定の日時」・GAP-88）。
///
/// 時差つきの日時（RFC 3339）か、時差の無い日時（`YYYY-MM-DDTHH:MM` か `YYYY-MM-DDTHH:MM:SS`・実行情報の時間帯
/// [`RUN_INFO_TIMEZONE`] で読む）を受ける。実行情報は予定の日時をその時間帯の RFC 3339 で言うので、その時間帯で
/// 1〜9999 年に入らない日時は受けない（「日付を計算」が読める範囲と同じ）。時間帯の上で無い時刻（夏時間の飛び）も
/// 受けない。
#[must_use]
pub fn parse_trial_scheduled_at(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    let at = if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        t.with_timezone(&Utc)
    } else {
        let naive = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S")
            .or_else(|_| NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M"))
            .ok()?;
        RUN_INFO_TIMEZONE
            .from_local_datetime(&naive)
            .earliest()?
            .with_timezone(&Utc)
    };
    let year = at.with_timezone(&RUN_INFO_TIMEZONE).year();
    (1..=9999).contains(&year).then_some(at)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// 時差つきはそのまま、時差の無い日時は日本時間で読む（実行のダイアログは時差を付けずに送る）。
    #[test]
    fn trial_scheduled_at_reads_offsets_and_local_times() {
        assert_eq!(
            parse_trial_scheduled_at("2026-11-09T09:00:00+09:00"),
            Some(utc("2026-11-09T00:00:00Z"))
        );
        assert_eq!(
            parse_trial_scheduled_at("2026-11-09T00:00:00Z"),
            Some(utc("2026-11-09T00:00:00Z"))
        );
        assert_eq!(
            parse_trial_scheduled_at(" 2026-11-09T09:00 "),
            Some(utc("2026-11-09T00:00:00Z")),
            "時差の無い日時は日本時間"
        );
        assert_eq!(
            parse_trial_scheduled_at("2026-11-09T09:00:30"),
            Some(utc("2026-11-09T00:00:30Z"))
        );
    }

    /// 日時でないもの・日付だけ・実行情報で言えない年は受けない。
    #[test]
    fn trial_scheduled_at_rejects_what_the_run_info_cannot_say() {
        for bad in [
            "",
            "来週の月曜",
            "2026-11-09",
            "2026/11/09 09:00",
            "2026-13-01T09:00",
            "2026-11-09T25:00",
            // 日本時間で 0 年になる（実行情報の RFC 3339 で言えない）。
            "0000-12-31T00:00:00Z",
            // 日本時間で 10000 年になる。
            "9999-12-31T20:00:00Z",
        ] {
            assert_eq!(parse_trial_scheduled_at(bad), None, "{bad:?}");
        }
        assert!(parse_trial_scheduled_at("0001-01-01T00:00").is_some());
        assert!(parse_trial_scheduled_at("9999-12-31T23:59").is_some());
    }

    /// 由来の式は、名前の決まりを Rust の定義から外さない。
    #[test]
    fn scheduled_from_names_every_source() {
        for source in [
            ScheduledAtSource::Schedule,
            ScheduledAtSource::Specified,
            ScheduledAtSource::Parent,
        ] {
            assert!(
                SCHEDULED_FROM.contains(&format!("'{}'", source.as_str())),
                "{SCHEDULED_FROM}"
            );
            assert_eq!(ScheduledAtSource::parse(source.as_str()), Some(source));
            assert_eq!(
                serde_json::to_value(source).unwrap(),
                serde_json::Value::String(source.as_str().into())
            );
        }
        assert_eq!(ScheduledAtSource::parse("manual"), None);
    }
}
