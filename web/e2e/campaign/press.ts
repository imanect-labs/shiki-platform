import { expect, type Page } from "@playwright/test";

import { call, idemKey, ok } from "./http";
import type { DataRecord } from "./truth";

/// ミニアプリのボタンを押す（画面から・API から）と、画面の表を読む。
///
/// - `pressInApp` … 本物の画面の道（ui-figure → 実行の確認 → yui-runs の data-status → yui-run-result）。送った
///   action_id・値・冪等キーと run_id を拾う（mini-apps.spec.ts の apply と同じ押し方）。
/// - `pressApi` … `POST /mini-apps/{id}/ui-actions` を、action_id・値・冪等キーで送る（同時押し・二度押しに使う）。
///   押した本人の権限で起動するのは画面と同じ（同じ入口・本人の記録に残る）。
/// - `gridRows` … 画面の表（```data_grid）を、閲覧者本人の権限で読む（`data_grid:<別名>`）。

/// 画面に返した結果の 1 節（`workflow.reply` の出力・GAP-09）。
export type ReplySection = {
  label?: string;
  tone: "neutral" | "success" | "warning" | "error";
  format: "text" | "markdown" | "table" | "file_link";
  text?: string;
  columns?: string[];
  rows?: string[][];
  files?: { id: string; name: string }[];
};

/// 本人が押した 1 件（`GET /mini-apps/{id}/runs/{run_id}`）。
export type AppRun = {
  action_id: string;
  app_version: number;
  label: string;
  workflow_name: string;
  run_id?: string | null;
  status: "starting" | "queued" | "running" | "waiting" | "succeeded" | "failed" | "cancelled" | "skipped";
  values: { label: string; value: string }[];
  waiting_for?: string;
  failure?: { code?: string; message: string; reference: string };
  reply?: { sections: ReplySection[]; omitted: number };
  has_reply: boolean;
};

const DONE = ["succeeded", "failed", "cancelled", "skipped"];

/// 結果の節の文をつないだもの（表・リンクは見出しだけ）。
export function replyText(run: AppRun): string {
  return (run.reply?.sections ?? []).map((s) => s.text ?? s.label ?? "").join("\n");
}

/// 結果の節の色合い（実行順）。
export function replyTones(run: AppRun): string[] {
  return (run.reply?.sections ?? []).map((s) => s.tone);
}

/// 本人が押した 1 件が終わるまで待って返す（他人の run・見えないアプリは 404 で落とす）。
export async function waitAppRun(page: Page, appId: string, runId: string, timeout = 90_000): Promise<AppRun> {
  let run: AppRun | undefined;
  await expect
    .poll(
      async () => {
        run = await ok<AppRun>(page, "GET", `/mini-apps/${appId}/runs/${runId}`);
        return DONE.includes(run.status);
      },
      { timeout, intervals: [500, 1_000, 2_000], message: `アプリの run ${runId} が終わる` },
    )
    .toBe(true);
  return run!;
}

export type PressResult = {
  runId: string;
  /// 送った action_id（`{ブロックの識別子}:a{n}`）。
  actionId: string;
  /// 送った部品の値（押した節の値だけが送られるか、の裏取りに使う）。
  params: Record<string, unknown>;
  idempotencyKey: string;
  version: number;
  /// 終わった記録（結果つき）。
  run: AppRun;
};

