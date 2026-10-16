import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 回答の本文の ```ui（Yui）のボタン（#604）。押すとサーバーが保存済みの本文から図を読み直し、
/// 値を検証してから会話へ送る・ワークフローを起動する。
/// 前提: LLM=stub。`yui:ask` / `yui:plan` / `yui:app <ワークフロー名>` で操作を持つ図が返る。

async function send(page: Page, text: string) {
  await page.goto("/");
  await page.getByLabel("メッセージを入力").fill(text);
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
}

/// BFF 経由で手動実行のワークフローを作る（workflows-enable.spec と同じ最小の IR）。
async function createFlow(page: Page, name: string) {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
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
  expect(res.ok()).toBeTruthy();
}

test.describe("```ui の図のボタン", () => {
  test("質問のカードの回答が会話に送られ、二度は送れない", async ({ page }) => {
    await loginViaKeycloak(page);
    await send(page, "yui:ask");
    const figure = page.getByTestId("ui-figure");
    await expect(figure).toHaveAttribute("data-ui-kind", "ask", { timeout: 30_000 });
    const submit = figure.getByRole("button", { name: "回答する" });
    await expect(submit).toBeEnabled({ timeout: 30_000 });

    // 必須の質問に答えないと送れない（図の中で止まり、会話には何も増えない）。
    await submit.click();
    await expect(figure.getByText("回答してください").first()).toBeVisible();

    await figure.getByRole("radio", { name: "関西" }).check();
    await figure.getByRole("checkbox", { name: "費用" }).check();
    await figure.getByRole("checkbox", { name: "速さ" }).check();
    await submit.click();
    await expect(figure.getByRole("status")).toContainText("送信しました");
    // 回答は自分の発話として会話に入る。
    await expect(page.getByText(/関西/).filter({ hasNot: figure }).first()).toBeVisible({ timeout: 30_000 });

    // 読み直しても「回答済み」のまま（サーバーの 1 回だけの記録）。
    await page.reload();
    const again = page.getByTestId("ui-figure").first();
    await expect(again.getByText("回答済みです")).toBeVisible({ timeout: 30_000 });
    await expect(again.getByRole("button", { name: "回答する" })).toBeDisabled();
  });

  test("計画のカードで「この計画で進める」を送れる", async ({ page }) => {
    await loginViaKeycloak(page);
    await send(page, "yui:plan");
    const figure = page.getByTestId("ui-figure");
    await expect(figure).toHaveAttribute("data-ui-kind", "plan", { timeout: 30_000 });
    const approve = figure.getByRole("button", { name: "この計画で進める" });
    await expect(approve).toBeEnabled({ timeout: 30_000 });
    await approve.click();
    await expect(figure.getByRole("status")).toContainText("送信しました");
  });

  test("アプリのボタンでワークフローを起動し、起動の記録が図の下に残る", async ({ page }) => {
    test.setTimeout(120_000);
    await loginViaKeycloak(page);
    const name = uniqueName("yui-app");
    await createFlow(page, name);
    await send(page, `yui:app ${name}`);
    const figure = page.getByTestId("ui-figure");
    await expect(figure).toHaveAttribute("data-ui-kind", "app", { timeout: 30_000 });
    const apply = figure.getByRole("button", { name: "申請する" });
    await expect(apply).toBeEnabled({ timeout: 30_000 });

    await figure.getByLabel("金額").fill("1200");
    await figure.getByRole("radio", { name: "交通費" }).click();
    await apply.click();
    // ワークフローを起動するボタンは確認を挟む。
    const confirm = figure.getByRole("group", { name: "実行の確認" });
    await expect(confirm).toContainText(`「${name}」をこの内容で起動します。`);
    await confirm.getByRole("button", { name: "実行する" }).click();
    await expect(figure.getByText(`ワークフロー「${name}」を起動しました`)).toBeVisible({ timeout: 30_000 });

    // 起動の記録（誰の・どのワークフローか・送った値）が図の下に出て、終われば成功になる。
    const runs = page.getByTestId("yui-runs");
    await expect(runs).toContainText(`申請する → ${name}`, { timeout: 30_000 });
    await expect(runs).toContainText("金額");
    await expect(runs.locator("li").first()).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });

    // 読み直しても記録は残る（会話の正本はサーバー）。
    await page.reload();
    await expect(page.getByTestId("yui-runs")).toContainText(`申請する → ${name}`, { timeout: 30_000 });
  });

  test("見つからないワークフローのボタンは押しても起動せず、理由が出る", async ({ page }) => {
    await loginViaKeycloak(page);
    const name = uniqueName("yui-missing");
    await send(page, `yui:app ${name}`);
    const figure = page.getByTestId("ui-figure");
    await expect(figure).toHaveAttribute("data-ui-kind", "app", { timeout: 30_000 });
    const apply = figure.getByRole("button", { name: "申請する" });
    await expect(apply).toBeEnabled({ timeout: 30_000 });
    await figure.getByLabel("金額").fill("500");
    await apply.click();
    const confirm = figure.getByRole("group", { name: "実行の確認" });
    if (await confirm.isVisible().catch(() => false)) await confirm.getByRole("button", { name: "実行する" }).click();
    await expect(figure.getByText(`ワークフロー「${name}」が見つからないか、使う権限がありません`)).toBeVisible();
    await expect(page.getByTestId("yui-runs")).toHaveCount(0);
  });
});
