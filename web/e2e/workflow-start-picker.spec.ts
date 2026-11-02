import { expect, test, type Page, type Route } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 「別のフローを開始」（workflow.start）の相手を選ぶ欄の e2e（BUG-107・D-91）:
/// - 相手はワークフローの一覧から画面に出る名前で選び、保存するのは今と同じ内部の名前。
/// - 無い名前を入れると、保存の前の検証が欄の下に「見つかりません。削除されたか、見る権限がありません。」と出す
///   （無いと見えないを言い分けない）。
/// - 相手の最新の保存内容が、このワークフローの権限で動くことを注記で言う。
/// - 一覧の 1 ページより後ろのワークフローも「さらに読み込む」で選択肢に足せる（一覧の応答は route で差し替える・
///   BUG-186 と同じ種類の欠け）。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

const NOT_FOUND = "実行するワークフローが見つかりません。削除されたか、見る権限がありません。";
const PERMISSION_NOTE =
  "相手のワークフローを編集できる人が保存した最新の内容が、そのまま動きます。";

test("別のフローを開始: 相手を一覧から表示名で選び、無い名前は保存の前に止める（BUG-107）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 保存していない変更を残して画面を移るときの確認（beforeunload）は、移る側を選ぶ。
  page.on("dialog", (d) => void d.accept());
  const ids: string[] = [];
  try {
    const create = async (ir: Record<string, unknown>): Promise<string> => {
      const res = await page.request.post("/api/workflows", { headers: await csrfHeaders(page), data: { ir } });
      expect(res.ok(), await res.text()).toBeTruthy();
      const id = ((await res.json()) as { id: string }).id;
      ids.push(id);
      return id;
    };
    const childName = uniqueName("child");
    const childLabel = `経費の集計 ${childName.slice(-6)}`;
    await create({
      ir_version: 1,
      name: childName,
      display_name: childLabel,
      triggers: [{ kind: "interactive" }],
      nodes: [{ id: "ls", type: "storage.list", params: {} }],
      edges: [],
    });
    const parentName = uniqueName("parent");
    const parent = await create({
      ir_version: 1,
      name: parentName,
      display_name: parentName,
      triggers: [{ kind: "interactive" }],
      nodes: [{ id: "start", type: "workflow.start", label: "集計を実行", params: { name: "" } }],
      edges: [],
    });

    await page.goto(`/workflows/${parent}`);
    await page.getByTestId("rf__node-start").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const select = panel.getByTestId("workflow-start-select");
    await expect(select).toHaveText("ワークフローを選んでください", { timeout: 15_000 });
    await expect(panel.getByText(PERMISSION_NOTE, { exact: false })).toBeVisible();
    await select.click();
    const picked = page.waitForResponse(
      (r) => r.url().endsWith("/api/workflows/validate") && r.request().method() === "POST",
    );
    await page.getByRole("option", { name: childLabel, exact: true }).click();
    await expect(select).toHaveText(childLabel);
    // 選んだ相手は保存者に見えるので、検証は誤りを返さず、欄の下は注記に戻る。
    expect(JSON.stringify(await (await picked).json())).not.toContain("ir.unknown_workflow");
    await expect(panel.getByText(/同じ組織のワークフローから選びます。/)).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/workflow-start-picked-pc.png` });

    // 保存すると、IR には相手の内部の名前が入る。
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${parent}`)).json()) as {
      ir: { nodes: { params: { name?: string } }[] };
    };
    expect(saved.ir.nodes[0].params.name).toBe(childName);

    // 無い名前は、保存の前の検証が欄の下に、存在を明かさない文で出す。
    await panel.getByRole("button", { name: "入力や結果から選ぶ" }).click();
    const validated = page.waitForResponse(
      (r) => r.url().endsWith("/api/workflows/validate") && r.request().method() === "POST",
    );
    await panel.getByRole("textbox", { name: "実行するワークフロー" }).fill(`${childName}-missing`);
    expect(JSON.stringify(await (await validated).json())).toContain("ir.unknown_workflow");
    await expect(panel.getByText(NOT_FOUND)).toBeVisible({ timeout: 10_000 });
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/workflow-start-missing-pc.png` });

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
      await phone.goto(`/workflows/${parent}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /集計を実行/ }).filter({ hasText: "集計を実行" }).first().click();
      const sheet = phone.getByRole("dialog");
      await expect(sheet.getByTestId("workflow-start-select")).toHaveText(childLabel, { timeout: 15_000 });
      await sheet.getByTestId("workflow-start-notes").scrollIntoViewIfNeeded();
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/workflow-start-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      ...ids.reverse().map((id) => async () => {
        const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      }),
    );
  }
});

function summary(n: number, label: string, day: number) {
  return {
    id: `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`,
    name: `picker-flow-${n}`,
    display_name: label,
    description: null,
    current_version: 1,
    trigger_kinds: ["interactive"],
    enabled_status: "none",
    enabled_version: null,
    updated_at: `2026-10-${String(day).padStart(2, "0")}T00:00:00Z`,
  };
}

test("別のフローを開始: 一覧の 1 ページより後ろのワークフローも「さらに読み込む」で選べる（BUG-186）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  page.on("dialog", (d) => void d.accept());
  let parent = "";
  const newest = summary(1, "最近のワークフロー", 20);
  const older = summary(2, "101 件目より古いワークフロー", 10);
  const LIST = /\/api\/workflows(\?.*)?$/;
  const asked: string[] = [];
  const handler = async (route: Route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const before = new URL(route.request().url()).searchParams.get("before_id");
    asked.push(before ?? "");
    const body = before
      ? { items: [older], owned_next: null, shared_next_cursor: null }
      : {
          items: [newest],
          owned_next: { before_updated_at: newest.updated_at, before_id: newest.id },
          shared_next_cursor: null,
        };
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
  };
  try {
    const name = uniqueName("picker-parent");
    const res = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [{ id: "start", type: "workflow.start", label: "集計を実行", params: { name: "" } }],
          edges: [],
        },
      },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    parent = ((await res.json()) as { id: string }).id;
    await page.route(LIST, handler);

    await page.goto(`/workflows/${parent}`);
    await page.getByTestId("rf__node-start").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const more = panel.getByTestId("list-more");
    await expect(more).toHaveText(/ほかにもワークフローがあります。「さらに読み込む」で選択肢に足します。/, {
      timeout: 15_000,
    });
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/workflow-start-more-pc.png` });
    await more.getByTestId("list-more-button").click();
    await expect(panel.getByTestId("list-more")).toHaveCount(0);
    expect(asked).toContain(newest.id);
    await panel.getByTestId("workflow-start-select").click();
    await expect(page.getByRole("option", { name: older.display_name, exact: true })).toBeVisible();
    await expect(page.getByRole("option", { name: newest.display_name, exact: true })).toBeVisible();
    await page.keyboard.press("Escape");
  } finally {
    await page.unroute(LIST, handler);
    await cleanupAll(async () => {
      if (!parent) return;
      const del = await page.request.delete(`/api/artifacts/${parent}`, { headers: await csrfHeaders(page) });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});
