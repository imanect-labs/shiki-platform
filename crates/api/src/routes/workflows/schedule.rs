//! スケジュールの「次の実行」の見通し（BUG-95・付録 D の N4.3）。
//!
//! エディタのスケジュールの欄は、次に実行する時刻をここから受け取って出す。ブラウザーで cron を読むと、サーバーと
//! 曜日の読み方がずれる（cron クレートは 1 を日曜と読む・web の cron-parser は 0 を日曜と読む）。保存時の検証と
//! 発火と同じ関数（`workflow_engine::scheduler::cron`）で数え、画面と実際の実行を食い違わせない。

use axum::{extract::State, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{extract::AuthContextExt, state::AppState};

/// 一度に返す時刻の数の上限。
const MAX_COUNT: u32 = 10;

/// スケジュールの式と時間帯。
#[derive(Debug, Deserialize, ToSchema)]
pub struct SchedulePreviewRequest {
    /// 5 つの欄の cron（分 時 日 月 曜日・曜日は 0 と 7 が日曜、1 が月曜、SUN〜SAT も書ける）。
    pub cron: String,
    /// IANA の時間帯（例: Asia/Tokyo）。
    pub tz: String,
    /// 返す時刻の数（省略は 2・最大 10）。
    #[serde(default)]
    pub count: Option<u32>,
}

/// 次の実行の見通し。式を読めないときは `next` が空で、`error` に利用者向けの文言が入る。
#[derive(Debug, Serialize, ToSchema)]
pub struct SchedulePreviewResponse {
    /// 次に実行する時刻（UTC・近い順）。
    pub next: Vec<DateTime<Utc>>,
    /// 式か時間帯を読めないときの文言（保存時の誤りと同じ文）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 読めない欄（`cron` か `tz`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_field: Option<String>,
}

/// 次に実行する時刻を、発火と同じ読み方で数える（保存しない・ログインしていれば誰でも使える）。
///
/// 読めない式は失敗ではなく 200 の本文で返す（エディタが打つたびに出し分けるため・検証 API と同じ）。
#[utoipa::path(
    post,
    path = "/workflows/schedule-preview",
    request_body = SchedulePreviewRequest,
    responses(
        (status = 200, description = "次の実行の見通し（読めない式は error つき）", body = SchedulePreviewResponse),
        (status = 401, description = "未認証"),
    ),
    tag = "workflows"
)]
pub async fn preview_schedule(
    State(_state): State<AppState>,
    AuthContextExt(_ctx): AuthContextExt,
    Json(req): Json<SchedulePreviewRequest>,
) -> Json<SchedulePreviewResponse> {
    Json(preview(&req, Utc::now()))
}

fn preview(req: &SchedulePreviewRequest, now: DateTime<Utc>) -> SchedulePreviewResponse {
    let count = req.count.unwrap_or(2).clamp(1, MAX_COUNT) as usize;
    match workflow_engine::scheduler::cron::next_occurrences(&req.cron, &req.tz, now, count) {
        Ok(next) => SchedulePreviewResponse {
            next,
            error: None,
            error_field: None,
        },
        Err(e) => SchedulePreviewResponse {
            next: Vec::new(),
            error: Some(e.user_message().to_string()),
            error_field: Some(e.field().to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::TimeZone;

    fn req(cron: &str, tz: &str) -> SchedulePreviewRequest {
        SchedulePreviewRequest {
            cron: cron.into(),
            tz: tz.into(),
            count: None,
        }
    }

    /// 「毎週・月曜日」は月曜に、日曜（0）は日曜に出る（サーバーの発火と同じ読み方・BUG-95）。
    #[test]
    fn previews_use_the_standard_weekdays() {
        // 2026-10-25（日）12:00 JST。
        let now = Utc.with_ymd_and_hms(2026, 10, 25, 3, 0, 0).unwrap();
        let monday = preview(&req("0 9 * * MON", "Asia/Tokyo"), now);
        assert_eq!(
            monday.next,
            [
                Utc.with_ymd_and_hms(2026, 10, 26, 0, 0, 0).unwrap(),
                Utc.with_ymd_and_hms(2026, 11, 2, 0, 0, 0).unwrap()
            ]
        );
        assert_eq!(
            preview(&req("0 9 * * 1", "Asia/Tokyo"), now).next,
            monday.next
        );
        let sunday = preview(&req("0 9 * * 0", "Asia/Tokyo"), now);
        assert_eq!(
            sunday.next[0],
            Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap()
        );
        assert!(sunday.error.is_none());
        let many = preview(
            &SchedulePreviewRequest {
                count: Some(99),
                ..req("* * * * *", "UTC")
            },
            now,
        );
        assert_eq!(many.next.len(), MAX_COUNT as usize);
    }

    /// 読めない式・時間帯は、保存時の誤りと同じ文言と欄を返す（英語の原文は出さない）。
    #[test]
    fn unreadable_schedules_say_which_field() {
        let now = Utc::now();
        let bad = preview(&req("0 9 * *", "Asia/Tokyo"), now);
        assert!(bad.next.is_empty());
        assert_eq!(bad.error_field.as_deref(), Some("cron"));
        assert!(bad.error.unwrap().contains("5 つを半角スペース"));
        let dow = preview(&req("0 9 * * 5-1", "Asia/Tokyo"), now);
        assert!(dow.error.unwrap().contains("SUN〜SAT"));
        let tz = preview(&req("0 9 * * *", "Mars/Base"), now);
        assert_eq!(tz.error_field.as_deref(), Some("tz"));
    }
}
