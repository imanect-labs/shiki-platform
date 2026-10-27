import { expect, test, type Browser, type Locator, type Page } from "@playwright/test";

import { loginViaKeycloak, stubChatModel, uniqueName } from "./helpers";

/// 「AI に聞く」の思考の深さが組織の設定に従うので使われないとき、保存時の警告を欄の下に出す（D-75）。
///
/// - 組織の既定が Auto でモデルを省略すると、思考の深さの欄の下に「Auto の設定に従う」旨を出す（注意の帯には出さない）。
///   欄の説明（読み上げ）にも同じ文言が付く。
/// - モデルを選ぶと警告は消える（選んだモデルで動くので、書いた思考の深さが使われる）。
/// - 組織がモデルの選択を禁じると、既定と同じモデルを選んでいても警告を出す。
/// - スマホ（390px）のシートでも欄の下に出て、横にはみ出さない。
/// 前提: compose の shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。実行はしない（実 LLM を呼ばない）。
/// 組織の AI 設定を変えたら最後に戻し、作ったワークフローは消す。`E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

type AiSettings = { default_model: string; allow_user_choice: boolean };

async function csrfHeader(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function aiSettings(page: Page): Promise<AiSettings> {
  const res = await page.request.get("/api/org-admin/ai/models");
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { settings: AiSettings }).settings;
}

async function putSettings(page: Page, data: Partial<AiSettings>) {
  const res = await page.request.put("/api/org-admin/ai/settings", { headers: await csrfHeader(page), data });
  expect(res.ok(), await res.text()).toBeTruthy();
}

/// モデルを省略して思考の深さだけを書いた「AI に聞く」1 つのワークフロー。
async function createFlow(page: Page): Promise<string> {
  const name = uniqueName("wf-effort");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeader(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        nodes: [
          {
            id: "ask",
            type: "llm.invoke",
            label: "要約する",
            params: { prompt: "今日の予定をまとめてください", effort: "low" },
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
  const res = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeader(page) });
  expect([204, 404], `DELETE /artifacts/${id} → ${res.status()}`).toContain(res.status());
}

async function openBlock(page: Page, id: string): Promise<Locator> {
  await page.goto(`/workflows/${id}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("要約する").first()).toBeVisible({ timeout: 15_000 });
  await canvas.getByText("要約する").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel).toBeVisible();
  return panel;
}

const AUTO_WARNING = "思考の深さも Auto の設定に従うため";
const LOCK_WARNING = "この組織ではモデルを選べないため、思考の深さ（effort）は使われず";

test("思考の深さ: 組織の設定に従うときは欄の下で知らせ、モデルを選ぶと消える（D-75）", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  await page.setViewportSize({ width: 1440, height: 1000 });
  const stub = await stubChatModel(page);
  test.skip(!stub, "組織で使える stub のモデルが無い構成");
  const original = await aiSettings(page);
  const id = await createFlow(page);
  try {
    // 組織の既定を Auto（選べる）にした状態で開く（元から Auto なら変えない）。
    if (original.default_model !== "auto" || !original.allow_user_choice) {
      await putSettings(page, { default_model: "auto", allow_user_choice: true });
    }
    const panel = await openBlock(page, id);
    const effort = panel.getByRole("combobox", { name: "思考の深さ（省略可）" });
    await expect(effort).toHaveText("速い");
    const warning = panel.locator("[data-field-warning]");
    await expect(warning).toContainText(AUTO_WARNING, { timeout: 15_000 });
    // 欄の説明（読み上げ）にも付き、注意の帯には重ねて出さない。
    await expect(effort).toHaveAccessibleDescription(new RegExp(AUTO_WARNING));
    await expect(panel.getByTestId("node-warnings")).toHaveCount(0);
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/effort-warning-auto-pc.png` });

    // モデルを選ぶと、選んだモデルで動くので警告は消え、欄の注記に戻る（stub のモデルは思考の深さに対応しないので、
    // 送られない旨の注記になる）。
    await panel.getByTestId("workflow-model-button").click();
    await page.getByTestId("workflow-model-picker").getByTestId(`model-option-${stub!.id}`).click();
    await expect(warning).toHaveCount(0, { timeout: 15_000 });
    await expect(effort).toHaveAccessibleDescription(/組織の既定より優先します|このモデルは思考の深さに対応しないため/);
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });

    // 組織がモデルの選択を禁じる（既定は選んだ stub のモデル）: 既定と同じモデルでも思考の深さは使われない。
    await putSettings(page, { default_model: stub!.id, allow_user_choice: false });
    const locked = await openBlock(page, id);
    await expect(locked.locator("[data-field-warning]")).toContainText(LOCK_WARNING, { timeout: 15_000 });
    await expect(locked.getByTestId("node-warnings")).toHaveCount(0);
    if (SHOTS) await locked.screenshot({ path: `${SHOTS}/effort-warning-locked-pc.png` });
  } finally {
    await putSettings(page, { default_model: original.default_model, allow_user_choice: original.allow_user_choice });
    await deleteFlow(page, id);
  }
});

test("思考の深さ: スマホのシートでも欄の下で知らせ、横にはみ出さない（D-75）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  const phone = await phonePage(browser, page);
  const original = await aiSettings(page);
  const id = await createFlow(page);
  try {
    if (original.default_model !== "auto" || !original.allow_user_choice) {
      await putSettings(page, { default_model: "auto", allow_user_choice: true });
    }
    await phone.goto(`/workflows/${id}`);
    const flow = phone.getByTestId("workflow-mobile-flow");
    await expect(flow).toBeVisible({ timeout: 15_000 });
    await flow.getByRole("button", { name: /要約する/ }).filter({ hasText: "要約する" }).click();
    const sheet = phone.getByRole("dialog");
    const warning = sheet.locator("[data-field-warning]");
    await expect(warning).toContainText(AUTO_WARNING, { timeout: 15_000 });
    await warning.scrollIntoViewIfNeeded();
    expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/effort-warning-auto-phone.png` });
  } finally {
    await putSettings(page, { default_model: original.default_model, allow_user_choice: original.allow_user_choice });
    await deleteFlow(page, id);
    await phone.context().close();
  }
});

/// ログインは PC の画面で済ませ、そのセッションをタッチ端末の文脈へ持ち込む（workflow-model-default.spec と同じ）。
async function phonePage(browser: Browser, page: Page): Promise<Page> {
  await loginViaKeycloak(page);
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  return ctx.newPage();
}
