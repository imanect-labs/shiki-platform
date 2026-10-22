//! 監査ログの検索と書き出し（`/org-audit/*`・#634・Task 8.2）。
//!
//! 誰が読めるか（`organization#audit_viewer`）はルート表のポリシー（`AccessPolicy::OrgAuditor`）が
//! 一律に掛ける。範囲（tenant_id ＋ org）は `AuditViewer` が `AuthContext` から取る。
//!
//! 監査ログを読んだこと自体も監査に残す（検索は 1 回ごと・書き出しは毎回）。残せなければ
//! 返さない（AI 活用分析の個人別と同じ fail-closed）。返すのは行の ID・判定・metadata と
//! 操作した人の表示名だけで、ファイル名や本文は解決しない（PIT-65）。

use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::header,
    response::{IntoResponse, Response},
    routing::get,
    Json,
};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveTime, Offset, Utc};
use futures::stream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use storage::{
    audit::{AuditEntry, Decision},
    audit_view::{AuditCursor, AuditEvent, AuditFilter, AuditViewer},
};
use utoipa::{IntoParams, ToSchema};

use super::org_admin_ai::invalid;
use crate::{
    error::ApiError,
    extract::{AuthContextExt, TraceIdExt},
    insights::timer::today_jst,
    server::{AccessPolicy, RouteDecl},
    state::AppState,
};

pub(crate) fn org_audit_route_decls() -> Vec<RouteDecl> {
    let r = RouteDecl::new;
    vec![
        r(
            "/org-audit/events",
            &["GET"],
            AccessPolicy::OrgAuditor,
            || get(list_events),
        ),
        r(
            "/org-audit/export",
            &["GET"],
            AccessPolicy::OrgAuditor,
            || get(export_events),
        ),
    ]
}

/// 期間の上限（日）。
const MAX_RANGE_DAYS: i64 = 366;
/// 1 ページの既定件数。
const DEFAULT_PAGE: i64 = 100;
/// 書き出しの上限件数（これを超える分は書き出さない・期間を分けて書き出してもらう）。
pub const EXPORT_MAX: usize = 100_000;
/// 書き出しで 1 回に DB から読む件数。
const EXPORT_BATCH: i64 = 1_000;
/// 文字列の条件の長さの上限。
const MAX_FIELD_LEN: usize = 256;

/// 検索条件（日付は日本時間・両端含む）。省略は直近 7 日。
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct AuditQuery {
    /// 開始日（YYYY-MM-DD）。
    pub from: Option<NaiveDate>,
    /// 終了日（YYYY-MM-DD）。
    pub to: Option<NaiveDate>,
    /// 操作した人（ユーザー ID か、名簿のメールアドレス）。
    pub actor: Option<String>,
    /// 操作の種類。末尾が `.` なら前方一致（例: `file.`）、それ以外は完全一致。
    pub action: Option<String>,
    /// `allow` か `deny`。
    pub decision: Option<String>,
    /// 対象の種類（`file` など）。
    pub object_type: Option<String>,
    /// 対象の ID。
    pub object_id: Option<String>,
    /// 引用されたチャンク ID（RAG 検索の監査行を逆引きする）。
    pub chunk_id: Option<String>,
    /// trace_id（1 回の要求に紐づく行をまとめて見る）。
    pub trace_id: Option<String>,
    /// 前のページの `next_cursor`。
    pub cursor: Option<String>,
    /// 1 ページの件数（1〜500・既定 100）。
    pub limit: Option<i64>,
    /// 書き出し形式（`csv` か `jsonl`・書き出しのみ）。
    pub format: Option<String>,
}

/// 検索結果の 1 ページ。
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEventsResponse {
    /// 新しい順。
    pub events: Vec<AuditEvent>,
    /// 続きがあるときの次ページの位置。
    pub next_cursor: Option<String>,
}

/// 空白だけの条件は「指定なし」。長すぎる条件は 400。
fn text(v: Option<&String>, name: &str) -> Result<Option<String>, ApiError> {
    match v.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.chars().count() > MAX_FIELD_LEN => {
            Err(invalid(&format!("{name}が長すぎます")))
        }
        Some(s) => Ok(Some(s.to_string())),
    }
}

/// 日本時間の日付の 0 時を UTC にする。
fn jst_midnight(d: NaiveDate) -> DateTime<Utc> {
    (d.and_time(NaiveTime::MIN) - Duration::hours(9)).and_utc()
}

