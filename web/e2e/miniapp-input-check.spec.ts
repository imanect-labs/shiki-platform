import { test, expect, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, trackCreatedResources, uniqueName } from "./helpers";

/// 画面の入力と、束ねたワークフローの入力項目の突き合わせ（GAP-12）の E2E。
/// - 「書く」タブでも、組み立てると同じ突き合わせ（必須の入力に当たる部品が無い・種類が違う・必須でない・無い入力）を、
///   図と部品の名前つきで知らせる。保存は止めない。
/// - 保存すると、サーバーが同じ規則で確かめ直し、応答の input_warnings と知らせで伝える（公開は止めない）。
/// PC（1440px）とスマホ（390px）で確かめる。作ったもの（アプリ・ワークフロー）は最後に消す。
/// `E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

/// 入力項目（件名・金額）を決めたワークフローを作る（最後に消す）。
async function createFlow(page: Page, name: string): Promise<void> {
  const res = await page.request.post("/api/workflows", {
    headers: { "Content-Type": "application/json", "X-CSRF-Token": await csrf(page) },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: {
          type: "object",
          required: ["title", "amount"],
          "x-order": ["title", "amount"],
          properties: {
            title: { type: "string", title: "件名" },
            amount: { type: "integer", title: "金額" },
          },
        },
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
  expect(res.ok(), `POST /workflows → ${res.status()} ${await res.text()}`).toBeTruthy();
  const { id } = (await res.json()) as { id: string };
  cleanup.add(() => removeWorkflowApi(page, id));
}

for (const width of [1440, 390] as const) {
  test(`書くタブと保存で、画面の入力とワークフローの入力項目の食い違いを知らせる（GAP-12・${width}px）`, async ({ page }) => {
    test.setTimeout(150_000);
    await page.setViewportSize({ width, height: width === 390 ? 844 : 900 });
    await loginViaKeycloak(page);
    const flow = uniqueName("input-check-flow");
    await createFlow(page, flow);

    await page.goto("/apps");
    await page.getByRole("button", { name: "アプリを作成" }).first().click();
    const dialog = page.getByRole("dialog");
    await dialog.getByLabel("名前").fill(uniqueName("input-check-app"));
    await dialog.getByLabel("画面の見出し").fill("経費の申請");
    await dialog.getByLabel("追加するワークフロー").selectOption({ label: `${flow}（v1）` });
    await dialog.getByRole("button", { name: "ワークフローを追加" }).click();

    // 雛形（金額・区分・申請する）を書くタブで見る。件名の部品が無く、区分は入力項目に無い。
    await dialog.getByRole("tab", { name: "書く" }).click();
    const notes = dialog.getByTestId("miniapp-write-input-notes");
    await expect(notes).toContainText("画面の入力が、ワークフローの入力項目と合っていません。保存はできます。", {
      timeout: 15_000,
    });
    await expect(notes.getByRole("listitem")).toHaveText([
      `画面の図「経費の申請」のボタン「申請する」: 「${flow}」の必須の入力「件名」（title）に当たる部品がありません`,
      `画面の図「経費の申請」の「区分」: 「${flow}」に「category」という入力はありません（送っても使われないか、断られます）`,
    ]);

    // 記述を直す: 件名を足し、金額を必須でない文字にする。書いたそばから知らせが変わる。
    const source = dialog.locator("#app-screen");
    const written = (await source.inputValue())
      .replace("金額 = number 必須 円 as amount", "件名 = text 必須 as title\n金額 = text as amount")
      .replace("区分 = choice 交通費 | 交際費 as category\n", "");
    await source.fill(written);
    await expect(notes.getByRole("listitem")).toHaveText([
      `画面の図「経費の申請」の「金額」: 「${flow}」の「amount」は数値を受けます（この部品は文字）`,
      `画面の図「経費の申請」の「金額」: 「${flow}」では必須の入力です。「必須」にしてください`,
    ]);
    await notes.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-write-input-notes-${width}.png` });

    // 保存は止めない。サーバーも同じ規則で確かめ直し、知らせで伝える。
    const saved = page.waitForResponse((r) => r.url().endsWith("/api/mini-apps") && r.request().method() === "POST");
    await dialog.getByRole("button", { name: "作成" }).click();
    const res = await saved;
    expect(res.status()).toBe(201);
    const body = (await res.json()) as { warnings: unknown[]; input_warnings: { code: string; message: string }[] };
    expect(body.warnings).toEqual([]);
    expect(body.input_warnings.map((w) => w.code)).toEqual(["miniapp.input_type", "miniapp.input_required"]);
    expect(body.input_warnings[0]!.message).toBe(
      `画面の図「経費の申請」の「金額」: 「${flow}」の「amount」は数値を受けます（この部品は文字）`,
    );
    await expect(dialog).toHaveCount(0);
    const toast = page.getByText(
      "画面の入力が、ワークフローの入力項目と 2 か所合っていません。「書く」か「組み立てる」で確かめてください。",
      { exact: true },
    );
    await expect(toast).toBeVisible({ timeout: 15_000 });
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-input-warnings-toast-${width}.png` });
  });
}
