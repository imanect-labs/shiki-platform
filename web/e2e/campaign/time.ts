import type { Page } from "@playwright/test";

import { ok, type Json } from "./http";
import { runs, type RunDetail, type RunItem } from "./truth";
import { enableWithPlan, removeWorkflow, type BuiltWorkflow } from "./world";

/// 時間で動く APP の確かめ。
///
/// - `runAt` … 予定の日時を指定して手動で動かす（`POST /runs` の `scheduled_at`・GAP-88）。**押した人の権限で動く**
///   （`start_interactive_trial`）。スケジュールの委譲・grants（二段の authz）・trigger_kind=schedule は通らない
///   （計画のレビュー 12）。日付の決まりを決まった日で確かめるのに使う。
/// - `withMinuteCron` … IR を写して cron を毎分にし、有効化して、スケジュールの run を本物で 1 回以上動かす
///   （委譲の経路）。スケジュールで動く APP は、どれもこれで最低 1 回は動かす。写しは必ず止めて消す。

/// 日本時間の今日（YYYY-MM-DD）。
export function jstToday(): string {
  return new Date().toLocaleDateString("sv-SE", { timeZone: "Asia/Tokyo" });
}

/// 日付（YYYY-MM-DD）を n 日ずらす（暦の日付で数える）。
export function addDays(day: string, n: number): string {
  const d = new Date(`${day}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + n);
  return d.toISOString().slice(0, 10);
}

/// 予定の日時（`2026-11-09T09:00` は日本時間で読む・時差つきも可）を指定して手動で動かし、run の id を返す。
export async function runAt(page: Page, workflowId: string, scheduledAt: string, input: Json = {}): Promise<string> {
  const r = await ok<{ run_id: string | null }>(page, "POST", `/workflows/${workflowId}/runs`, { input, scheduled_at: scheduledAt }, [202]);
  if (!r.run_id) throw new Error(`予定の日時 ${scheduledAt} の実行が起動されなかった`);
  return r.run_id;
}

/// `runAt` して終わるまで待つ。
export async function runAtDone(page: Page, workflowId: string, scheduledAt: string, input: Json = {}): Promise<RunDetail> {
  return runs.waitEnd(page, workflowId, await runAt(page, workflowId, scheduledAt, input));
}

/// IR のきっかけを毎分のスケジュールにしたもの（ほかのきっかけは落とす）。
function minuteIr(ir: Json, name: string): Json {
  return {
    ...ir,
    name,
    display_name: `${String(ir.display_name ?? ir.name)}（毎分の確かめ）`,
    triggers: [{ kind: "schedule", cron: "* * * * *", tz: "Asia/Tokyo", catchup: "skip" }],
  };
}

export type CronRun = { workflowId: string; run: RunItem; detail: RunDetail };

/// `src` の IR を写して毎分のスケジュールで有効化し（委譲は `src.grants`・同意計画と照らす）、スケジュールの run が
/// `minRuns` 本（既定 1）終わるまで待つ（上限 150 秒・tick は 5 秒・workflow-date-calc.spec.ts と同じ）。止めてから
/// `judge` に最初の run を渡す。判定は run の本数でなく、動いた後のデータで行う（止める前に次の分が入りうる）。
/// 写しは成否によらず止めて消す。
export async function withMinuteCron<T>(
  page: Page,
  src: Pick<BuiltWorkflow, "ir" | "grants" | "name">,
  judge: (cron: CronRun) => Promise<T>,
  opts: { minRuns?: number; timeout?: number } = {},
): Promise<T> {
  // 写しの名前は `campaign-cron-` で始める（消し損ねたら次の組み立ての sweepCampaignSchedules が拾う）。
  const name = `${src.name.replace(/^campaign-/, "campaign-cron-").slice(0, 56)}-${Date.now().toString(36).slice(-6)}`;
  const made = await ok<{ id: string; version: number }>(page, "POST", "/workflows", { ir: minuteIr(src.ir, name) });
  try {
    await enableWithPlan(page, made.id, made.version ?? 1, src.grants);
    const want = opts.minRuns ?? 1;
    const items = await runs.waitFor(page, made.id, {
      trigger_kind: "schedule",
      count: want,
      timeout: opts.timeout ?? 150_000,
      message: "毎分のスケジュールで動く",
    });
    const first = items[items.length - 1];
    const detail = await runs.waitEnd(page, made.id, first.run_id, 120_000);
    if (want > 1) {
      for (const r of items.slice(0, items.length - 1)) await runs.waitEnd(page, made.id, r.run_id, 120_000);
    }
    // 次の分を待たない（判定の前に止める）。
    await ok(page, "POST", `/workflows/${made.id}/disable`, { revoke_delegations: true });
    return await judge({ workflowId: made.id, run: first, detail });
  } finally {
    await removeWorkflow(page, made.id);
  }
}
