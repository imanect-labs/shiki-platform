//! 「日付を計算」（`transform.date`）の実行（GAP-15）。
//!
//! 値式（基準の日時・ずらす数）を解決し、計算は [`crate::control::date`] の純関数に任せる。能力を呼ばない pure
//! ノードで、外部にもデータにも届かない。基準の日時を省くと、実行情報の予定の日時（`$from run` の `scheduled_at`・
//! スケジュールの run では予定の時刻、ほかでは始めた時刻）から数える。どの誤りも同じ入力では直らないので、やり直さない。

use serde_json::Value;

use crate::control::date::{base_date, calc};
use crate::control::eval::ValueResolver;
use crate::ir::expr::FromRef;
use crate::ir::params::{self, parse_tz, shift_amount, TransformDateParams, MAX_DATE_SHIFT};
use crate::run::{NodeContext, NodeResult};

use super::resolver::{param, ParamResolver};

/// 値の誤り（step の permanent な失敗）。
fn invalid(message: impl Into<String>) -> NodeResult {
    NodeResult::fail("invalid", message, false)
}

/// 「日付を計算」を実行する（上のモジュールの説明）。
pub(super) fn execute(raw: &Value, ctx: &NodeContext) -> NodeResult {
    let p: TransformDateParams = match params::parse(raw) {
        Ok(p) => p,
        Err(e) => return NodeResult::fail("bad_params", format!("日付の設定が不正: {e}"), false),
    };
    let Some(tz) =
        p.tz.as_deref()
            .map_or(Some(chrono_tz::Asia::Tokyo), parse_tz)
    else {
        return invalid("時間帯を選び直してください。");
    };
    let r = ParamResolver::new(ctx);
    let base = match &p.base {
        Some(e) => match param(e, &r, "日付を計算: 基準の日") {
            Ok(v) => v,
            Err(e) => return NodeResult::fail(&e.code, e.message, e.retryable),
        },
        // 省くと実行情報の予定の日時（項目の決まりは vocab の run_info の 1 か所）。
        None => r
            .resolve_from(&FromRef {
                from: "run".into(),
                path: Some("/scheduled_at".into()),
                default: None,
            })
            .unwrap_or(Value::Null),
    };
    let Some(base) = base_date(&base, tz) else {
        return invalid(
            "「基準の日」に、日付（2026-11-02）か時差つきの日時（2026-11-02T09:00:00+09:00）ではない値が入りました。",
        );
    };
    let shift_by = match &p.shift {
        None => None,
        Some(s) => {
            let amount = match param(&s.amount, &r, "日付を計算: ずらす数") {
                Ok(v) => v,
                Err(e) => return NodeResult::fail(&e.code, e.message, e.retryable),
            };
            let Some(n) = shift_amount(&amount) else {
                return invalid(format!(
                    "「ずらす数」に、-{MAX_DATE_SHIFT}〜{MAX_DATE_SHIFT} の整数ではない値が入りました。"
                ));
            };
            Some((n, s.unit))
        }
    };
    match calc(
        base,
        shift_by,
        p.period.unwrap_or_default(),
        p.week_starts_on.unwrap_or_default(),
        tz,
    ) {
        Ok(out) => NodeResult::ok(out.to_value()),
        Err(message) => invalid(message),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::json;
    use uuid::Uuid;

    fn ctx(started: &str, scheduled: Option<&str>, input: Value) -> NodeContext {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        NodeContext {
            tenant_id: "t1".into(),
            org: "acme".into(),
            run_id: Uuid::nil(),
            run_started_at: at(started),
            run_scheduled_at: scheduled.map(at),
            step_path: "d".into(),
            idempotency_key: "wf:t1:0:d".into(),
            attempt: 1,
            principal: "wf".into(),
            principal_kind: "workflow".into(),
            input,
            trigger: Value::Null,
            node_outputs: json!({}),
            each: None,
            trace_id: None,
            scope_ceiling: vec![],
            cancel: crate::run::CancelSignal::default(),
            workflow_id: Uuid::nil(),
            cache: None,
        }
    }

    fn output(r: &NodeResult) -> &Value {
        assert!(r.ok, "{:?}", r.error);
        &r.output
    }

    /// 基準を省くと予定の日時から数える。止まっていたあとに火曜に始めた月曜 9:00 の実行でも、先週は前の月曜〜日曜。
    #[test]
    fn omitted_base_counts_from_the_scheduled_time() {
        let c = ctx(
            "2026-11-10T01:00:00Z",
            Some("2026-11-09T00:00:00Z"),
            Value::Null,
        );
        let r = execute(
            &json!({ "shift": { "amount": -1, "unit": "week" }, "period": "week" }),
            &c,
        );
        let o = output(&r);
        assert_eq!(o["start"], "2026-11-02");
        assert_eq!(o["end"], "2026-11-08");
        assert_eq!(o["start_at"], "2026-11-02T00:00:00+09:00");
        assert_eq!(o["next_start_at"], "2026-11-09T00:00:00+09:00");
        // 手動の実行（予定の時刻が無い）は始めた日時から数える。
        let c = ctx("2026-11-10T01:00:00Z", None, Value::Null);
        let r = execute(&json!({}), &c);
        assert_eq!(output(&r)["date"], "2026-11-10");
    }

    /// ずらす数は前の結果や入力から決められる。整数でなければやり直さない失敗にする。
    #[test]
    fn amount_can_come_from_the_input() {
        let c = ctx(
            "2026-11-10T01:00:00Z",
            None,
            json!({ "lead": 7, "bad": "7" }),
        );
        let shift = |path: &str| json!({ "base": "2026-11-10", "shift": { "amount": { "$from": "input", "path": path }, "unit": "day" } });
        let r = execute(&shift("/lead"), &c);
        assert_eq!(output(&r)["date"], "2026-11-17");
        let r = execute(&shift("/bad"), &c);
        let e = r.error.unwrap();
        assert_eq!((e.code.as_str(), e.retryable), ("invalid", false));
        assert!(e.message.contains("ずらす数"), "{}", e.message);
        // 参照が解決できなければ、黙って 0 にしない。
        let r = execute(&shift("/missing"), &c);
        assert_eq!(r.error.unwrap().code, "expr_resolve_error");
    }

    /// 基準の日時が日付でも日時でもなければ、やり直さない失敗にする（今日などに黙って置き換えない）。
    #[test]
    fn unreadable_base_fails() {
        let c = ctx("2026-11-10T01:00:00Z", None, json!({ "d": "来週" }));
        let r = execute(&json!({ "base": { "$from": "input", "path": "/d" } }), &c);
        let e = r.error.unwrap();
        assert_eq!(e.code, "invalid");
        assert!(e.message.contains("「基準の日」に"), "{}", e.message);
    }
}
