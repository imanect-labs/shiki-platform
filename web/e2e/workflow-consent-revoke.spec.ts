import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 自動実行のダイアログの同意と取り消しの e2e（BUG-102・D-90）と、AI の提供元の表示（D-99）:
/// - 対象（テーブル・フォルダー）の無いワークフローも、使ってよい操作が増えた版にダイアログから同意できる
///   （以前は 409 で止まった）。
/// - いま使えるようにしている人と対象の一覧を出す。「自動実行をオフにする」は、取り消しを選ばなければ権限を残し、
///   「権限も取り消す」を選ぶと取り消す（オフにした後でも取り消せる）。
/// - AI のブロックに入れた内容の送り先の提供元を、自動実行のダイアログとモデルの欄の「詳しく」で言う。
///
/// スケジュールは発火しない時刻（毎年 1 月 1 日 3:00）にし、オンにしたワークフローは最後にオフにして消す。
/// 実 LLM を呼ばない（AI のブロックのあるワークフローはオンにせず、実行もしない）。

const SHOTS = process.env.E2E_SHOTS_DIR;
/// 試験のあいだに発火しないスケジュール。
const NEVER_SOON = { kind: "schedule", cron: "0 3 1 1 *", tz: "Asia/Tokyo", catchup: "skip" } as const;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type Registration = {
  status: string;
  enabled_version: number | null;
  consented_scopes: string[];
  delegations: { object_kind: string | null; object_id: string | null; relation: string }[];
};

async function registration(page: Page, id: string): Promise<Registration> {
  const res = await page.request.get(`/api/workflows/${id}/registration`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()) as Registration;
}

async function createFlow(page: Page, ir: Record<string, unknown>): Promise<string> {
  const res = await page.request.post("/api/workflows", { headers: await csrfHeaders(page), data: { ir } });
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}

/// オンにしたワークフローを、権限も取り消してオフにしてから消す（片付け）。
async function disableAndDelete(page: Page, id: string) {
  const off = await page.request.post(`/api/workflows/${id}/disable`, {
    headers: await csrfHeaders(page),
    data: { revoke_delegations: true },
  });
  expect(off.status(), await off.text()).toBeLessThan(300);
  const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
  expect(del.status(), await del.text()).toBeLessThan(300);
}

test("自動実行: 対象の無いワークフローも、使ってよい操作が増えた版にダイアログから同意できる（BUG-102）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("consent-no-target");
  const ir = {
    ir_version: 1,
    name,
    display_name: name,
    declared_scopes: [] as string[],
    triggers: [NEVER_SOON],
    nodes: [
      { id: "calc", type: "script.run", label: "計算する", params: { source: { inline: "function main(i){ return {}; }" } } },
    ],
    edges: [],
  };
  const id = await createFlow(page, ir);
  try {
    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: /自動実行を有効化/ }).click();
    const dialog = page.getByRole("dialog");
    await dialog.getByRole("button", { name: "同意してオンにする" }).click();
    await expect(page.getByText("自動実行をオンにしました").first()).toBeVisible({ timeout: 15_000 });
    expect((await registration(page, id)).enabled_version).toBe(1);

    // 使ってよい操作が増えた版（対象は無いまま）。
    const put = await page.request.put(`/api/workflows/${id}`, {
      headers: await csrfHeaders(page),
      data: { ir: { ...ir, declared_scopes: ["http.egress"] }, expected_version: 1 },
    });
    expect(put.ok(), await put.text()).toBeTruthy();
    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: "自動実行の設定" }).click();
    await expect(dialog.getByText("外部サービスへ送信する")).toBeVisible();
    if (SHOTS) await page.waitForTimeout(400).then(() => page.screenshot({ path: `${SHOTS}/consent-no-target-pc.png` }));
    await dialog.getByRole("button", { name: "同意してオンにする" }).click();
    await expect(page.getByText("自動実行をオンにしました").first()).toBeVisible({ timeout: 15_000 });
    const reg = await registration(page, id);
    expect(reg.enabled_version).toBe(2);
    expect(reg.consented_scopes).toEqual(["http.egress"]);
  } finally {
    await cleanupAll(() => disableAndDelete(page, id));
  }
});

