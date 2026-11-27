import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 変更を待つ（control.wait・kind: event）の期限切れで「実行を失敗にする」ときの理由の e2e（BUG-112）:
/// 期限が来ると、ブロックの誤り（wait_timeout）と実行の失敗の理由（wait_timeout）が残り、実行履歴に
/// 「待つ期限までに変更が起きなかった」と出る。以前はどちらも空で、理由の分からない失敗になっていた。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type RunDetail = {
  status: string;
  fail_reason: string | null;
  steps: { node_id: string; status: string; error?: { code?: string; message?: string } | null }[];
};

const RUN_REASON = "待つ期限までに変更が起きなかったため、実行を止めました。";
const STEP_MESSAGE = "待つ期限までに変更が起きなかったため、止めました";

test("変更を待つ: 期限切れで失敗にすると、実行履歴に時間切れの理由が出る（BUG-112）", async ({ browser, page }) => {
  test.setTimeout(150_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  let id = "";
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: uniqueName("e2e-待つフォルダ"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;

    const name = uniqueName("wait-timeout");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "wait_file",
              type: "control.wait",
              label: "ファイルを待つ",
              params: {
                kind: "event",
                source: "storage.write",
                scope: { folder: folderId },
                timeout_sec: 1,
                on_timeout: "fail",
              },
            },
          ],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    id = ((await created.json()) as { id: string }).id;

    const started = await page.request.post(`/api/workflows/${id}/runs`, {
      headers: await csrfHeaders(page),
      data: { input: {} },
    });
    expect(started.ok(), await started.text()).toBeTruthy();
    const runId = ((await started.json()) as { run_id: string }).run_id;

    // 期限（1 秒）の後、スケジューラの見回りで失敗になる。
    const detail = async () =>
      (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as RunDetail;
    await expect
      .poll(async () => (await detail()).status, { timeout: 90_000, intervals: [1_000] })
      .toBe("failed");
    const failed = await detail();
    expect(failed.fail_reason).toBe("wait_timeout");
    const step = failed.steps.find((s) => s.node_id === "wait_file");
    expect(step?.status).toBe("failed");
    expect(step?.error?.code).toBe("wait_timeout");
    expect(step?.error?.message).toBe(STEP_MESSAGE);

    // 実行履歴の詳細に、実行の失敗の理由とブロックの誤りが出る。
    await page.goto(`/workflows/${id}/runs?run=${runId}`);
    const sheet = page.getByRole("dialog");
    await expect(sheet.getByText(RUN_REASON)).toBeVisible({ timeout: 30_000 });
    await expect(sheet.getByText(STEP_MESSAGE).first()).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/wait-timeout-pc.png` });

    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 3,
      ignoreHTTPSErrors: true,
    });
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/workflows/${id}/runs?run=${runId}`);
      const phoneSheet = phone.getByRole("dialog");
      await expect(phoneSheet.getByText(RUN_REASON)).toBeVisible({ timeout: 30_000 });
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/wait-timeout-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      async () => {
        if (!id) return;
        const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});
