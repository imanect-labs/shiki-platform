import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// ドライブの「新規作成 > CSV」を同じフォルダーで 2 回押すと、2 つ目はノートやスライドと同じ規則で
/// 「無題のスプレッドシート (2).csv」になる（番号はサーバーが付ける・D-112。以前は画面が「… 2.csv」と付けていた）。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

/// フォルダーの中のファイルの名前（名前順・1 ページ 100 件まで）。
async function namesIn(page: Page, folderId: string): Promise<{ id: string; name: string }[]> {
  const res = await page.request.get(`/api/nodes?parent_id=${folderId}&limit=100`);
  expect(res.ok(), await res.text()).toBeTruthy();
  const items = ((await res.json()) as { items?: { id: string; name: string }[] }).items ?? [];
  return items.sort((a, b) => a.name.localeCompare(b.name));
}

test("ドライブの新規作成の CSV は、同じ名前があると (2) を付けて作る（D-112）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  const fileIds: string[] = [];
  try {
    const folder = await page.request.post("/api/folders", {
      headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
      data: { name: uniqueName("e2e-新しい CSV"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;

    for (let i = 0; i < 2; i++) {
      await page.goto(`/drive?folder=${folderId}`);
      await page.getByRole("button", { name: "新規作成" }).click();
      await page.getByTestId("new-csv").click();
      await page.waitForURL(/\/csv\/[0-9a-f-]{36}/i, { timeout: 25_000 });
      fileIds.push(page.url().match(/\/csv\/([0-9a-f-]{36})/i)?.[1] ?? "");
      await expect(page.getByTestId("csv-grid")).toBeVisible({ timeout: 20_000 });
    }
    const names = (await namesIn(page, folderId)).map((n) => n.name);
    expect(names).toEqual(["無題のスプレッドシート (2).csv", "無題のスプレッドシート.csv"]);

    // ドライブの一覧に 2 つが並ぶ（PC とスマホ）。
    await page.goto(`/drive?folder=${folderId}`);
    await expect(page.getByText("無題のスプレッドシート (2).csv", { exact: true })).toBeVisible({ timeout: 20_000 });
    await expect(page.getByText("無題のスプレッドシート.csv", { exact: true })).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-new-csv-name-pc.png` });
    await page.setViewportSize({ width: 390, height: 844 });
    await page.reload();
    await expect(page.getByText("無題のスプレッドシート (2).csv", { exact: true })).toBeVisible({ timeout: 20_000 });
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-new-csv-name-mobile.png` });
  } finally {
    await cleanupAll(
      ...fileIds.map((id) => async () => {
        if (!id) return;
        const del = await page.request.delete(`/api/files/${id}`, { headers: { "x-csrf-token": await csrf(page) } });
        expect(del.status(), await del.text()).toBeLessThan(300);
      }),
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, {
          headers: { "x-csrf-token": await csrf(page) },
        });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});

test("ドライブの新規作成の CSV は、(50) まで埋まっていると作れない理由を出す（D-112）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  try {
    const folder = await page.request.post("/api/folders", {
      headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
      data: { name: uniqueName("e2e-CSV の上限"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;
    // 画面と同じ要求で (50) まで埋める。
    for (let i = 0; i < 50; i++) {
      const res = await page.request.post("/api/tabular/save", {
        headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
        data: { parent_id: folderId, name: "無題のスプレッドシート", csv: "列1\n", on_name_taken: "add_number" },
      });
      expect(res.ok(), await res.text()).toBeTruthy();
    }
    expect((await namesIn(page, folderId)).map((n) => n.name)).toContain("無題のスプレッドシート (50).csv");

    await page.goto(`/drive?folder=${folderId}`);
    await page.getByRole("button", { name: "新規作成" }).click();
    await page.getByTestId("new-csv").click();
    // 通知の本文（読み上げ用の領域にも同じ文が入るので、本文の全体で照らす）。
    await expect(page.getByText("CSV の作成に失敗しました", { exact: true })).toBeVisible({ timeout: 20_000 });
    await expect(
      page.getByText(
        "このフォルダーに「無題のスプレッドシート」の CSV が多すぎるため、作成できませんでした。いくつか名前を変えるか削除してから、もう一度作成してください。",
        { exact: true },
      ),
    ).toBeVisible();
    await expect(page).toHaveURL(/\/drive/);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-new-csv-full-pc.png` });
    expect(await namesIn(page, folderId)).toHaveLength(50);
  } finally {
    await cleanupAll(async () => {
      if (!folderId) return;
      for (const n of await namesIn(page, folderId)) {
        const del = await page.request.delete(`/api/files/${n.id}`, { headers: { "x-csrf-token": await csrf(page) } });
        expect(del.status(), await del.text()).toBeLessThan(300);
      }
      const del = await page.request.delete(`/api/folders/${folderId}`, {
        headers: { "x-csrf-token": await csrf(page) },
      });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});