/// 検索条件を組む（期間・判定・長さを検証する）。
fn to_filter(q: &AuditQuery, today: NaiveDate) -> Result<AuditFilter, ApiError> {
    let to = q.to.unwrap_or(today).min(today);
    let from = q.from.unwrap_or(to - Duration::days(6));
    if from > to {
        return Err(invalid("開始日が終了日より後になっています"));
    }
    if (to - from).num_days() >= MAX_RANGE_DAYS {
        return Err(invalid("期間は 1 年以内で指定してください"));
    }
    let decision = text(q.decision.as_ref(), "判定")?;
    if let Some(d) = &decision {
        if d != "allow" && d != "deny" {
            return Err(invalid("判定は allow か deny で指定してください"));
        }
    }
    Ok(AuditFilter {
        from: jst_midnight(from),
        to: jst_midnight(to + Duration::days(1)),
        actor: text(q.actor.as_ref(), "操作した人")?,
        action: text(q.action.as_ref(), "操作の種類")?,
        decision,
        object_type: text(q.object_type.as_ref(), "対象の種類")?,
        object_id: text(q.object_id.as_ref(), "対象の ID")?,
        chunk_id: text(q.chunk_id.as_ref(), "チャンク ID")?,
        trace_id: text(q.trace_id.as_ref(), "trace_id")?,
    })
}

/// 監査に残す検索条件（日付は日本時間のまま）。
fn filter_metadata(q: &AuditQuery, f: &AuditFilter) -> serde_json::Value {
    json!({
        "from": jst_date(f.from),
        "to": jst_date(f.to - Duration::days(1)),
        "actor": f.actor, "action": f.action, "decision": f.decision,
        "object_type": f.object_type, "object_id": f.object_id,
        "chunk_id": f.chunk_id, "trace_id": f.trace_id,
        "format": q.format,
    })
}

fn jst() -> FixedOffset {
    FixedOffset::east_opt(9 * 3600).unwrap_or_else(|| Utc.fix())
}

fn jst_date(t: DateTime<Utc>) -> NaiveDate {
    t.with_timezone(&jst()).date_naive()
}

fn viewer(state: &AppState) -> AuditViewer {
    state.audit.viewer()
}

/// 監査ログを読んだことを記録する。残せなければ読ませない。
async fn record_read(
    state: &AppState,
    ctx: &authz::AuthContext,
    action: &'static str,
    trace: Option<&str>,
    metadata: serde_json::Value,
) -> Result<(), ApiError> {
    let object_id = format!("{}|{}", ctx.tenant_id, ctx.org);
    state
        .audit
        .record(
            ctx,
            AuditEntry {
                action,
                object_type: "organization",
                object_id: &object_id,
                decision: Decision::Allow,
                trace_id: trace,
                metadata,
            },
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, action, "監査ログの閲覧の記録に失敗（結果は返さない）");
            ApiError::Internal("監査ログに記録できなかったため表示を中止しました".into())
        })
}

/// 監査ログを検索する（新しい順）。
#[utoipa::path(
    get, path = "/org-audit/events", params(AuditQuery),
    responses(
        (status = 200, description = "監査ログ（新しい順）", body = AuditEventsResponse),
        (status = 400, description = "条件が不正"),
        (status = 403, description = "監査閲覧者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn list_events(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<AuditQuery>,
) -> Result<Json<AuditEventsResponse>, ApiError> {
    let filter = to_filter(&q, today_jst())?;
    let after = match q.cursor.as_deref().filter(|c| !c.is_empty()) {
        None => None,
        Some(c) => Some(AuditCursor::decode(c).ok_or_else(|| invalid("cursor が不正です"))?),
    };
    let limit = q
        .limit
        .unwrap_or(DEFAULT_PAGE)
        .clamp(1, storage::audit_view::MAX_PAGE);
    let events = viewer(&state)
        .search(&ctx, &filter, after, limit)
        .await
        .map_err(|e| ApiError::Internal(format!("audit search: {e}")))?;
    // 検索 1 回につき 1 行（続きのページは同じ検索なので残さない）。
    if after.is_none() {
        record_read(
            &state,
            &ctx,
            "org_audit.search",
            trace.as_deref(),
            filter_metadata(&q, &filter),
        )
        .await?;
    }
    let next_cursor = (i64::try_from(events.len()).unwrap_or(i64::MAX) == limit)
        .then(|| events.last())
        .flatten()
        .map(|e| {
            AuditCursor {
                created_at: e.created_at,
                id: e.id,
            }
            .encode()
        });
    Ok(Json(AuditEventsResponse {
        events,
        next_cursor,
    }))
}

/// 書き出し形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Csv,
    Jsonl,
}

impl Format {
    fn parse(s: Option<&str>) -> Result<Self, ApiError> {
        match s.unwrap_or("csv") {
            "csv" => Ok(Format::Csv),
            "jsonl" => Ok(Format::Jsonl),
            _ => Err(invalid("形式は csv か jsonl で指定してください")),
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Format::Csv => "text/csv; charset=utf-8",
            Format::Jsonl => "application/x-ndjson; charset=utf-8",
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Jsonl => "jsonl",
        }
    }
}

