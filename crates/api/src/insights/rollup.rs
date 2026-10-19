//! 利用者×日のロールアップ（`ai_activity_daily`・#751）。
//!
//! 1 日分（日本時間）を**消して作り直す**（同じ日を何度流しても同じ結果・タイマー/jobq の
//! at-least-once に耐える・PIT-31）。材料はすべて既存の記録:
//! - `llm_usage`: 呼び出し・利用元・トークン・金額（`user_sub`）
//! - `generation_run`: 発話（run）・スレッド・エージェントモード・失敗・中断（`actor`）
//! - 回答の content ブロック: ツール呼び出し（文書検索・Web 検索・コード実行）と成果物カード
//! - `audit_log`: スキルの適用（`skill.apply`）・ミニアプリの呼び出し（`gateway.call`）
//! - `workflow_run`: ワークフローの実行と成功（委譲者がいればその人に寄せる）
//!
//! 数えるのはディレクトリに在籍する人だけ（分類器 [`super::CLASSIFIER_PRINCIPAL`] や、予約・イベントで
//! 動いたワークフローの主体は除く）。

use agent_core::ToolName;
use chrono::{Duration, NaiveDate};
use sqlx::PgPool;

/// 定常時に毎回作り直す過去の日数。あとから終わる実行（承認待ちのワークフロー・長いエージェント）の
/// 結果を、作成日の行へ取り込むため。
const RECOMPUTE_DAYS: i64 = 7;

/// 1 回の実行で 1 組織あたり埋める過去日数の上限。初回の遡り（[`BACKFILL_DAYS`]）は 1 回で
/// 埋め切る（導入直後に直近の数字が欠けないように）。長く止まっていたあとの追いつきだけ分割する。
const MAX_DAYS_PER_RUN: i64 = 120;
/// 初回に遡る日数（画面の最長期間に合わせる）。
const BACKFILL_DAYS: i64 = 120;

/// 1 日分を作り直す SQL（$1 tenant, $2 org, $3 day, $4〜$6 文書検索・Web・コード実行のツール名）。
const ROLLUP_SQL: &str = r"
WITH b AS (
    SELECT ($3::date)::timestamp AT TIME ZONE 'Asia/Tokyo' AS lo,
           ($3::date + 1)::timestamp AT TIME ZONE 'Asia/Tokyo' AS hi
),
u AS (
    SELECT l.user_sub AS user_id,
           count(*)::int AS calls,
           (count(*) FILTER (WHERE l.source = 'chat'))::int AS chat_calls,
           (count(*) FILTER (WHERE l.source = 'agent'))::int AS agent_calls,
           (count(*) FILTER (WHERE l.source = 'workflow'))::int AS workflow_calls,
           (count(*) FILTER (WHERE l.source = 'miniapp'))::int AS miniapp_calls,
           COALESCE(sum(l.prompt_tokens + l.completion_tokens), 0)::bigint AS tokens,
           COALESCE(sum(l.cost_usd_micros), 0)::bigint AS cost
    FROM llm_usage l, b
    WHERE l.tenant_id = $1 AND l.org = $2 AND l.created_at >= b.lo AND l.created_at < b.hi
      AND l.user_sub IS NOT NULL
    GROUP BY l.user_sub
),
r AS (
    SELECT g.actor AS user_id,
           count(*)::int AS runs,
           count(DISTINCT g.thread_id)::int AS threads,
           (count(*) FILTER (WHERE g.agent_mode))::int AS agent_runs,
           (count(*) FILTER (WHERE g.status = 'failed'))::int AS failed_runs,
           (count(*) FILTER (WHERE g.status = 'cancelled'))::int AS cancelled_runs
    FROM generation_run g, b
    WHERE g.tenant_id = $1 AND g.org = $2 AND g.created_at >= b.lo AND g.created_at < b.hi
    GROUP BY g.actor
),
k AS (
    SELECT g.actor AS user_id,
           (count(*) FILTER (WHERE x.blk->>'type' = 'tool_call'
                 AND x.blk->>'name' = ANY($4::text[])))::int AS doc_searches,
           (count(*) FILTER (WHERE x.blk->>'type' = 'tool_call'
                 AND x.blk->>'name' = ANY($5::text[])))::int AS web_searches,
           (count(*) FILTER (WHERE x.blk->>'type' = 'tool_call'
                 AND x.blk->>'name' = ANY($6::text[])))::int AS code_runs,
           (count(*) FILTER (WHERE x.blk->>'type' IN ('document_ref', 'note_ref')))::int AS documents
    FROM generation_run g
    JOIN message m ON m.id = g.message_id
    CROSS JOIN LATERAL jsonb_array_elements(
        CASE WHEN jsonb_typeof(m.content) = 'array' THEN m.content ELSE '[]'::jsonb END
    ) AS x(blk)
    CROSS JOIN b
    WHERE g.tenant_id = $1 AND g.org = $2 AND g.created_at >= b.lo AND g.created_at < b.hi
    GROUP BY g.actor
),
a AS (
    SELECT al.actor AS user_id,
           (count(*) FILTER (WHERE al.action = 'skill.apply'))::int AS skill_uses,
           (count(*) FILTER (WHERE al.action = 'gateway.call'))::int AS miniapp_uses
    FROM audit_log al, b
    WHERE al.tenant_id = $1 AND al.org = $2 AND al.created_at >= b.lo AND al.created_at < b.hi
      AND al.action IN ('skill.apply', 'gateway.call') AND al.decision = 'allow'
    GROUP BY al.actor
),
w AS (
    SELECT COALESCE(wr.delegator, wr.principal) AS user_id,
           count(*)::int AS workflow_runs,
           (count(*) FILTER (WHERE wr.status = 'succeeded'))::int AS workflow_ok
    FROM workflow_run wr, b
    WHERE wr.tenant_id = $1 AND wr.org = $2 AND wr.created_at >= b.lo AND wr.created_at < b.hi
    GROUP BY 1
),
ids AS (
    SELECT user_id FROM u UNION SELECT user_id FROM r UNION SELECT user_id FROM k
    UNION SELECT user_id FROM a UNION SELECT user_id FROM w
)
INSERT INTO ai_activity_daily (
    tenant_id, org, day, user_id,
    calls, chat_calls, agent_calls, workflow_calls, miniapp_calls, tokens, cost_usd_micros,
    threads, user_messages, runs, agent_runs, failed_runs, cancelled_runs,
    doc_searches, web_searches, code_runs, documents, skill_uses, miniapp_uses,
    workflow_runs, workflow_ok
)
SELECT $1, $2, $3::date, ids.user_id,
       COALESCE(u.calls, 0), COALESCE(u.chat_calls, 0), COALESCE(u.agent_calls, 0),
       COALESCE(u.workflow_calls, 0), COALESCE(u.miniapp_calls, 0),
       COALESCE(u.tokens, 0), COALESCE(u.cost, 0),
       COALESCE(r.threads, 0), COALESCE(r.runs, 0), COALESCE(r.runs, 0),
       COALESCE(r.agent_runs, 0), COALESCE(r.failed_runs, 0), COALESCE(r.cancelled_runs, 0),
       COALESCE(k.doc_searches, 0), COALESCE(k.web_searches, 0), COALESCE(k.code_runs, 0),
       COALESCE(k.documents, 0), COALESCE(a.skill_uses, 0), COALESCE(a.miniapp_uses, 0),
       COALESCE(w.workflow_runs, 0), COALESCE(w.workflow_ok, 0)
