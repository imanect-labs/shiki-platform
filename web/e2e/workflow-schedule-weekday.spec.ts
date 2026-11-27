import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// スケジュールの曜日（BUG-95・D-81）の e2e:
/// サーバーは曜日を標準の読み方（0 と 7 が日曜、1 が月曜）で読み、画面は曜日を MON〜SUN の名前で書く。
/// 「次の実行」はサーバーが発火と同じ関数で数えたもの（ブラウザーで cron を読まない）。
/// 保存済みの `0 9 * * 1-5`（AI の説明文の例・平日のつもり）は平日の 9 時として出る。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

/// 平日の 9 時に動く（つもりの）スケジュールのワークフローを API で作る。
async function createScheduledFlow(page: Page, cron: string): Promise<string> {
  const name = uniqueName("schedule-weekday");
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "schedule", cron, tz: "Asia/Tokyo", catchup: "skip" }],
        nodes: [
          {
            id: "compute",
            type: "script.run",
            label: "計算する",
            params: { source: { inline: "function main(i){ return { ok: true }; }" } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}

async function deleteFlow(page: Page, id: string) {
  if (!id) return;
  const res = await page.request.delete(`/api/artifacts/${id}`, {
    headers: { "x-csrf-token": await csrf(page) },
  });
  expect(res.status(), await res.text()).toBeLessThan(300);
}

/// 「次の実行」の行に出た曜日（「10/28（水）9:00」の括弧の中）。
async function previewWeekdays(page: Page): Promise<string[]> {
  const line = page.getByText(/^次の実行（日本時間）:/);
  await expect(line).toBeVisible({ timeout: 10_000 });
  const text = (await line.textContent()) ?? "";
  return [...text.matchAll(/（(.)）(\d+):(\d+)/g)].map((m) => `${m[1]} ${m[2]}:${m[3]}`);
}

test("スケジュール: 平日の式は月〜金に、毎週・月曜日は月曜に、日曜日は日曜に出て保存できる（BUG-95）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let id = "";
  try {
    id = await createScheduledFlow(page, "0 9 * * 1-5");
    await page.goto(`/workflows/${id}`);
    await page.locator(".react-flow").getByText("スケジュール").first().click();
    const panel = page.getByRole("complementary", { name: "きっかけの設定" });

    // 保存済みの 1-5 は「平日（月〜金）」として読み、次の実行も平日の 9 時になる（日曜〜木曜にならない）。
    await expect(panel.getByRole("combobox", { name: "繰り返し" })).toHaveText("平日（月〜金）");
    for (const d of await previewWeekdays(page)) {
      expect(["月 9:00", "火 9:00", "水 9:00", "木 9:00", "金 9:00"]).toContain(d);
    }
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/schedule-weekdays-pc.png` });

    // 毎週を選ぶと既定は月曜日で、式は名前（MON）で書く。次の実行は月曜。
    await panel.getByRole("combobox", { name: "繰り返し" }).click();
    await page.getByRole("option", { name: "毎週" }).click();
    await expect(panel.getByRole("combobox", { name: "曜日" })).toHaveText("月曜日");
    await expect(panel.getByRole("textbox", { name: /cron 式/ })).toHaveValue("0 9 * * MON");
    await expect.poll(() => previewWeekdays(page)).toEqual(["月 9:00", "月 9:00"]);

    // 日曜日も選べて（以前は 0 を保存できなかった）、次の実行は日曜。保存した式は SUN。
    await panel.getByRole("combobox", { name: "曜日" }).click();
    await page.getByRole("option", { name: "日曜日" }).click();
    await expect(panel.getByRole("textbox", { name: /cron 式/ })).toHaveValue("0 9 * * SUN");
    await expect.poll(() => previewWeekdays(page)).toEqual(["日 9:00", "日 9:00"]);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/schedule-sunday-pc.png` });
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
      ir: { triggers: { cron: string }[] };
    };
    expect(saved.ir.triggers[0].cron).toBe("0 9 * * SUN");

    // 写せない曜日（範囲の逆転）は、サーバーの文言で誤りを出す（保存時の誤りと同じ文）。
    await panel.getByRole("textbox", { name: /cron 式/ }).fill("0 9 * * 5-1");
    await expect(panel.getByRole("alert")).toHaveText(
      "スケジュールの曜日の書き方が正しくありません。曜日は 0〜7（0 と 7 が日曜日、1 が月曜日）か、SUN〜SAT の名前で書いてください。",
      { timeout: 10_000 },
    );
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/schedule-invalid-pc.png` });
  } finally {
    await cleanupAll(() => deleteFlow(page, id));
  }
});

test("スケジュール: スマホのシートでも曜日と次の実行が出て、横にはみ出さない（BUG-95）", async ({ browser, page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  let id = "";
  try {
    id = await createScheduledFlow(page, "30 8 * * 1");
    const ctx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 3,
      ignoreHTTPSErrors: true,
    });
    try {
      const phone = await ctx.newPage();
      await phone.goto(`/workflows/${id}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /スケジュール/ }).first().click();
      const sheet = phone.getByRole("dialog");
      // 保存済みの 1 は月曜（cron クレートの番号の日曜ではない）。
      await expect(sheet.getByRole("combobox", { name: "曜日" })).toHaveText("月曜日");
      await expect.poll(() => previewWeekdays(phone)).toEqual(["月 8:30", "月 8:30"]);
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/schedule-weekly-phone.png` });
    } finally {
      await ctx.close();
    }
  } finally {
    await cleanupAll(() => deleteFlow(page, id));
  }
});