test("自動実行: いま使えるようにしているものを一覧にし、オフにするときに権限も取り消せる（D-90）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const folderName = uniqueName("e2e-日報の箱");
  let folderId = "";
  let id = "";
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: folderName, parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;
    const name = uniqueName("consent-revoke");
    id = await createFlow(page, {
      ir_version: 1,
      name,
      display_name: name,
      declared_scopes: ["storage.read"],
      triggers: [NEVER_SOON],
      nodes: [{ id: "ls", type: "storage.list", label: "日報を数える", params: { folder: folderId } }],
      edges: [],
    });
    await page.goto(`/workflows/${id}`);
    const dialog = page.getByRole("dialog");
    await page.getByRole("button", { name: /自動実行を有効化/ }).click();
    await dialog.getByRole("button", { name: "同意してオンにする" }).click();
    await expect(page.getByText("自動実行をオンにしました").first()).toBeVisible({ timeout: 15_000 });

    // いま使えるようにしているもの: 対象を名前で、使えるようにした人を「あなた」で言う。
    await page.getByRole("button", { name: "自動実行の設定" }).click();
    const list = dialog.getByTestId("enable-delegations");
    await expect(list).toContainText(`フォルダー「${folderName}」のファイルを読む`);
    await expect(list).toContainText("あなたが使えるようにしました");
    await expect(dialog.getByText("自動実行では、このワークフローが下の人の代わりに、これらを使います。")).toBeVisible();

    // 取り消しを選ばずにオフにする: 権限は残る（今までどおり）。
    await dialog.getByRole("button", { name: "自動実行をオフにする" }).click();
    const confirm = dialog.getByTestId("enable-off-confirm");
    await expect(confirm).toContainText("自動実行をオフにしますか？");
    await expect(confirm).toContainText("オフにしても、ワークフローが使えるようにした権限は残ります。");
    await expect(confirm.getByRole("checkbox", { name: "権限も取り消す" })).not.toBeChecked();
    if (SHOTS) await page.waitForTimeout(400).then(() => page.screenshot({ path: `${SHOTS}/revoke-confirm-pc.png` }));
    await confirm.getByRole("button", { name: "オフにする", exact: true }).click();
    await expect(page.getByText("自動実行をオフにしました").first()).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText(/取り消すには「権限も取り消す」を押してください/).first()).toBeVisible();
    let reg = await registration(page, id);
    expect(reg.status).toBe("disabled");
    expect(reg.delegations).toEqual([expect.objectContaining({ object_kind: "folder", object_id: folderId })]);

    // オフのあとで取り消す。
    await page.getByRole("button", { name: /自動実行を有効化/ }).click();
    await expect(dialog.getByText("自動実行はオフですが、ワークフローが使えるようにした権限は残っています。")).toBeVisible();
    if (SHOTS) await page.waitForTimeout(400).then(() => page.screenshot({ path: `${SHOTS}/revoke-after-off-pc.png` }));
    await dialog.getByRole("button", { name: "権限も取り消す" }).click();
    await expect(confirm).toContainText("権限も取り消しますか？");
    await confirm.getByRole("button", { name: "権限も取り消す" }).click();
    await expect(page.getByText("権限を取り消しました").first()).toBeVisible({ timeout: 15_000 });
    await expect(dialog).toHaveCount(0);
    reg = await registration(page, id);
    expect(reg.delegations).toEqual([]);
    expect(reg.consented_scopes).toEqual([]);
    await page.getByRole("button", { name: /自動実行を有効化/ }).click();
    await expect(dialog.getByTestId("enable-delegations")).toHaveCount(0);

    // もう一度オンにして、オフにするときに「権限も取り消す」を選ぶ。
    await dialog.getByRole("button", { name: "同意してオンにする" }).click();
    await expect(dialog).toHaveCount(0, { timeout: 15_000 });
    await expect.poll(async () => (await registration(page, id)).delegations.length).toBe(1);
    await page.getByRole("button", { name: "自動実行の設定" }).click();
    await dialog.getByRole("button", { name: "自動実行をオフにする" }).click();
    await confirm.getByRole("checkbox", { name: "権限も取り消す" }).check();
    await confirm.getByRole("button", { name: "オフにして権限も取り消す" }).click();
    await expect(page.getByText("自動実行をオフにし、権限も取り消しました").first()).toBeVisible({ timeout: 15_000 });
    await expect(dialog).toHaveCount(0);
    reg = await registration(page, id);
    expect(reg.status).toBe("disabled");
    expect(reg.delegations).toEqual([]);

    // スマホ幅: 一覧と確認が横にはみ出さない。
    const enable = await page.request.post(`/api/workflows/${id}/enable`, {
      headers: await csrfHeaders(page),
      data: {
        version: 1,
        consent: true,
        grants: [{ scope: "storage.read", object_type: "folder", object_id: folderId, relation: "viewer" }],
      },
    });
    expect(enable.ok(), await enable.text()).toBeTruthy();
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
      await phone.getByTestId("chrome-menu").click();
      await phone.getByRole("menu").getByRole("menuitem", { name: "自動実行の設定" }).click();
      const sheet = phone.getByRole("dialog");
      await expect(sheet.getByTestId("enable-delegations")).toContainText(`フォルダー「${folderName}」のファイルを読む`);
      await sheet.getByRole("button", { name: "自動実行をオフにする" }).click();
      await expect(sheet.getByTestId("enable-off-confirm")).toBeVisible();
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.waitForTimeout(400).then(() => phone.screenshot({ path: `${SHOTS}/revoke-confirm-phone.png` }));
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      async () => {
        if (id) await disableAndDelete(page, id);
      },
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});

