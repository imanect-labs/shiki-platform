import { test, expect, type Page } from "@playwright/test";

import {
  cleanupQueue,
  deleteArtifactApi,
  loginViaKeycloak,
  removeWorkflowApi,
  trackCreatedResources,
  uniqueName,
} from "./helpers";

/// アプリの誤りの文言（GAP-41）の E2E。
/// - 保存の版の競合（409 version_conflict）: 「API 409」でなく、ほかの人が先に保存したことと、最新の内容を読み込む
///   ボタンを出す。読み込むと最新の内容で編集し直せる。「この内容を下書きにする」の競合も同じく言う。
/// - 同じ名前のアプリの作成（409 name_taken）: 何が起きたかと直し方を言う。
/// - ボタンの入力の誤り: 入力のキー（amount）でなく画面の部品の名前（金額）で言う。
/// - ボタンの 5xx: 503 は「いま使えません」、500 は「サーバーで問題が起きました」と言い、「送信の結果を確かめられません
///   でした」にしない。
/// 作ったもの（アプリ・ワークフロー）は最後に消す。`E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function api<T = Record<string, unknown>>(
  page: Page,
  method: "POST" | "PUT",
  path: string,
  body: unknown,
): Promise<T> {
  const res = await page.request.fetch(`/api${path}`, {
    method,
    headers: { "Content-Type": "application/json", "X-CSRF-Token": await csrf(page) },
    data: body,
  });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return (await res.json()) as T;
}

/// 金額（必須・整数）を入力項目に持つワークフローの IR（`label` はブロックの名前）。
function flowIr(name: string, label = "計算する") {
  return {
    ir_version: 1,
    name,
    display_name: name,
    triggers: [{ kind: "interactive" }],
    input_schema: {
      type: "object",
      required: ["amount"],
      properties: { amount: { type: "integer", title: "金額", minimum: 1 } },
    },
    nodes: [
      {
        id: "compute",
        type: "script.run",
        label,
        params: { source: { inline: "function main(i){ return { ok: true }; }" } },
      },
    ],
    edges: [],
  };
}

/// 金額（必須・整数）を入力項目に持つワークフロー（最後に消す）。
async function createFlow(page: Page): Promise<{ id: string; name: string }> {
  const name = uniqueName("errors-flow");
  const wf = await api<{ id: string }>(page, "POST", "/workflows", { ir: flowIr(name) });
  cleanup.add(() => removeWorkflowApi(page, wf.id));
  return { id: wf.id, name };
}

/// 画面の金額は必須にしていない（入力項目では必須）。
function screen(flow: string, lead = "経費の申請です。"): string {
  return `${lead}\n\n\`\`\`ui\napp 経費の申請\n金額 = number 円 as amount\nbutton 申請する -> workflow ${flow}\n\`\`\`\n`;
}

async function createApp(page: Page, flow: { id: string; name: string }, name = uniqueName("errors-app")) {
  const app = await api<{ id: string; version: number }>(page, "POST", "/mini-apps", {
    name,
    body: {
      description: "経費の申請",
      screen: screen(flow.name),
      workflows: [{ alias: flow.name, artifact_id: flow.id, version: 1 }],
    },
  });
  cleanup.add(() => deleteArtifactApi(page, app.id));
  return { id: app.id, name };
}

