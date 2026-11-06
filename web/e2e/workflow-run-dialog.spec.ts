import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 手動の実行のダイアログの e2e（BUG-109・D-93）:
/// - 説明は決まった文（使ってよい操作でさらに絞る）で言う。
/// - 必須の欄が空なら送らずに欄の下で知らせる。サーバーの入力の誤りは、本文を捨てず欄の下に出す（以前は
///   「API 400」とだけ出た）。どちらでも実行は作られない。
/// - ファイルが保存されたときのきっかけのワークフローは、「詳しい設定」のきっかけの内容（JSON）で試せる。書いた値が
///   `$from trigger` の源になり、空欄なら入力の値を使う。
/// - 「詳しい設定」を閉じても、送る値があることを見出しの下で言い、閉じて開き直しても値を残して開いた形で始める（BUG-229）。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function createFlow(page: Page, ir: Record<string, unknown>): Promise<string> {
  const res = await page.request.post("/api/workflows", { headers: await csrfHeaders(page), data: { ir } });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}

async function runCount(page: Page, id: string): Promise<number> {
  const res = await page.request.get(`/api/workflows/${id}/runs`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { items: unknown[] }).items.length;
}

test("実行のダイアログ: 決まった文で説明し、入力の誤りを欄の下に出す（BUG-109）", async ({ browser, page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("run-dialog-input");
  const id = await createFlow(page, {
    ir_version: 1,
    name,
    display_name: name,
    input_schema: {
      type: "object",
      required: ["amount"],
      properties: {
        amount: { type: "integer", minimum: 1, title: "金額", description: "税込みの金額（円）" },
        memo: { type: "string", title: "メモ" },
      },
      "x-order": ["amount", "memo"],
    },
    triggers: [{ kind: "interactive" }],
    nodes: [{ id: "calc", type: "script.run", label: "計算する", params: { source: { inline: "function main(i){ return i; }" } } }],
    edges: [],
  });
  try {
    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: "実行", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "いま実行する" });
    await expect(
      dialog.getByText("あなたの権限で 1 回実行します。このワークフローが使ってよい操作のうち、あなたにできることだけを行います。"),
    ).toBeVisible();
    await expect(dialog.getByText("読める範囲・書ける範囲はあなたと同じ")).toHaveCount(0);
    // 項目の説明は欄の下に出す（placeholder にしない）。
    await expect(dialog.getByText("税込みの金額（円）")).toBeVisible();

    // 必須の欄が空: 送らずに欄の下で知らせる。
    await dialog.getByRole("button", { name: "実行する" }).click();
    await expect(dialog.getByText("「金額」を入れてください。")).toBeVisible();
    await expect(dialog.getByRole("textbox", { name: "金額" }).or(dialog.getByRole("spinbutton", { name: "金額" }))).toHaveAttribute(
      "aria-invalid",
      "true",
    );

    // サーバーの入力の誤り: 欄の下に理由を出す（「API 400」にしない）。
    await dialog.getByRole("spinbutton", { name: "金額" }).fill("0");
    await dialog.getByRole("button", { name: "実行する" }).click();
    await expect(dialog.getByText("1 以上にしてください。")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText(/API 400/)).toHaveCount(0);
    if (SHOTS) await page.waitForTimeout(400).then(() => page.screenshot({ path: `${SHOTS}/run-dialog-input-error-pc.png` }));
    expect(await runCount(page, id)).toBe(0);

    // スマホ幅でも、誤りが欄の下に収まる。
    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 3,
    });
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/workflows/${id}`);
      await phone.getByRole("button", { name: "実行", exact: true }).click();
      const sheet = phone.getByRole("dialog", { name: "いま実行する" });
      await sheet.getByRole("spinbutton", { name: "金額" }).fill("0");
      await sheet.getByRole("button", { name: "実行する" }).click();
      await expect(sheet.getByText("1 以上にしてください。")).toBeVisible({ timeout: 15_000 });
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.waitForTimeout(400).then(() => phone.screenshot({ path: `${SHOTS}/run-dialog-input-error-phone.png` }));
    } finally {
      await phoneCtx.close();
    }
    expect(await runCount(page, id)).toBe(0);
  } finally {
    await cleanupAll(async () => {
      const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});

/// 実行できない設定の行は、名前を付けていないブロックもブロック名（カタログの名前）で言う。ブロックの ID は出さない
/// （利用者向けのダイアログ・付録 C の P1）。実行は作られないので AI は呼ばない。
test("実行のダイアログ: 実行できない設定は、名前を付けていないブロックもブロック名で言う", async ({ page }) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("run-dialog-unnamed");
  const id = await createFlow(page, {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "interactive" }],
    nodes: [{ id: "invoke_1", type: "llm.invoke", params: { prompt: "" } }],
    edges: [],
  });
  try {
    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: "実行", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "いま実行する" });
    await dialog.getByRole("button", { name: "実行する" }).click();
    await expect(dialog.getByText(/^「AI に聞く」: /)).toBeVisible({ timeout: 15_000 });
    await expect(dialog).not.toContainText("invoke_1");
    if (SHOTS) await page.waitForTimeout(400).then(() => page.screenshot({ path: `${SHOTS}/run-dialog-unnamed-pc.png` }));
    expect(await runCount(page, id)).toBe(0);
  } finally {
    await cleanupAll(async () => {
      const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});

type StepOutput = { output: unknown };
type RunDetail = { status: string };

test("実行のダイアログ: ファイルが保存されたときのワークフローを、きっかけの内容を入れて試せる（D-93）", async ({ page }) => {
  test.setTimeout(150_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let folderId = "";
  let id = "";
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: uniqueName("e2e-受付の箱"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;
    const name = uniqueName("run-dialog-trigger");
    id = await createFlow(page, {
      ir_version: 1,
      name,
      display_name: name,
      declared_scopes: ["storage.read"],
      triggers: [{ kind: "event", source: "storage.write", scope: { folder: folderId } }],
      nodes: [
        {
          id: "echo",
          type: "script.run",
          label: "きっかけを返す",
          params: { source: { inline: "function main(i){ return i; }" }, input: { $from: "trigger" } },
        },
      ],
      edges: [],
    });
    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: "実行", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "いま実行する" });
    await expect(
      dialog.getByText("このワークフローのきっかけは「ファイルが保存されたとき」ですが、ここでは試しに 1 回だけ実行します。"),
    ).toBeVisible();
    await expect(dialog.getByTestId("run-now-trigger")).toHaveCount(0);
    await dialog.getByRole("button", { name: "詳しい設定" }).click();
    const field = dialog.getByRole("textbox", { name: "きっかけの内容（JSON・省略可）" });
    // 項目のひな形はカタログのきっかけの項目から並べる。
    await dialog.getByRole("button", { name: "項目を並べる" }).click();
    await expect(field).toHaveValue(/"file_id": ""/);
    // 書き方の誤りは欄の下（送らない）。
    await field.fill('{"file_id": ');
    await dialog.getByRole("button", { name: "実行する" }).click();
    await expect(dialog.getByText("JSON の書き方が正しくありません。引用符、カンマ、括弧の対応を確かめてください。")).toBeVisible();
    await field.fill('["file"]');
    await dialog.getByRole("button", { name: "実行する" }).click();
    await expect(dialog.getByText("項目のまとまり（{ } で囲んだ JSON）で書いてください。")).toBeVisible();
    expect(await runCount(page, id)).toBe(0);
    if (SHOTS) await page.waitForTimeout(400).then(() => page.screenshot({ path: `${SHOTS}/run-dialog-trigger-pc.png` }));

    // 書いたきっかけの内容が `$from trigger` の源になる。
    const trial = { file_id: "e2e-file-1", version: 3, op: "update" };
    await field.fill(JSON.stringify(trial));
    // 「詳しい設定」を閉じても、送る値があることを見出しの下で言う（見えないまま前の値で実行しない・BUG-229）。
    const toggle = dialog.getByRole("button", { name: "詳しい設定" });
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    await expect(dialog.getByTestId("run-now-advanced-held")).toHaveText("入れたきっかけの内容で実行します。");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/run-dialog-trigger-held-pc.png` });
    // 閉じて開き直しても値は残し、開いた形で始める。
    await dialog.getByRole("button", { name: "閉じる" }).click();
    await expect(dialog).toHaveCount(0);
    await page.getByRole("button", { name: "実行", exact: true }).click();
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    await expect(field).toHaveValue(JSON.stringify(trial));
    await expect(dialog.getByTestId("run-now-advanced-held")).toHaveCount(0);
    await dialog.getByRole("button", { name: "実行する" }).click();
    await page.waitForURL(/\/runs\?run=/, { timeout: 20_000 });
    const runId = new URL(page.url()).searchParams.get("run")!;
    await expect
      .poll(
        async () => ((await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as RunDetail).status,
        { timeout: 60_000 },
      )
      .toBe("succeeded");
    const step = (await (
      await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=echo`)
    ).json()) as StepOutput;
    expect(step.output).toEqual(trial);
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