type ModelOption = { id: string; label: string; provider_label: string; processed_abroad: boolean };

test("AI の提供元: 自動実行のダイアログとモデルの欄の「詳しく」で、入れた内容の送り先を言う（D-99）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const listed = await page.request.get("/api/llm/models?purpose=workflow");
  expect(listed.ok(), await listed.text()).toBeTruthy();
  const models = ((await listed.json()) as { models: ModelOption[] }).models;
  // 国外処理のモデルがあればそれを選ぶ（印も確かめる）。保存するだけで、実行もオンにもしない（実 LLM を呼ばない）。
  const model = models.find((m) => m.processed_abroad) ?? models[0];
  expect(model, "ワークフローで選べるモデルが無い").toBeTruthy();
  const name = uniqueName("ai-provider");
  const id = await createFlow(page, {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [NEVER_SOON],
    nodes: [
      { id: "ask", type: "llm.invoke", label: "日報をまとめる", params: { prompt: "今日の日報をまとめてください", model: model!.id } },
    ],
    edges: [],
  });
  try {
    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: /自動実行を有効化/ }).click();
    const note = page.getByRole("dialog").getByTestId("enable-ai-providers");
    await expect(note).toContainText(`AI のブロックに入れた内容は、${model!.provider_label}`);
    await expect(note).toContainText("に送られます。");
    if (model!.processed_abroad) await expect(note.getByText("国外処理")).toBeVisible();
    if (SHOTS) await page.waitForTimeout(400).then(() => page.screenshot({ path: `${SHOTS}/ai-provider-enable-pc.png` }));
    await page.keyboard.press("Escape");

    // モデルの欄の「詳しく」。
    await page.locator(".react-flow").getByText("日報をまとめる").first().click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const info = panel.getByTestId("workflow-model-provider");
    await info.getByRole("button", { name: "詳しく" }).click();
    await expect(info).toContainText(
      `入れた内容は、組織の AI の設定で決めたモデルの提供元に送られます（いま: ${model!.provider_label}）。`,
    );
    if (model!.processed_abroad) await expect(info.getByText("国外処理")).toBeVisible();
    if (SHOTS) await page.waitForTimeout(400).then(() => page.screenshot({ path: `${SHOTS}/ai-provider-model-field-pc.png` }));
    const reg = await registration(page, id);
    expect(reg.status).toBe("none");
  } finally {
    await cleanupAll(async () => {
      const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});
