import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 「ファイルを読む」（storage.read）が文章として読めないファイルを断る e2e（BUG-101）:
/// PDF を読もうとすると、ブロックは「文章として読めない」誤り（not_text_file・やり直さない）で失敗し、実行履歴に
/// 読み方の案内と、原因を直してから再開する案内が出る。テキストのファイルはそのまま読める。以前は PDF や Word を文字化けしたまま成功させていた。
/// 「読むファイル」の注記はテキストのファイルだけを読むことを言う。拡張子が文章の種類なら、ブラウザーが付けた MIME
/// （Windows の .csv の application/vnd.ms-excel）で断らない。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type RunDetail = {
  status: string;
  steps: { node_id: string; status: string; error?: { code?: string; message?: string; retryable?: boolean } | null }[];
  failure?: { category: string; message: string } | null;
  resume?: { advice: string; offer: boolean; note: string } | null;
};

const NOT_TEXT =
  "このファイルは文章として読めません。PDF や Word の本文は「AI エージェント」の「文書の本文を読む」ツールで読みます。";

test("ファイルを読む: PDF は文章として読めない誤りで失敗し、テキストは読める（BUG-101）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  let id = "";
  const fileIds: string[] = [];
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: uniqueName("e2e-読むファイル"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;

    // ドライブのフォルダに PDF とテキストを置く。
    await page.goto(`/drive?folder=${folderId}`);
    const pdfName = `${uniqueName("契約書")}.pdf`;
    const txtName = `${uniqueName("メモ")}.txt`;
    const input = page.locator('input[type="file"][multiple]');
    await input.setInputFiles({
      name: pdfName,
      mimeType: "application/pdf",
      buffer: Buffer.from("%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<< /Type /Catalog >>\nendobj\n", "latin1"),
    });
    await expect(page.getByText(pdfName, { exact: true })).toBeVisible({ timeout: 20_000 });
    await input.setInputFiles({ name: txtName, mimeType: "text/plain", buffer: Buffer.from("経費の申請\n") });
    await expect(page.getByText(txtName, { exact: true })).toBeVisible({ timeout: 20_000 });
    // Excel を入れた Windows のブラウザーは .csv に application/vnd.ms-excel を付ける（そのまま保存される）。
    const csvName = `${uniqueName("売上")}.csv`;
    await input.setInputFiles({
      name: csvName,
      mimeType: "application/vnd.ms-excel",
      buffer: Buffer.from("日付,金額\n2026-10-29,1200\n"),
    });
    await expect(page.getByText(csvName, { exact: true })).toBeVisible({ timeout: 20_000 });
    // アップロードの確定を待ってから ID を引く（一覧に出るのは確定した後）。
    const ids = async (): Promise<string[]> => {
      const res = await page.request.get(`/api/nodes?parent_id=${folderId}`);
      const children = (await res.json()) as { items?: { id: string; name: string }[] };
      return [pdfName, txtName, csvName].map((n) => children.items?.find((c) => c.name === n)?.id ?? "");
    };
    await expect.poll(async () => (await ids()).every(Boolean), { timeout: 30_000 }).toBe(true);
    const [pdfId, txtId, csvId] = await ids();
    fileIds.push(pdfId, txtId, csvId);

    const name = uniqueName("read-text");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          input_schema: { type: "object", properties: { file: { type: "string", title: "ファイル" } } },
          nodes: [
            {
              id: "rd",
              type: "storage.read",
              label: "書類を読む",
              params: { file: { $from: "input", path: "/file" } },
            },
          ],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    id = ((await created.json()) as { id: string }).id;

    // 「読むファイル」の注記は、入力から選ぶときも、テキストのファイルだけを読むことを言う。
    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-rd").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(
      panel.getByText(
        "UTF-8 か Shift_JIS のテキストのファイル（.txt、.md、.csv、.json など）を読みます。PDF や Word の本文は読めません。",
        { exact: true },
      ),
    ).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/storage-read-hint-pc.png` });

    const run = async (file: string): Promise<{ runId: string; detail: RunDetail }> => {
      const started = await page.request.post(`/api/workflows/${id}/runs`, {
        headers: await csrfHeaders(page),
        data: { input: { file } },
      });
      expect(started.ok(), await started.text()).toBeTruthy();
      const runId = ((await started.json()) as { run_id: string }).run_id;
      const get = async () =>
        (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as RunDetail;
      await expect
        .poll(async () => (await get()).status, { timeout: 60_000, intervals: [1_000] })
        .toMatch(/^(succeeded|failed)$/);
      return { runId, detail: await get() };
    };

    // テキストは読める。
    const ok = await run(txtId);
    expect(ok.detail.status).toBe("succeeded");
    // 拡張子が文章の種類（.csv）なら、ブラウザーが付けた ms-excel の MIME で断らずに読む。
    const csv = await run(csvId);
    expect(csv.detail.status, JSON.stringify(csv.detail.steps)).toBe("succeeded");
    const csvStep = (await (
      await page.request.get(`/api/workflows/${id}/runs/${csv.runId}/steps?path=rd`)
    ).json()) as { output?: { text?: string } };
    expect(csvStep.output?.text).toBe("日付,金額\n2026-10-29,1200\n");

    // PDF は文章として読めない誤り（やり直さない）で失敗する。
    const bad = await run(pdfId);
    expect(bad.detail.status).toBe("failed");
    const step = bad.detail.steps.find((s) => s.node_id === "rd");
    expect(step?.error?.code).toBe("not_text_file");
    expect(step?.error?.retryable).toBe(false);
    expect(step?.error?.message).toBe(NOT_TEXT);
    // 再開は同じファイルの最新のバージョンを読み直すので、ファイルを保存し直してから再開すれば通る。原因を直して
    // から再開するよう勧める（最初から実行し直すと、済んだ書き込みや送信が二重になる・失敗の分類の単一定義から）。
    expect(bad.detail.failure?.category).toBe("configuration");
    expect(bad.detail.resume).toMatchObject({ advice: "fix_first", offer: true });

    await page.goto(`/workflows/${id}/runs?run=${bad.runId}`);
    const sheet = page.getByRole("dialog");
    await expect(sheet.getByText(NOT_TEXT).first()).toBeVisible({ timeout: 30_000 });
    await expect(sheet.getByTestId("run-failure")).toHaveText(bad.detail.failure!.message);
    await expect(sheet.getByRole("button", { name: "失敗したところから再開" })).toBeVisible();
    await expect(sheet.getByTestId("run-resume-note")).toHaveText(bad.detail.resume!.note);
    await expect(sheet.getByTestId("run-resume-note")).toContainText("原因を直してから再開すると");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/storage-read-not-text-pc.png` });

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
      await phone.goto(`/workflows/${id}/runs?run=${bad.runId}`);
      await expect(phone.getByRole("dialog").getByText(NOT_TEXT).first()).toBeVisible({ timeout: 30_000 });
      await expect(phone.getByRole("dialog").getByTestId("run-resume-note")).toBeVisible();
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/storage-read-not-text-phone.png` });
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
      ...fileIds.map((fileId) => async () => {
        const del = await page.request.delete(`/api/files/${fileId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      }),
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});
