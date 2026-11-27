//! 日付を計算するノード（`transform.date`・GAP-15）の params 契約。
//!
//! 基準の日時（省略すると実行情報の予定の日時・`$from run` の `scheduled_at`）から、日・週・月・年をずらした日と、
//! その日を含む期間（日・週・月・年）の最初の日と最後の日を求める。前日・先週・先月・月末・週の初めを、script を
//! 書かずに作るためのもの（ir.md §3 の値は 3 種のままで、値の式に計算を足さない）。
//!
//! 計算は時間帯（`tz`・既定は日本時間）の暦の日付で行い、時刻を足し引きしない（夏時間のある地域で、日の長さが
//! 23・25 時間の日をまたいでも日付がずれない）。実行は `nodes/date.rs`。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::ParamsIssue;
use crate::ir::expr::ValueExpr;

/// ずらす数の上限（絶対値）。日なら 27 年ほど、年なら 1 万年。計算した日付は 1〜9999 年に収める（実行時に照らす）。
pub const MAX_DATE_SHIFT: i64 = 10_000;

/// 時間帯の既定（日本時間・組織の時間帯の設定はまだ無い・実行情報と同じ）。
pub const DEFAULT_DATE_TZ: &str = "Asia/Tokyo";

/// 日付の単位（ずらす単位と、期間の単位で共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DateUnit {
    /// 日（既定）。
    #[default]
    Day,
    /// 週（7 日）。期間では週の初め（`week_starts_on`）から 7 日。
    Week,
    /// 月。ずらした先にその日が無ければ月末にする（1 月 31 日の 1 か月後は 2 月 28 日か 29 日）。
    Month,
    /// 年。ずらした先にその日が無ければ月末にする（うるう年の 2 月 29 日の 1 年後は 2 月 28 日）。
    Year,
}

/// 週の初めの曜日。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WeekStart {
    /// 月曜（既定）。
    #[default]
    Monday,
    /// 日曜。
    Sunday,
}

/// 日付をずらす量（`amount` は整数・負の数で前へ）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DateShift {
    /// ずらす数（整数・-1 で 1 つ前。前の結果や入力から決めてもよい）。
    pub amount: ValueExpr,
    /// 単位。
    pub unit: DateUnit,
}

/// `transform.date` — 日付を計算する（ir.md §7.13）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct TransformDateParams {
    /// 基準の日時（日付 `YYYY-MM-DD` か、時差つきの日時 RFC 3339）。省略すると実行情報の予定の日時（スケジュールでは
    /// 予定の時刻、手動の実行では指定した日時か実行した日時、「別のワークフローを実行」で動いた実行では元の実行の
    /// 予定の日時、きっかけでは実行した日時）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base: Option<ValueExpr>,
    /// ずらす量（省略すると基準の日のまま）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub shift: Option<DateShift>,
    /// 期間の単位（ずらした日を含む日・週・月・年の最初の日と最後の日を出す・省略は日）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub period: Option<DateUnit>,
    /// 週の初め（期間が週のとき・省略は月曜）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub week_starts_on: Option<WeekStart>,
    /// 時間帯（IANA の名前・省略は日本時間 `Asia/Tokyo`）。基準の日時をこの時間帯の日付として読み、期間の始まりの
    /// 日時もこの時間帯で言う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tz: Option<String>,
}

/// 時間帯の名前を読む（IANA の名前だけ・空や知らない名前は `None`）。
#[must_use]
pub fn parse_tz(name: &str) -> Option<chrono_tz::Tz> {
    name.parse::<chrono_tz::Tz>().ok()
}

/// ずらす数（リテラル・解決した値）を読む（整数で、絶対値が [`MAX_DATE_SHIFT`] まで）。
///
/// 2.0 のような小数点つきの整数（テーブルの数の列の値など）も整数として読む。-1.5 や文字の "1" は読まない。
#[must_use]
pub fn shift_amount(v: &serde_json::Value) -> Option<i64> {
    const MAX_F64: f64 = 10_000.0;
    let n = if let Some(n) = v.as_i64() {
        n
    } else {
        let f = v.as_f64()?;
        if f.fract().abs() > 0.0 || f.abs() > MAX_F64 {
            return None;
        }
        f as i64
    };
    (n.abs() <= MAX_DATE_SHIFT).then_some(n)
}

impl TransformDateParams {
    /// 時間帯の名前と、リテラルで書いたずらす数・基準の日時を保存時に照らす（serde では表せない分）。
    pub fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        let issue = |path: &str, message: String| {
            Err(ParamsIssue {
                path: format!("/params/{path}"),
                message,
            })
        };
        if let Some(tz) = &self.tz {
            if parse_tz(tz).is_none() {
                return issue("tz", "時間帯を選び直してください".to_string());
            }
        }
        if let Some(ValueExpr::Literal(v)) = self.shift.as_ref().map(|s| &s.amount) {
            if shift_amount(v).is_none() {
                return issue(
                    "shift/amount",
                    format!(
                        "「ずらす数」は -{MAX_DATE_SHIFT}〜{MAX_DATE_SHIFT} の整数で書いてください"
                    ),
                );
            }
        }
        if let Some(ValueExpr::Literal(v)) = &self.base {
            let tz = self
                .tz
                .as_deref()
                .and_then(parse_tz)
                .unwrap_or(chrono_tz::Asia::Tokyo);
            if crate::control::date::base_date(v, tz).is_none() {
                return issue(
                    "base",
                    "「基準の日」は、日付（2026-11-02）か時差つきの日時（2026-11-02T09:00:00+09:00）で書いてください"
                        .to_string(),
                );
            }
        }
        Ok(())
    }
}
