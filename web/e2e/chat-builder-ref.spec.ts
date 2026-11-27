import { expect, test, type Page, type Route } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 作ったテーブル・保存ビュー・ミニアプリ・ワークフローへの参照カード（builder_ref・D-204・T02）。
///
/// 前提: LLM=stub。T02 の時点では builder_ref を出す道具がまだ無いので、実際の会話の回答を読み込むときに、
/// メッセージ一覧の応答の最後の回答へ builder_ref のブロックを足して描かせる（backend は値を JSON のまま返す）。
/// 形の崩れたブロックは描かない。カードはそれぞれの画面への導線を持つ。
/// `E2E_SHOTS_DIR` を渡すと PC と電話の幅のスクリーンショットを保存する。

const SHOTS = process.env.E2E_SHOTS_DIR;
const PHONE = { width: 390, height: 844 };

const REFS = [
  { kind: "workflow", id: "11111111-1111-4111-8111-111111111111", name: "経費の確認", version: 1 },
  { kind: "data_table", id: "22222222-2222-4222-8222-222222222222", name: "顧客", version: 1 },
  { kind: "data_view", id: "33333333-3333-4333-8333-333333333333", name: "今月の売上", version: 3 },
  {
    kind: "mini_app",
    id: "44444444-4444-4444-8444-444444444444",
    name: "経費の申請アプリ（とても長い名前の例で、カードの幅に収まらないときは省略して出す）",
    version: 2,
  },
];

async function send(page: Page, text: string): Promise<void> {
  const input = page.getByLabel("メッセージを入力");
  await input.click();
  await input.fill(text);
  await page.getByRole("button", { name: "送信" }).click();
}

/// メッセージ一覧の最後の回答に builder_ref のブロック（と形の崩れたもの 1 件）を足して返す。
async function withBuilderRefs(route: Route): Promise<void> {
  if (route.request().method() !== "GET") return route.fallback();
  const res = await route.fetch();
  const body = (await res.json()) as { messages: { role: string; content: unknown[] }[] };
  const last = [...body.messages].reverse().find((m) => m.role === "assistant");
  if (last) {
    for (const item of REFS) last.content.push({ type: "builder_ref", item });
    last.content.push({ type: "builder_ref", item: { kind: "unknown", id: "x", name: "壊れた参照", version: 1 } });
  }
  await route.fulfill({ response: res, json: body });
}

test.describe("作ったものの参照カード", () => {
  test("種類ごとのカードを描き、それぞれの画面へ導く", async ({ page }) => {
    await loginViaKeycloak(page);
    await page.goto("/");
    const tag = Date.now().toString(36);
    await send(page, `参照カードの確認 ${tag}`);
    await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });
    await expect(page.getByText(`回答: 参照カードの確認 ${tag}`).first()).toBeVisible({ timeout: 30_000 });
    await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, { timeout: 60_000 });

    await page.route(/\/api\/threads\/[0-9a-f-]+\/messages(\?.*)?$/, withBuilderRefs);
    await page.reload();
    const saved = page.locator('[data-message-state="saved"]').filter({
      hasText: `回答: 参照カードの確認 ${tag}`,
    });
    const cards = saved.getByTestId("builder-ref-card");
    await expect(cards).toHaveCount(REFS.length, { timeout: 30_000 });
    // 形の崩れた参照は描かない。
    await expect(page.getByText("壊れた参照")).toHaveCount(0);

    const expected = [
      { label: "ワークフロー", open: "エディタで開く", href: `/workflows/${REFS[0].id}` },
      { label: "テーブル", open: "テーブルを開く", href: `/data/${REFS[1].id}` },
      { label: "保存ビュー", open: "ビューを開く", href: null },
      { label: "ミニアプリ", open: "アプリを開く", href: `/apps/${REFS[3].id}` },
    ];
    for (const [i, want] of expected.entries()) {
      const card = cards.nth(i);
      await expect(card).toContainText(REFS[i].name.slice(0, 8));
      await expect(card).toContainText(`v${REFS[i].version}`);
      await expect(card).toContainText(want.label);
      if (want.href) {
        await expect(card.getByRole("link", { name: want.open })).toHaveAttribute("href", want.href);
      } else {
        await expect(card.getByRole("button", { name: want.open })).toBeVisible();
      }
    }
    // 長い名前は 1 行に省略して、カードからはみ出さない。
    const longName = cards.nth(3).getByTitle(REFS[3].name);
    const [nameBox, cardBox] = await Promise.all([longName.boundingBox(), cards.nth(3).boundingBox()]);
    expect(nameBox && cardBox && nameBox.x + nameBox.width <= cardBox.x + cardBox.width).toBeTruthy();
    if (SHOTS) await saved.screenshot({ path: `${SHOTS}/builder-ref-cards-pc.png` });

    // 保存ビューはビューを読んでテーブルの画面へ移る。読めなければその場で言う（ここではビューが無い）。
    await cards.nth(2).getByRole("button", { name: "ビューを開く" }).click();
    await expect(cards.nth(2).getByRole("alert")).toHaveText("開けませんでした", { timeout: 15_000 });
    await expect(page).toHaveURL(/\/c\/[0-9a-f-]+/i);

    // 電話の幅でもカードの中に収まる（横にはみ出さない）。
    await page.setViewportSize(PHONE);
    await expect(cards).toHaveCount(REFS.length);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(0);
    for (let i = 0; i < REFS.length; i += 1) {
      const box = await cards.nth(i).boundingBox();
      expect(box && box.x >= 0 && box.x + box.width <= PHONE.width).toBeTruthy();
    }
    if (SHOTS) await saved.screenshot({ path: `${SHOTS}/builder-ref-cards-phone.png` });

    // テーブルのカードからテーブルの画面へ移る（ここではテーブルが無いので、開けない旨の画面になる）。
    await cards.nth(1).getByRole("link", { name: "テーブルを開く" }).click();
    await page.waitForURL(new RegExp(`/data/${REFS[1].id}$`), { timeout: 20_000 });
  });
});