test.describe("アプリの誤りの文言（GAP-41）", () => {
  test("ほかの人が先に保存したら、変更が保存されていないことと読み込み直しを言う", async ({ page }) => {
    test.setTimeout(120_000);
    await loginViaKeycloak(page);
    const flow = await createFlow(page);
    const app = await createApp(page, flow);
    // 束ねたワークフローに新しいバージョンがある（最新の内容を読み込んだ後も、エディターがそれを言い続ける）。
    await api(page, "PUT", `/workflows/${flow.id}`, { ir: flowIr(flow.name, "改訂"), expected_version: 1 });
    await page.goto(`/apps/${app.id}`);
    await page.getByRole("button", { name: "編集" }).click();
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByTestId("miniapp-pin-update")).toContainText("新しいバージョン v2 があります", {
      timeout: 15_000,
    });
    await dialog.getByLabel(/画面の見出し/).fill("私が直した見出し");
    // 開いた後に、ほかの人が保存する（v2）。
    await api(page, "PUT", `/mini-apps/${app.id}`, {
      body: { description: "ほかの人が直した見出し", screen: screen(flow.name), workflows: [{ alias: flow.name, artifact_id: flow.id, version: 1 }] },
      expected_version: 1,
    });
    await dialog.getByRole("button", { name: "保存", exact: true }).click();
    const conflict = dialog.getByTestId("miniapp-save-conflict");
    await expect(conflict).toContainText("ほかの人が先にアプリを保存しました");
    await expect(conflict).toContainText("あなたの変更は保存されていません。");
    await expect(dialog.getByText(/API 409/)).toHaveCount(0);
    // 変更はまだ残っている（控えられる）。
    await expect(dialog.getByLabel(/画面の見出し/)).toHaveValue("私が直した見出し");
    await conflict.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-save-conflict-pc.png` });
    await conflict.getByRole("button", { name: "最新の内容を読み込む（変更は消えます）" }).click();
    await expect(dialog.getByLabel(/画面の見出し/)).toHaveValue("ほかの人が直した見出し", { timeout: 15_000 });
    await expect(dialog.getByTestId("miniapp-save-conflict")).toHaveCount(0);
    // 読み込み直した内容でも、束ねた部品の新しいバージョンを確かめ直す。
    await expect(dialog.getByTestId("miniapp-pin-update")).toContainText("新しいバージョン v2 があります", {
      timeout: 15_000,
    });
    await dialog.getByLabel(/画面の見出し/).fill("最新から直した見出し");
    await dialog.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText("v3 を下書きとして保存しました", { exact: true })).toBeVisible({ timeout: 15_000 });

    // スマホ幅でも、競合の知らせとボタンが横にはみ出さずに出る。
    {
      const ctx = await page.context().browser()!.newContext({
        storageState: await page.context().storageState(),
        viewport: { width: 390, height: 844 },
        hasTouch: true,
        isMobile: true,
        deviceScaleFactor: 2,
        ignoreHTTPSErrors: true,
      });
      try {
        const phone = await ctx.newPage();
        await phone.goto(`/apps/${app.id}`);
        await phone.getByRole("button", { name: "編集" }).click();
        const d = phone.getByRole("dialog");
        await expect(d.getByLabel(/画面の見出し/)).toHaveValue("最新から直した見出し", { timeout: 15_000 });
        await api(page, "PUT", `/mini-apps/${app.id}`, {
          body: { description: "スマホの間の見出し", screen: screen(flow.name), workflows: [{ alias: flow.name, artifact_id: flow.id, version: 1 }] },
          expected_version: 3,
        });
        await d.getByRole("button", { name: "保存", exact: true }).click();
        const c = d.getByTestId("miniapp-save-conflict");
        await expect(c).toBeVisible({ timeout: 15_000 });
        await c.scrollIntoViewIfNeeded();
        if (SHOTS) await phone.screenshot({ path: `${SHOTS}/miniapp-save-conflict-phone.png` });
        const overflow = await phone.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
        expect(overflow).toBeLessThanOrEqual(0);
      } finally {
        await ctx.close();
      }
    }

    // 前のバージョンの内容を下書きにする操作も、間にほかの保存が入ったら、そう言って読み直す。
    await page.getByRole("button", { name: "バージョン" }).click();
    const versions = page.getByRole("dialog");
    await versions
      .getByTestId("version-row")
      .filter({ has: page.getByTestId("version-label").getByText("v1", { exact: true }) })
      .getByRole("button", { name: "開く" })
      .click();
    await expect(page.getByTestId("miniapp-publish-bar")).toContainText("v1 を表示しています", { timeout: 15_000 });
    await api(page, "PUT", `/mini-apps/${app.id}`, {
      body: { description: "さらにほかの人が直した見出し", screen: screen(flow.name), workflows: [{ alias: flow.name, artifact_id: flow.id, version: 1 }] },
      expected_version: 4,
    });
    await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "この内容を下書きにする" }).click();
    await expect(page.getByText("ほかの人が先にアプリを保存しました", { exact: true })).toBeVisible({ timeout: 15_000 });
    await expect(
      page.getByText("最新の内容を読み込みました。もう一度「この内容を下書きにする」を押してください。", { exact: true }),
    ).toBeVisible();
    await expect(page.getByText(/API 409/)).toHaveCount(0);
    // 読み直した後は、そのまま押せば下書きにできる。
    await page.getByTestId("miniapp-publish-bar").getByRole("button", { name: "この内容を下書きにする" }).click();
    await expect(page.getByText("v1 の内容で v6 を下書きとして保存しました", { exact: true })).toBeVisible({
      timeout: 15_000,
    });
  });

  test("同じ名前のアプリを作ろうとすると、何が起きたかと直し方を言う", async ({ page }) => {
    test.setTimeout(120_000);
    await loginViaKeycloak(page);
    const flow = await createFlow(page);
    const taken = await createApp(page, flow);
    await page.goto("/apps");
    await page.getByRole("button", { name: "アプリを作成" }).first().click();
    const dialog = page.getByRole("dialog");
    await dialog.getByLabel(/名前/).fill(taken.name);
    await dialog.getByLabel(/画面の見出し/).fill("経費の申請");
    await dialog.getByLabel("追加するワークフロー").selectOption({ label: `${flow.name}（v1）` });
    await dialog.getByRole("button", { name: "ワークフローを追加" }).click();
    await dialog.getByRole("button", { name: "作成" }).click();
    await expect(dialog.getByRole("alert")).toHaveText(
      "同じ名前のアプリがすでにあります（あなたが見られないものを含みます）。別の名前にしてください。",
      { timeout: 15_000 },
    );
    await expect(dialog.getByText(/API 409/)).toHaveCount(0);
    if (SHOTS) await dialog.screenshot({ path: `${SHOTS}/miniapp-name-taken-pc.png` });
  });

  test("ボタンの入力の誤りは部品の名前で、5xx は起きたことごとに言う", async ({ page }) => {
    test.setTimeout(120_000);
    await loginViaKeycloak(page);
    const flow = await createFlow(page);
    const app = await createApp(page, flow);
    await page.goto(`/apps/${app.id}`);
    const figure = page.getByTestId("ui-figure");
    const press = async () => {
      await figure.getByRole("button", { name: "申請する" }).click();
      await figure.getByRole("group", { name: "実行の確認" }).getByRole("button", { name: "実行する" }).click();
    };
    await expect(figure.getByRole("button", { name: "申請する" })).toBeEnabled({ timeout: 15_000 });

    // 金額を空で押す（画面では必須でないが、ワークフローの入力項目では必須）。キー（amount）を出さない。
    await press();
    const status = figure.getByRole("status");
    await expect(status).toContainText("金額: 必須です", { timeout: 15_000 });
    await expect(status).not.toContainText("amount");
    if (SHOTS) await figure.screenshot({ path: `${SHOTS}/miniapp-button-input-error-pc.png` });

    // 503（実行先がいま使えない）と 500（サーバーの問題）。どちらも「確かめられませんでした」と言わない。
    let reply = 503;
    await page.route("**/api/mini-apps/*/ui-actions", (route) =>
      route.fulfill({
        status: reply,
        contentType: "application/json",
        body: JSON.stringify({ status: reply, title: reply === 503 ? "Service Unavailable" : "Internal Server Error" }),
      }),
    );
    await figure.getByLabel("金額").fill("1200");
    await press();
    await expect(status).toHaveText("このボタンはいま使えません。少し待ってから、もう一度押してください。");
    if (SHOTS) await figure.screenshot({ path: `${SHOTS}/miniapp-button-503-pc.png` });
    reply = 500;
    await press();
    await expect(status).toHaveText("サーバーで問題が起きました。もう一度押しても続くときは、管理者に連絡してください。");
    await expect(figure.getByText("送信の結果を確かめられませんでした")).toHaveCount(0);
  });
});
