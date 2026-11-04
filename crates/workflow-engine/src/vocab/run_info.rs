//! 実行情報（`$from run`）の項目の単一定義（ir.md §3.1・D-97）。
//!
//! 値を組む関数（[`run_info`]・`$from run` の源）と項目の表（[`RUN_INFO_FIELDS`]）をここだけに置く。
//! 表は `bin/export-ts.rs` が `RUN_INFO_CATALOG` として web へ書き出し（値の欄の候補）、AI の説明文
//! （chat の `workflow_tool::guide`）も同じ表から作る。値と表は単体テストで突き合わせる。
//!
//! 日時は実行を始めた時刻で決める（同じ実行の中では、どのブロックが何度やり直しても同じ値）。
//! ファイル名に差し込んでも、やり直しで別の名前にならない（冪等キーの照合が崩れない）。
//!
//! 予定の日時（`scheduled_at`）は、スケジュールで動いた run ではスケジュールの予定の時刻（止まっていたあとの発火や、
//! 多重度で待たされた run でも予定の時刻のまま）、ほかの run では実行を始めた日時（`started_at` と同じ）。「先週」
//! 「前日」のような日付を、どの実行でも同じ項目から数えられるようにする（「日付を計算」の基準の既定・GAP-15）。
//! スケジュールの run の入力ときっかけの内容は今までどおり null のまま（保存済みの定義の値を変えない）。
//!
//! 実行した人（`started_by`）は、人が手動で実行した run の実行主体（ユーザーの ID）。申請者を承認者の表から
//! 引くような組み方に使う（GAP-17・D-101）。入力項目で申請者を受け取ると、実行する人が別の人の ID を入れて
//! 偽れるが、これは実行主体から決まるので偽れない。スケジュール・きっかけ（ワークフロー主体）やミニアプリの
//! 自動化（サービスの主体）で動いた run には人がいないので、項目ごと入れない（参照すると、値がないときの
//! 代わりの値が無ければブロックが失敗する。黙って別の人や空の値で探さない）。

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
    /// 補足（値が入らない場合など・利用者と AI の説明に添える・無ければ空）。
    pub note_ja: &'static str,
}

/// 実行情報の項目（表示の順）。
pub const RUN_INFO_FIELDS: &[RunInfoField] = &[
    RunInfoField {
        name: "date",
        ty: "string",
        description_ja: "実行した日",
        example: "2026-10-28",
        suggested: true,
        note_ja: "",
    },
    RunInfoField {
        name: "started_at",
        ty: "string",
        description_ja: "実行した日時",
        example: "2026-10-28T09:00:00+09:00",
        suggested: true,
        note_ja: "",
    },
    RunInfoField {
        name: "scheduled_at",
        ty: "string",
        description_ja: "予定の日時",
        example: "2026-11-09T09:00:00+09:00",
        suggested: true,
        note_ja: "スケジュールで動いた実行では予定の時刻（遅れて動いても変わらない）。手動の実行やきっかけで動いた実行では、実行した日時と同じ",
    },
    RunInfoField {
        name: "started_by",
        ty: "string",
        description_ja: "実行した人",
        example: "",
        suggested: true,
        note_ja: "手動で実行した人のユーザー ID（メンバーの列と比べられる）。スケジュールやきっかけで動いた実行には入らない",
    },
    RunInfoField {
        name: "run_id",
        ty: "string",
        description_ja: "実行の ID",
        example: "",
        suggested: false,
        note_ja: "",
    },
    RunInfoField {
        name: "tenant_id",
        ty: "string",
        description_ja: "契約全体の ID",
        example: "",
        suggested: false,
        note_ja: "",
    },
];