const CSV_HEADER: &str =
    "id,created_at,actor,actor_name,action,object_type,object_id,decision,trace_id,chained,metadata\r\n";

/// CSV の 1 セル。表計算ソフトで式として実行されうる先頭（= + - @ タブ 改行）は `'` で打ち消し、
/// 常に引用符で囲む（RFC 4180）。
fn csv_cell(s: &str) -> String {
    let neutral = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s.to_string()
    };
    format!("\"{}\"", neutral.replace('"', "\"\""))
}

/// 1 行を書き出し形式の文字列にする（末尾の改行込み）。
fn render(format: Format, e: &AuditEvent) -> String {
    match format {
        Format::Jsonl => {
            let mut line = serde_json::to_string(e).unwrap_or_default();
            line.push('\n');
            line
        }
        Format::Csv => {
            let cells = [
                e.id.to_string(),
                e.created_at.with_timezone(&jst()).to_rfc3339(),
                e.actor.clone(),
                e.actor_name.clone().unwrap_or_default(),
                e.action.clone(),
                e.object_type.clone(),
                e.object_id.clone(),
                e.decision.clone(),
                e.trace_id.clone().unwrap_or_default(),
                e.chained.to_string(),
                e.metadata.to_string(),
            ];
            let mut line = cells
                .iter()
                .map(|c| csv_cell(c))
                .collect::<Vec<_>>()
                .join(",");
            line.push_str("\r\n");
            line
        }
    }
}

/// 書き出しの状態（keyset で `EXPORT_BATCH` 件ずつ読み、上限まで流す）。
struct ExportState {
    viewer: AuditViewer,
    ctx: authz::AuthContext,
    filter: AuditFilter,
    format: Format,
    after: Option<AuditCursor>,
    sent: usize,
    started: bool,
    done: bool,
}

/// 次に流す塊を作る。`None` で終わり。
async fn next_chunk(mut s: ExportState) -> Option<(Result<Bytes, std::io::Error>, ExportState)> {
    if s.done {
        return None;
    }
    let mut out = String::new();
    if !s.started {
        s.started = true;
        if s.format == Format::Csv {
            // Excel が UTF-8 と判定できるよう BOM を付ける。
            out.push('\u{feff}');
            out.push_str(CSV_HEADER);
        }
    }
    let room = EXPORT_MAX.saturating_sub(s.sent);
    let batch = i64::try_from(room)
        .unwrap_or(EXPORT_BATCH)
        .min(EXPORT_BATCH);
    if batch == 0 {
        s.done = true;
        return (!out.is_empty()).then(|| (Ok(Bytes::from(out)), s));
    }
    match s.viewer.search(&s.ctx, &s.filter, s.after, batch).await {
        Ok(rows) => {
            if i64::try_from(rows.len()).unwrap_or(0) < batch {
                s.done = true;
            }
            if let Some(last) = rows.last() {
                s.after = Some(AuditCursor {
                    created_at: last.created_at,
                    id: last.id,
                });
            }
            s.sent += rows.len();
            for e in &rows {
                out.push_str(&render(s.format, e));
            }
            Some((Ok(Bytes::from(out)), s))
        }
        Err(e) => {
            // 途中で失敗したら接続を切る（中途半端なファイルを完了扱いにさせない）。
            tracing::error!(error = %e, "監査ログの書き出しが途中で失敗");
            s.done = true;
            Some((Err(std::io::Error::other("audit export failed")), s))
        }
    }
}