/// 画面から押す: `/apps/{id}` を開き（`open: false` なら今の画面のまま）、節（`## 見出し`）の中の部品に値を入れて
/// ボタンを押し、実行の確認で「実行する」を押す。画面の記録（yui-runs）が終わった状態になり、結果の欄が出るまで待つ。
export async function pressInApp(
  page: Page,
  appId: string,
  opts: { button: string; section?: string; fill?: Record<string, string>; open?: boolean; timeout?: number },
): Promise<PressResult> {
  if (opts.open ?? true) await page.goto(`/apps/${appId}`);
  const figure = page.getByTestId("ui-figure").first();
  const scope = opts.section ? figure.getByRole("region", { name: opts.section }) : figure;
  const button = scope.getByRole("button", { name: opts.button, exact: true });
  await expect(button).toBeEnabled({ timeout: 30_000 });
  for (const [label, value] of Object.entries(opts.fill ?? {})) {
    await scope.getByLabel(label).fill(value);
  }
  const sent = page.waitForRequest(
    (r) => r.method() === "POST" && r.url().includes(`/api/mini-apps/${appId}/ui-actions`) && !String(r.postData()).includes('"data_grid:'),
    { timeout: 30_000 },
  );
  const answered = page.waitForResponse(
    (r) =>
      r.request().method() === "POST" &&
      r.url().includes(`/api/mini-apps/${appId}/ui-actions`) &&
      !String(r.request().postData()).includes('"data_grid:'),
    { timeout: 30_000 },
  );
  await button.click();
  const confirm = scope.getByRole("group", { name: "実行の確認" });
  if (await confirm.isVisible({ timeout: 3_000 }).catch(() => false)) {
    await confirm.getByRole("button", { name: "実行する" }).click();
  }
  const req = (await sent).postDataJSON() as {
    action_id: string;
    params: Record<string, unknown>;
    idempotency_key: string;
    version: number;
  };
  const res = await answered;
  expect(res.ok(), `ui-actions → ${res.status()} ${await res.text()}`).toBe(true);
  const runId = ((await res.json()) as { result: { kind: string; run_id?: string } }).result.run_id;
  expect(runId, "ボタンがワークフローを起動した").toBeTruthy();
  const row = figure.getByTestId("yui-runs").locator(`li[data-run-id="${runId}"]`);
  await expect(row).toHaveAttribute("data-status", /^(succeeded|failed|cancelled|skipped)$/, { timeout: opts.timeout ?? 90_000 });
  const run = await waitAppRun(page, appId, runId!, opts.timeout);
  if (run.has_reply) await expect(row.getByTestId("yui-run-result")).toBeVisible({ timeout: 15_000 });
  return { runId: runId!, actionId: req.action_id, params: req.params, idempotencyKey: req.idempotency_key, version: req.version, run };
}

export type ApiPress = {
  status: number;
  /// 起動した run（起動しなかったら undefined）。
  runId?: string;
  body: unknown;
  key: string;
};

/// API から押す（画面と同じ入口・押した本人の権限）。`key` を省略すると新しい冪等キーにする。
export async function pressApi(
  page: Page,
  appId: string,
  opts: { version: number; actionId: string; params: Record<string, unknown>; key?: string },
): Promise<ApiPress> {
  const key = opts.key ?? idemKey("press");
  const r = await call<{ result?: { kind: string; run_id?: string } }>(page, "POST", `/mini-apps/${appId}/ui-actions`, {
    version: opts.version,
    action_id: opts.actionId,
    params: opts.params,
    idempotency_key: key,
  });
  return { status: r.status, runId: r.body?.result?.run_id, body: r.body, key };
}

/// API から押して、終わった記録を返す（起動できなければ本文つきで落とす）。
export async function pressApiDone(
  page: Page,
  appId: string,
  opts: { version: number; actionId: string; params: Record<string, unknown>; key?: string },
): Promise<AppRun & { run_id: string }> {
  const p = await pressApi(page, appId, opts);
  expect(p.status, JSON.stringify(p.body)).toBe(200);
  expect(p.runId, "ボタンがワークフローを起動した").toBeTruthy();
  return { ...(await waitAppRun(page, appId, p.runId!)), run_id: p.runId! };
}

export type GridPage = { status: number; rows: DataRecord[]; body: unknown };

/// 画面の表（```data_grid の別名）を読む（閲覧者本人の権限・行述語とフィールドマスクつき）。
export async function gridRows(
  page: Page,
  appId: string,
  version: number,
  alias: string,
  params: { limit?: number; cursor?: string } = {},
): Promise<GridPage> {
  const r = await call<{ result?: { kind: string; result?: { result?: { items?: DataRecord[] } } } }>(
    page,
    "POST",
    `/mini-apps/${appId}/ui-actions`,
    { version, action_id: `data_grid:${alias}`, params },
  );
  return { status: r.status, rows: r.body?.result?.result?.result?.items ?? [], body: r.body };
}