/// 実行情報の値（`$from run` の源）。`started_at` は実行を始めた時刻（UTC）。`scheduled_at` はスケジュールの予定の
/// 時刻（スケジュールの run だけ・`None` なら始めた時刻）。`started_by` は人が手動で実行した run のユーザーの ID
/// （人のいない run は `None` で、項目ごと入れない）。
#[must_use]
pub fn run_info(
    run_id: Uuid,
    tenant_id: &str,
    started_at: DateTime<Utc>,
    scheduled_at: Option<DateTime<Utc>>,
    started_by: Option<&str>,
) -> Value {
    let local = started_at.with_timezone(&RUN_INFO_TIMEZONE);
    let scheduled = scheduled_at
        .unwrap_or(started_at)
        .with_timezone(&RUN_INFO_TIMEZONE);
    let mut v = json!({
        "date": local.format("%Y-%m-%d").to_string(),
        "started_at": local.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        "scheduled_at": scheduled.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        "run_id": run_id.to_string(),
        "tenant_id": tenant_id,
    });
    if let (Some(by), Some(o)) = (started_by, v.as_object_mut()) {
        o.insert("started_by".into(), Value::String(by.to_string()));
    }
    v
}

/// 実行した人（[`run_info`] の `started_by`）: 実行主体がユーザーのとき（手動の実行・その中から始めた別の
/// ワークフロー）だけ、そのユーザーの ID。ワークフロー主体（スケジュール・きっかけ）とサービスの主体は `None`。
#[must_use]
pub fn started_by<'a>(principal: &'a str, principal_kind: &str) -> Option<&'a str> {
    (principal_kind == "user" && !principal.is_empty()).then_some(principal)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 値の項目と表の項目は同じ（片方だけを変えると落ちる）。
    #[test]
    fn fields_match_the_values() {
        let v = run_info(Uuid::nil(), "t", Utc::now(), None, Some("alice"));
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
        let v = run_info(Uuid::nil(), "t", at, None, None);
        assert_eq!(v["date"], "2026-10-28");
        assert_eq!(v["started_at"], "2026-10-28T00:30:05+09:00");
    }

    /// 予定の日時は、スケジュールの run では予定の時刻（遅れて始めても変わらない）、ほかの run では始めた日時
    /// （GAP-15）。どちらも日本時間で言う。
    #[test]
    fn scheduled_at_is_the_planned_time_or_the_start() {
        let planned = DateTime::parse_from_rfc3339("2026-11-09T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // 止まっていたあとの発火で、翌日に始めた。
        let started = DateTime::parse_from_rfc3339("2026-11-10T01:15:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let v = run_info(Uuid::nil(), "t", started, Some(planned), None);
        assert_eq!(v["scheduled_at"], "2026-11-09T09:00:00+09:00");
        assert_eq!(v["started_at"], "2026-11-10T10:15:00+09:00");
        assert_eq!(v["date"], "2026-11-10");
        // 手動の実行・きっかけの実行（予定の時刻が無い）は始めた日時。
        let v = run_info(Uuid::nil(), "t", started, None, Some("alice"));
        assert_eq!(v["scheduled_at"], v["started_at"]);
    }

    /// 実行した人は、人が実行した run（ユーザーの主体）だけに入る。人のいない run には項目ごと入れない
    /// （参照は値がないとして失敗し、黙って空の値で探さない・D-101）。
    #[test]
    fn started_by_is_only_for_runs_a_person_started() {
        assert_eq!(started_by("alice", "user"), Some("alice"));
        assert_eq!(started_by("wf-1", "workflow"), None);
        assert_eq!(started_by("app-1", "miniapp"), None);
        assert_eq!(started_by("", "user"), None);
        let v = run_info(
            Uuid::nil(),
            "t",
            Utc::now(),
            None,
            started_by("alice", "user"),
        );
        assert_eq!(v["started_by"], "alice");
        let v = run_info(
            Uuid::nil(),
            "t",
            Utc::now(),
            None,
            started_by("wf-1", "workflow"),
        );
        assert!(v.get("started_by").is_none(), "{v}");
    }
}
