//! 実行情報（`$from run`）の項目の単一定義（ir.md §3.1・D-97）。
//!
//! 値を組む関数（[`run_info`]・`$from run` の源）と項目の表（[`RUN_INFO_FIELDS`]）をここだけに置く。
//! 表は `bin/export-ts.rs` が `RUN_INFO_CATALOG` として web へ書き出し（値の欄の候補）、AI の説明文
//! （chat の `workflow_tool::guide`）も同じ表から作る。値と表は単体テストで突き合わせる。
//!
//! 日時は実行を始めた時刻で決める（同じ実行の中では、どのブロックが何度やり直しても同じ値）。
//! ファイル名に差し込んでも、やり直しで別の名前にならない（冪等キーの照合が崩れない）。

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use ts_rs::TS;
use uuid::Uuid;

/// 実行情報の日時を言い表すタイムゾーン（日本時間）。
///
/// 組織ごとのタイムゾーンの設定はまだ無い（チャットの「今日の日付」もデプロイの既定は日本時間）。
/// 設定ができたら、ここを実行の組織の値にする。
pub const RUN_INFO_TIMEZONE: chrono_tz::Tz = chrono_tz::Asia::Tokyo;

/// 実行情報の 1 項目（後続が `$from run` の path で読む）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct RunInfoField {
    /// 項目名（path は `/<name>`）。
    pub name: &'static str,
    /// 型（TS 風の表記）。
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub ty: &'static str,
    /// 何が入るか（利用者向けの日本語・値の候補の名前）。
    pub description_ja: &'static str,
    /// 値の例（日時は日本時間・ID は空）。
    pub example: &'static str,
    /// 値の候補に出すか（ID は画面の候補に出さない）。
    pub suggested: bool,
}

/// 実行情報の項目（表示の順）。
pub const RUN_INFO_FIELDS: &[RunInfoField] = &[
    RunInfoField {
        name: "date",
        ty: "string",
        description_ja: "実行した日",
        example: "2026-10-28",
        suggested: true,
    },
    RunInfoField {
        name: "started_at",
        ty: "string",
        description_ja: "実行した日時",
        example: "2026-10-28T09:00:00+09:00",
        suggested: true,
    },
    RunInfoField {
        name: "run_id",
        ty: "string",
        description_ja: "実行の ID",
        example: "",
        suggested: false,
    },
    RunInfoField {
        name: "tenant_id",
        ty: "string",
        description_ja: "契約全体の ID",
        example: "",
        suggested: false,
    },
];

/// 実行情報の値（`$from run` の源）。`started_at` は実行を始めた時刻（UTC）。
#[must_use]
pub fn run_info(run_id: Uuid, tenant_id: &str, started_at: DateTime<Utc>) -> Value {
    let local = started_at.with_timezone(&RUN_INFO_TIMEZONE);
    json!({
        "date": local.format("%Y-%m-%d").to_string(),
        "started_at": local.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        "run_id": run_id.to_string(),
        "tenant_id": tenant_id,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 値の項目と表の項目は同じ（片方だけを変えると落ちる）。
    #[test]
    fn fields_match_the_values() {
        let v = run_info(Uuid::nil(), "t", Utc::now());
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut names: Vec<&str> = RUN_INFO_FIELDS.iter().map(|f| f.name).collect();
        names.sort_unstable();
        assert_eq!(keys, names);
    }

    /// 日時は日本時間で言う（UTC の 15 時は翌日の 0 時）。
    #[test]
    fn dates_are_in_japan_time() {
        let at = DateTime::parse_from_rfc3339("2026-10-27T15:30:05Z")
            .unwrap()
            .with_timezone(&Utc);
        let v = run_info(Uuid::nil(), "t", at);
        assert_eq!(v["date"], "2026-10-28");
        assert_eq!(v["started_at"], "2026-10-28T00:30:05+09:00");
    }
}