/// 監査ログを書き出す（CSV / JSONL・最大 10 万件・書き出しも監査に残す）。
#[utoipa::path(
    get, path = "/org-audit/export", params(AuditQuery),
    responses(
        (status = 200, description = "監査ログのファイル（attachment）", content_type = "text/csv"),
        (status = 400, description = "条件・形式が不正"),
        (status = 403, description = "監査閲覧者ではない"),
    ),
    security(("session" = [])),
)]
pub async fn export_events(
    State(state): State<AppState>,
    AuthContextExt(ctx): AuthContextExt,
    trace: TraceIdExt,
    Query(q): Query<AuditQuery>,
) -> Result<Response, ApiError> {
    let filter = to_filter(&q, today_jst())?;
    let format = Format::parse(q.format.as_deref())?;
    // 書き出す前に残す（残せなければ書き出さない）。
    record_read(
        &state,
        &ctx,
        "org_audit.export",
        trace.as_deref(),
        filter_metadata(&q, &filter),
    )
    .await?;
    let file_name = format!(
        "audit-log_{}_{}.{}",
        jst_date(filter.from),
        jst_date(filter.to - Duration::days(1)),
        format.ext()
    );
    let init = ExportState {
        viewer: viewer(&state),
        ctx,
        filter,
        format,
        after: None,
        sent: 0,
        started: false,
        done: false,
    };
    let body = Body::from_stream(stream::unfold(init, next_chunk));
    let headers = [
        (header::CONTENT_TYPE, format.content_type().to_string()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{file_name}\""),
        ),
        (header::CACHE_CONTROL, "no-store".to_string()),
    ];
    Ok((headers, body).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default()
    }

    #[test]
    fn default_range_is_last_seven_days_in_jst() {
        let f = to_filter(&AuditQuery::default(), day("2026-10-22")).unwrap_or_default();
        assert_eq!(f.from.to_rfc3339(), "2026-10-15T15:00:00+00:00");
        assert_eq!(f.to.to_rfc3339(), "2026-10-22T15:00:00+00:00");
    }

    #[test]
    fn future_end_is_clamped_to_today() {
        let q = AuditQuery {
            from: Some(day("2026-10-20")),
            to: Some(day("2026-12-31")),
            ..AuditQuery::default()
        };
        let f = to_filter(&q, day("2026-10-22")).unwrap_or_default();
        assert_eq!(jst_date(f.to - Duration::days(1)), day("2026-10-22"));
    }

    #[test]
    fn rejects_bad_ranges_and_values() {
        let today = day("2026-10-22");
        let reversed = AuditQuery {
            from: Some(day("2026-10-21")),
            to: Some(day("2026-10-20")),
            ..AuditQuery::default()
        };
        assert!(to_filter(&reversed, today).is_err());
        let too_long = AuditQuery {
            from: Some(day("2025-01-01")),
            to: Some(day("2026-10-22")),
            ..AuditQuery::default()
        };
        assert!(to_filter(&too_long, today).is_err());
        let bad_decision = AuditQuery {
            decision: Some("maybe".into()),
            ..AuditQuery::default()
        };
        assert!(to_filter(&bad_decision, today).is_err());
        let long_text = AuditQuery {
            actor: Some("a".repeat(MAX_FIELD_LEN + 1)),
            ..AuditQuery::default()
        };
        assert!(to_filter(&long_text, today).is_err());
    }

    #[test]
    fn blank_conditions_are_ignored() {
        let q = AuditQuery {
            actor: Some("  ".into()),
            decision: Some(" deny ".into()),
            ..AuditQuery::default()
        };
        let f = to_filter(&q, day("2026-10-22")).unwrap_or_default();
        assert_eq!(f.actor, None);
        assert_eq!(f.decision.as_deref(), Some("deny"));
    }

    #[test]
    fn format_parse() {
        assert_eq!(Format::parse(None).ok(), Some(Format::Csv));
        assert_eq!(Format::parse(Some("jsonl")).ok(), Some(Format::Jsonl));
        assert!(Format::parse(Some("xlsx")).is_err());
    }

    #[test]
    fn csv_cells_neutralize_formulas_and_quote() {
        assert_eq!(csv_cell("plain"), "\"plain\"");
        assert_eq!(csv_cell("=1+1"), "\"'=1+1\"");
        assert_eq!(csv_cell("@cmd"), "\"'@cmd\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    fn event() -> AuditEvent {
        AuditEvent {
            id: 9,
            created_at: DateTime::parse_from_rfc3339("2026-10-22T00:00:00Z")
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_default(),
            actor: "u1".into(),
            actor_name: Some("田中, 太郎".into()),
            action: "rag.search".into(),
            object_type: "rag_query".into(),
            object_id: "h".into(),
            decision: "allow".into(),
            trace_id: None,
            metadata: json!({"cited_chunk_ids": ["c1"]}),
            chained: false,
        }
    }

    #[test]
    fn csv_row_has_every_column_in_jst() {
        let line = render(Format::Csv, &event());
        assert!(line.ends_with("\r\n"));
        assert!(line.starts_with("\"9\",\"2026-10-22T09:00:00+09:00\",\"u1\",\"田中, 太郎\""));
        assert!(line.contains("\"{\"\"cited_chunk_ids\"\":[\"\"c1\"\"]}\""));
        assert_eq!(CSV_HEADER.matches(',').count(), 10);
    }

    #[test]
    fn jsonl_row_is_one_json_line() {
        let line = render(Format::Jsonl, &event());
        assert!(line.ends_with('\n'));
        let v: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap_or_default();
        assert_eq!(v["id"], 9);
        assert_eq!(v["metadata"]["cited_chunk_ids"][0], "c1");
    }
}