FROM ids
LEFT JOIN u ON u.user_id = ids.user_id
LEFT JOIN r ON r.user_id = ids.user_id
LEFT JOIN k ON k.user_id = ids.user_id
LEFT JOIN a ON a.user_id = ids.user_id
LEFT JOIN w ON w.user_id = ids.user_id
WHERE ids.user_id IS NOT NULL
  -- 人だけを数える（ディレクトリの在籍者）。予約・イベント起動のワークフロー（主体がワークフロー
  -- ID）や分類器（system:）などは利用者に含めない。
  AND EXISTS (SELECT 1 FROM directory_user du
              WHERE du.tenant_id = $1 AND du.org = $2 AND du.user_id = ids.user_id)
";

/// ツールの区分（名前は agent-core の語彙から取る・手書きしない）。
fn tool_names(names: &[ToolName]) -> Vec<&'static str> {
    names.iter().map(|n| n.as_str()).collect()
}

/// 1 組織の 1 日分を作り直す（消して入れる・1 トランザクション）。
pub async fn rollup_day(
    db: &PgPool,
    tenant_id: &str,
    org: &str,
    day: NaiveDate,
) -> Result<u64, sqlx::Error> {
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM ai_activity_daily WHERE tenant_id = $1 AND org = $2 AND day = $3")
        .bind(tenant_id)
        .bind(org)
        .bind(day)
        .execute(&mut *tx)
        .await?;
    let n = sqlx::query(ROLLUP_SQL)
        .bind(tenant_id)
        .bind(org)
        .bind(day)
        .bind(tool_names(&[ToolName::DocSearch, ToolName::DocRead]))
        .bind(tool_names(&[ToolName::WebSearch, ToolName::WebFetch]))
        .bind(tool_names(&[
            ToolName::CodeInterpreter,
            ToolName::Shell,
            ToolName::CsvQuery,
        ]))
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(n)
}

/// 集計対象の組織（ディレクトリに利用者がいる組織）。
pub async fn orgs(db: &PgPool) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as("SELECT DISTINCT tenant_id, org FROM directory_user ORDER BY 1, 2")
        .fetch_all(db)
        .await
}

