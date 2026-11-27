import { expect, test } from "@playwright/test";

import { setup, teardown, waitForApproval } from "./approval-fixture";
import { cleanupQueue, loginViaKeycloak, uniqueName, writeFgaTuples } from "./helpers";

/// 所属する部署が ListObjects の上限（1,000 件）に届いた人にも、部署宛ての承認が「承認」の画面に出て判断できる
/// （BUG-161）。これまでは所属する部署の一覧を 1,000 件で打ち切り、上限の外の部署宛ての承認が黙って出なかった。
/// 部署の所属は OpenFGA に直接書く（1,000 件の詰め物を先に書き、承認の部署は後から入る＝上限の外の位置）。
/// 作ったもの（表・状態の進め方・ワークフローと待っている実行・タプル）は最後に片付ける（失敗したテストでも走る）。
/// `E2E_SHOTS_DIR` があれば、PC（1440px）とスマホ（390px）の画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;
const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

test("承認待ち: 所属する部署が多い人にも、部署宛ての承認が出て判断できる（BUG-161）", async ({ page, browser }) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const me = (await (await page.request.get("/api/me")).json()) as { id: string; tenant_id: string };
  const tag = uniqueName("e2e-many").replace(/[^A-Za-z0-9-]/g, "");
  const member = (role: string) => ({
    user: `user:${me.tenant_id}|${me.id}`,
    relation: "member",
    object: `role:${me.tenant_id}|${role}`,
  });
  cleanup.add(await writeFgaTuples(page, Array.from({ length: 1_000 }, (_, i) => member(`${tag}-filler-${i}`))));
  const dept = `${tag}-approvers`;
  cleanup.add(await writeFgaTuples(page, [member(dept)]));

  // 承認の担当は部署だけ（承認者の列の人には出さない）。判断は状態の進め方の「承認者の列の人」（本人）。
  const s = await setup(page, undefined, { assignees: [{ role: dept }] });
  cleanup.add(() => teardown(page, s));
  await waitForApproval(page, s);

  await page.goto("/approvals");
  const item = page.getByTestId("approval-item").filter({ hasText: s.tableName });
  await expect(item).toBeVisible({ timeout: 15_000 });
  await expect(item).toContainText("経費申請の承認");
  const count = (await (await page.request.get("/api/approvals/count")).json()) as { count: number };
  expect(count.count).toBeGreaterThanOrEqual(1);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/approvals-many-roles-pc.png` });

  if (SHOTS) {
    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 2,
    });
    cleanup.add(() => phoneCtx.close());
    const phone = await phoneCtx.newPage();
    await phone.goto("/approvals");
    await expect(phone.getByTestId("approval-item").filter({ hasText: s.tableName })).toBeVisible({ timeout: 15_000 });
    expect(await phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    await phone.screenshot({ path: `${SHOTS}/approvals-many-roles-phone.png` });
  }

  // 1 件を開いて判断できる（部署の担当として見つかり、状態の進め方で本人が承認できる）。
  await item.click();
  const sheet = page.getByTestId("approval-sheet");
  await expect(sheet).toBeVisible();
  await sheet.getByRole("radio", { name: /承認する/ }).click();
  const decided = page.waitForResponse((r) => r.url().includes("/decision") && r.request().method() === "POST");
  await sheet.getByTestId("approval-send").click();
  expect((await decided).status()).toBe(202);
  await expect(sheet).toBeHidden();
  await expect(page.getByTestId("approval-item").filter({ hasText: s.tableName })).toHaveCount(0);
});
