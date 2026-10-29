import { expect, test } from "@playwright/test";

import { setup, teardown } from "./approval-fixture";
import { cleanupQueue, loginViaKeycloak } from "./helpers";

/// 「承認を待つ」の承認者の欄の注記（BUG-110・付録 D の N3.4.1）:
/// 承認者は「承認」の画面に依頼を出す相手を決めるだけで、判断できるかは状態の進め方が決める。状態の進め方が申請者を
/// 除いていなければ、申請者は自分の申請を承認できる（api の workflow_approval_it で確かめてある）。欄の注記と「詳しく」で
/// そのことと、防ぎ方を言う。作ったものは最後に片付ける（待っている実行を止めてから消す）。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

test("承認を待つ: 承認者の欄は、判断できる人を決めないことと、申請者が自分で承認できうることを言う（BUG-110）", async ({
  page,
  browser,
}) => {
  test.setTimeout(150_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const s = await setup(page);
  cleanup.add(() => teardown(page, s));
  await page.goto(`/workflows/${s.workflowId}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("上長の承認").first()).toBeVisible({ timeout: 15_000 });
  await canvas.getByText("上長の承認").first().click();
  const panel = page.getByRole("complementary", { name: "ブロックの設定" });
  await expect(panel.getByText("承認者", { exact: true })).toBeVisible();
  await expect(panel.getByTestId("approval-assignees-note")).toHaveText(
    "選んだ人の「承認」の画面に依頼が出ます。判断できるかは状態の進め方で決まります。",
  );
  const more = panel.getByTestId("approval-assignees-more");
  await more.getByRole("button", { name: "詳しく" }).click();
  await expect(more).toContainText("ここで選ぶのは依頼を出す相手で、判断できる人を決めるものではありません。");
  await expect(more).toContainText("状態の進め方によっては、申請者が自分の申請を承認できます。");
  await expect(more).toContainText("いまは画面では変えられません。管理者に頼んでください。");
  await more.scrollIntoViewIfNeeded();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/bug110-assignees-note-pc.png` });

  // スマホのシートでも同じ注記が出て、横にはみ出さない。
  const phoneCtx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  cleanup.add(() => phoneCtx.close());
  const phone = await phoneCtx.newPage();
  await phone.goto(`/workflows/${s.workflowId}`);
  const flow = phone.getByTestId("workflow-mobile-flow");
  await expect(flow).toBeVisible({ timeout: 15_000 });
  await flow.getByRole("button", { name: /上長の承認/ }).filter({ hasText: "上長の承認" }).click();
  const sheet = phone.getByRole("dialog");
  await expect(sheet.getByTestId("approval-assignees-note")).toBeVisible();
  await sheet.getByTestId("approval-assignees-more").getByRole("button", { name: "詳しく" }).click();
  await expect(sheet.getByTestId("approval-assignees-more")).toContainText("申請者が自分の申請を承認できます");
  await sheet.getByTestId("approval-assignees-more").scrollIntoViewIfNeeded();
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/bug110-assignees-note-phone.png` });
});
