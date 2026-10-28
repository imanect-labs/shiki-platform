import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 「ファイルを保存」で同じ名前のファイルがあるときの e2e（BUG-97・D-97・D-96）:
/// - 「同じ名前があるとき」が 番号を付けて保存 なら、2 回目は「名前 (2).md」で保存する。名前には実行情報の
///   「実行した日」（日本時間）を差し込める。ファイルの種類を選ばなければ拡張子から決める（text/markdown）。
/// - 欄が無い既存の定義は今のまま失敗にし、失敗の理由は「同じ名前のファイル」と言う（以前は「ほかの操作と
///   重なった…表を読み直して」と出ていた）。
/// - 結果にはファイルの ID が file_id でも載る（古い id も残る）。
/// - 新しく置いたブロックは 番号を付けて保存 から始まる。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type RunDetail = {
  status: string;
  steps: { node_id: string; status: string; error?: { code?: string; message?: string } | null }[];
};

/// 今日の日付（日本時間・実行情報の date と同じ形）。
function todayInJapan(): string {
  return new Date().toLocaleDateString("sv-SE", { timeZone: "Asia/Tokyo" });
}

test("ファイルを保存: 同じ名前があるときは番号を付け、欄の無い定義は今のまま失敗する（BUG-97）", async ({
  browser,
  page,
}) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 保存していない変更を残して画面を移るときの確認（beforeunload）は、移る側を選ぶ。
  page.on("dialog", (d) => void d.accept());
  let folderId = "";
  const workflowIds: string[] = [];
  const fileIds = new Set<string>();
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: uniqueName("e2e-同じ名前"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;

    const create = async (name: string, params: Record<string, unknown>): Promise<string> => {
      const res = await page.request.post("/api/workflows", {
        headers: await csrfHeaders(page),
        data: {
          ir: {
            ir_version: 1,
            name,
            display_name: name,
            triggers: [{ kind: "interactive" }],
            nodes: [{ id: "save", type: "storage.write", label: "日報を保存", params: { folder: folderId, ...params } }],
            edges: [],
          },
        },
      });
      expect(res.ok(), await res.text()).toBeTruthy();
      const id = ((await res.json()) as { id: string }).id;
      workflowIds.push(id);
      return id;
    };
    const numbered = await create(uniqueName("name-number"), {
      name: { $template: "日報-{d}.md", vars: { d: { $from: "run", path: "/date" } } },
      content: "# 日報",
      on_name_taken: "add_number",
    });
    const legacy = await create(uniqueName("name-legacy"), { name: "固定の名前.txt", content: "x" });

    const run = async (id: string): Promise<{ runId: string; detail: RunDetail }> => {
      const started = await page.request.post(`/api/workflows/${id}/runs`, {
        headers: await csrfHeaders(page),
        data: { input: {} },
      });
      expect(started.ok(), await started.text()).toBeTruthy();
      const runId = ((await started.json()) as { run_id: string }).run_id;
      const get = async () => (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as RunDetail;
      await expect
        .poll(async () => (await get()).status, { timeout: 60_000, intervals: [1_000] })
        .toMatch(/^(succeeded|failed)$/);
      return { runId, detail: await get() };
    };
    const output = async (id: string, runId: string) => {
      const res = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=save`);
      const out = ((await res.json()) as { output: { file_id: string; id: string; name: string } }).output;
      fileIds.add(out.id);
      return out;
    };

    // 番号を付けて保存: 1 回目は実行した日の名前、2 回目は番号を付けた名前。種類は拡張子から決まる。
    const day = todayInJapan();
    const first = await run(numbered);
    expect(first.detail.status).toBe("succeeded");
    const firstOut = await output(numbered, first.runId);
    expect(firstOut.name).toBe(`日報-${day}.md`);
    expect(firstOut.file_id).toBe(firstOut.id);
    const second = await run(numbered);
    expect(second.detail.status).toBe("succeeded");
    const secondOut = await output(numbered, second.runId);
    expect(secondOut.name).toBe(`日報-${day} (2).md`);
    expect(secondOut.file_id).not.toBe(firstOut.file_id);
    const meta = (await (await page.request.get(`/api/files/${secondOut.file_id}`)).json()) as { content_type?: string };
    expect(meta.content_type).toBe("text/markdown");

    // 欄の無い既存の定義は、2 回目で失敗する。理由は同じ名前のファイル。
    const legacyFirst = await run(legacy);
    expect(legacyFirst.detail.status).toBe("succeeded");
    await output(legacy, legacyFirst.runId);
    const legacySecond = await run(legacy);
    expect(legacySecond.detail.status).toBe("failed");
    const step = legacySecond.detail.steps.find((s) => s.node_id === "save");
    expect(step?.error?.code).toBe("name_taken");
    expect(step?.error?.message).toContain("同じ名前のファイル「固定の名前.txt」がすでにあります");
    await page.goto(`/workflows/${legacy}/runs?run=${legacySecond.runId}`);
    const sheet = page.getByRole("dialog");
    // 実行の詳細は作成者向けの原因（同じ名前のファイルと直し方）を出す。利用者向けの 1 行目（同じ名前のファイルが
    // すでにあるため、保存できませんでした。）は失敗の分類の単体テストで確かめる。
    await expect(sheet.getByText(/同じ名前のファイル「固定の名前\.txt」がすでにあります/).first()).toBeVisible({
      timeout: 30_000,
    });
    await expect(sheet.getByText(/ほかの操作と重なった/)).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/name-taken-run-pc.png` });

    // 設定の画面: 欄の無い定義は「保存しない」、番号を付ける定義は「番号を付けて保存」。
    await page.goto(`/workflows/${legacy}`);
    await page.getByTestId("rf__node-save").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByTestId("name-taken-select")).toHaveText("保存しない");
    await expect(panel.getByText(/保存せずに、このブロックを失敗にします。/)).toBeVisible();
    await page.goto(`/workflows/${numbered}`);
    await page.getByTestId("rf__node-save").click();
    await expect(panel.getByTestId("name-taken-select")).toHaveText("番号を付けて保存");
    await expect(panel.getByText(/番号を付けた空いている名前で保存します。番号は 50 まで付けます。/)).toBeVisible();
    await expect(panel.getByText(/選ばなければ、ファイル名の拡張子（\.md など）から決めます。/)).toBeVisible();
    await expect(panel.getByRole("combobox", { name: "ファイルの種類" })).toHaveText("拡張子から決める");
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/name-taken-panel-pc.png` });

    // 差し込みの候補に、実行情報の 実行した日 が出る（日本時間の見出しの下）。
    await panel.getByTestId("insert-var").first().click();
    await expect(page.getByText("実行情報（日本時間）")).toBeVisible();
    await expect(page.getByRole("menuitem", { name: /実行した日（date）/ })).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/name-taken-run-info-pc.png` });
    await page.keyboard.press("Escape");

    // 新しく置いたブロックは 番号を付けて保存 から始まる（新しいワークフローの最初のブロックとして置く）。
    await page.goto("/workflows");
    await expect(page.getByText("読み込み中…")).toHaveCount(0);
    await page.getByRole("button", { name: "新しいワークフロー", exact: true }).click();
    await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i, { timeout: 20_000 });
    workflowIds.push(page.url().split("/").pop() ?? "");
    await page.getByRole("button", { name: "最初のブロック" }).click();
    await page.getByRole("button", { name: /^CSV を保存/ }).click();
    const canvas = page.locator(".react-flow");
    await canvas.getByText("CSV を保存").first().click();
    await expect(panel.getByTestId("name-taken-select")).toHaveText("番号を付けて保存");

    // スマホの一覧から開いても同じ欄が出て、横にはみ出さない。
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
      await phone.goto(`/workflows/${numbered}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /日報を保存/ }).filter({ hasText: "日報を保存" }).first().click();
      const phoneSheet = phone.getByRole("dialog");
      await expect(phoneSheet.getByTestId("name-taken-select")).toHaveText("番号を付けて保存");
      await phoneSheet.getByTestId("name-taken-select").scrollIntoViewIfNeeded();
      expect(await phoneSheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/name-taken-panel-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      ...workflowIds.map((id) => async () => {
        const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      }),
      ...[...fileIds].map((fileId) => async () => {
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