/// この実行の計画: 作り直す日と、終わったら記録する到達点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub days: Vec<NaiveDate>,
    /// 連続して作れた最後の過去日（当日は毎回作り直すので含めない）。
    pub reached: NaiveDate,
}

/// この実行で作り直す日（直近 [`RECOMPUTE_DAYS`] 日と当日は毎回。未集計の過去は古い順に上限まで）。
pub fn plan(today: NaiveDate, computed_through: Option<NaiveDate>) -> Plan {
    let yesterday = today - Duration::days(1);
    let start = match computed_through {
        // 前回の到達点の数日前から（あとから終わった実行の結果も拾う）。
        Some(d) => (d - Duration::days(RECOMPUTE_DAYS)).min(yesterday),
        None => today - Duration::days(BACKFILL_DAYS),
    };
    let mut days: Vec<NaiveDate> = Vec::new();
    let mut d = start;
    while d < yesterday && i64::try_from(days.len()).unwrap_or(i64::MAX) < MAX_DAYS_PER_RUN {
        days.push(d);
        d += Duration::days(1);
    }
    // 追いつきが上限で切れたら、到達点はそこまで（次回は続きから）。前日まで届いたら前日。
    let reached = if d < yesterday {
        days.last().copied().unwrap_or(start)
    } else {
        yesterday
    };
    // 前日・当日は必ず作る（画面の鮮度を保つ）。
    days.push(yesterday);
    days.push(today);
    days.dedup();
    Plan { days, reached }
}

/// 1 組織分を進める（作り直した日数を返す）。
pub async fn rollup_org(
    db: &PgPool,
    tenant_id: &str,
    org: &str,
    today: NaiveDate,
) -> Result<usize, sqlx::Error> {
    let through: Option<NaiveDate> = sqlx::query_scalar(
        "SELECT computed_through FROM ai_insight_rollup_state WHERE tenant_id = $1 AND org = $2",
    )
    .bind(tenant_id)
    .bind(org)
    .fetch_optional(db)
    .await?;
    let plan = plan(today, through);
    for d in &plan.days {
        rollup_day(db, tenant_id, org, *d).await?;
    }
    sqlx::query(
        "INSERT INTO ai_insight_rollup_state (tenant_id, org, computed_through) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (tenant_id, org) DO UPDATE SET \
           computed_through = GREATEST(ai_insight_rollup_state.computed_through, EXCLUDED.computed_through), \
           updated_at = now()",
    )
    .bind(tenant_id)
    .bind(org)
    .bind(plan.reached)
    .execute(db)
    .await?;
    Ok(plan.days.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn steady_state_recomputes_the_last_week_and_today() {
        let today = d("2026-10-19");
        let p = plan(today, Some(d("2026-10-18")));
        assert_eq!(p.days.first(), Some(&d("2026-10-11")));
        assert_eq!(p.days.last(), Some(&today));
        assert_eq!(
            p.days.len() as i64,
            RECOMPUTE_DAYS + 2,
            "10/11〜10/19 を欠けなく"
        );
        assert_eq!(p.reached, d("2026-10-18"));
    }

    #[test]
    fn first_run_backfills_the_whole_window_at_once() {
        let today = d("2026-10-19");
        let p = plan(today, None);
        let start = today - Duration::days(BACKFILL_DAYS);
        assert_eq!(p.days.first(), Some(&start));
        assert_eq!(p.days.last(), Some(&today));
        assert_eq!(
            p.days.len() as i64,
            BACKFILL_DAYS + 1,
            "遡り期間＋当日を欠けなく"
        );
        assert_eq!(p.reached, d("2026-10-18"));
    }

    #[test]
    fn long_outage_catches_up_in_bounded_chunks_without_skipping_the_gap() {
        let today = d("2026-10-19");
        let old = today - Duration::days(400);
        let p = plan(today, Some(old));
        assert_eq!(
            p.days.first(),
            Some(&(old - Duration::days(RECOMPUTE_DAYS)))
        );
        assert!(p.days.contains(&today) && p.days.contains(&d("2026-10-18")));
        assert!(p.days.len() as i64 <= MAX_DAYS_PER_RUN + 2);
        // 到達点は追いつきの最終日（前日ではない）。次回はその続きから。
        assert_eq!(
            p.reached,
            old - Duration::days(RECOMPUTE_DAYS) + Duration::days(MAX_DAYS_PER_RUN - 1)
        );
        let next = plan(today, Some(p.reached));
        assert_eq!(
            next.days.first(),
            Some(&(p.reached - Duration::days(RECOMPUTE_DAYS)))
        );
        // 追いつき切ると到達点は前日。
        let mut through = Some(p.reached);
        for _ in 0..10 {
            through = Some(plan(today, through).reached);
        }
        assert_eq!(through, Some(d("2026-10-18")));
    }
}
