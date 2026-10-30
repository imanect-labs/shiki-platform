import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 「差し込む」のメニューを閉じた直後に、ブロックの＋のメニューを開いても閉じない（BUG-160）。
///
/// 差し込むのメニュー（Radix の DropdownMenu）は、閉じきった後（消える動きの後）に文字の欄へフォーカスを戻す。以前は、
/// その間に開いた別のメニューからもフォーカスを奪い、開いたばかりの＋のメニューが外へのフォーカスとみなして閉じていた。
/// いまは、閉じきるまでにほかの場所へ移ったフォーカスは奪わない。差し込んだ後、続けて書けるように欄へ戻すのは同じ。

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

test("差し込むのメニューを閉じた直後に開いた、ブロックの＋のメニューが閉じない", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("wf-insert-focus");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: { type: "object", properties: { body: { type: "string", title: "本文" } }, required: ["body"] },
        nodes: [
          { id: "ask", type: "llm.invoke", label: "要約する", params: { prompt: "次の本文を要約してください: " } },
          { id: "polish", type: "llm.invoke", label: "整える", params: { prompt: "短く整えてください" } },
        ],
        edges: [{ from: "ask", from_port: "out", to: "polish" }],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  const id = ((await res.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));

  await page.goto(`/workflows/${id}`);
  const ask = page.getByTestId("rf__node-ask");
  const polish = page.getByTestId("rf__node-polish");
  await expect(ask).toBeVisible({ timeout: 20_000 });
  await ask.click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  const prompt = panel.locator('[data-value-expr="AI への指示"]');
  const text = prompt.getByRole("textbox", { name: "AI への指示" });
  await expect(text).toBeVisible();

  // 1. 差し込んだ後、欄へフォーカスが戻り、続けて書ける（直す前と同じ）。
  await text.click();
  await text.press("End");
  await prompt.getByRole("button", { name: "差し込む" }).click();
  await page.getByRole("menuitem", { name: "本文（body）", exact: true }).click();
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(text).toBeFocused();
  await expect(text).toHaveValue(/\{body\}$/);

  // 2. 差し込むのメニューが消えた直後（欄へフォーカスを戻すまでの数十ミリ秒）に、設定を開いているブロックの＋を押す。
  //    人が押すときと同じく、押したボタンにフォーカスが移ってから開く。待って押すと戻しの後になって起きないので、
  //    メニューが消えた瞬間に押す。＋のメニューは開いたままで、検索の欄にフォーカスがある（消えたメニューがフォーカスを
  //    欄へ戻して閉じさせない）。別のブロックの＋は、押すと設定がそのブロックに替わって欄が無くなるので起きない。
  await prompt.getByRole("button", { name: "差し込む" }).click();
  await expect(page.getByRole("menu")).toBeVisible();
  await page.evaluate(() => {
    const plus = document.querySelector<HTMLButtonElement>(
      '[data-testid="rf__node-ask"] button[aria-label="次のブロックを追加"]',
    );
    if (!plus) throw new Error("＋のボタンが無い");
    const watch = new MutationObserver(() => {
      if (document.querySelector('[role="menu"]')) return;
      watch.disconnect();
      plus.focus();
      plus.click();
    });
    watch.observe(document.body, { childList: true, subtree: true });
  });
  await page.getByRole("menuitem", { name: "本文（body）", exact: true }).click();
  const addMenu = page.getByTestId("add-node-menu");
  await expect(addMenu).toBeVisible();
  // フォーカスの戻し（メニューが消えた後の 0 ミリ秒のタイマー）が済むまで待っても、開いたまま。
  await page.waitForTimeout(800);
  await expect(addMenu).toBeVisible();
  await expect(addMenu.getByRole("searchbox", { name: "ブロックを検索" })).toBeFocused();
  await page.screenshot({ path: "test-results/insert-menu-then-add-menu-pc.png" });

  // 3. ふつうに押しても（メニューが消えた後に）、開いたまま。別のブロックの＋も同じ。
  await page.keyboard.press("Escape");
  await expect(addMenu).toHaveCount(0);
  await prompt.getByRole("button", { name: "差し込む" }).click();
  await page.getByRole("menuitem", { name: "本文（body）", exact: true }).click();
  await polish.getByRole("button", { name: "次のブロックを追加" }).click();
  await expect(addMenu).toBeVisible();
  await page.waitForTimeout(800);
  await expect(addMenu.getByRole("searchbox", { name: "ブロックを検索" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(addMenu).toHaveCount(0);
});
