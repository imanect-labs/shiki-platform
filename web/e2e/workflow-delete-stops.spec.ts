import { expect, test } from "@playwright/test";

import { csrfHeaders, runOf, setup, waitForApproval } from "./approval-fixture";
import { cleanupAll, loginViaKeycloak } from "./helpers";

/// ワークフローを消すと、動いている実行（承認待ちを含む）も止まり、承認者の受信箱から依頼が消える（BUG-128・D-105）。
/// 消す前の確かめで、動いている実行も止まることを言う（付録 C の §3.3 の型・論理削除）。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。承認者は申請者と同じ alice。
test("ワークフローの削除: 承認待ちの実行も止まり、承認者の受信箱から依頼が消える（BUG-128）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const s = await setup(page);
  let deleted = false;
  try {
    await waitForApproval(page, s);
    await runOf(page, s);
    const listed = (await (await page.request.get("/api/approvals")).json()) as {
      items: { approval_id: string; record: { id: string; rev: number } }[];
    };
    const item = listed.items.find((i) => i.record.id === s.recordId)!;
    await page.goto("/approvals");
    await expect(page.getByTestId("approval-item").filter({ hasText: s.tableName })).toBeVisible({ timeout: 15_000 });

    // 一覧から消す。確かめの文は、自動実行と動いている実行が止まること・戻せないことを言う。
    await page.goto("/workflows");
    // 削除の通知（toast も li）は名前を含むので、一覧の行だけを数える。
    const row = page
      .getByRole("listitem")
      .filter({ hasText: s.workflowName })
      .and(page.locator(':not([data-slot="toast"])'));
    await expect(row).toBeVisible({ timeout: 15_000 });
    await row.hover();
    await row.getByRole("button", { name: `「${s.workflowName}」を削除` }).click();
    const dialog = page.getByRole("dialog", { name: "ワークフローを削除しますか？" });
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText(
      `「${s.workflowName}」を削除すると、自動実行も、動いている実行も止まり、承認者に出している依頼も取り下げられます。元に戻せません。データは消さずに保管されます。`,
    );
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/workflow-delete-confirm-pc.png` });
    const removed = page.waitForResponse(
      (r) => r.url().includes(`/api/artifacts/${s.workflowId}`) && r.request().method() === "DELETE",
    );
    await dialog.getByRole("button", { name: "削除する" }).click();
    expect((await removed).status()).toBe(204);
    deleted = true;
    await expect(dialog).toBeHidden();
    await expect(row).toHaveCount(0);

    // 承認者の受信箱から消える（読み直しても出ず、件数にも数えない）。承認しようとしても続きは動かない。
    await page.goto("/approvals");
    // 一覧（ほかの承認）か空の状態が出るまで待ってから、消したワークフローの依頼が無いことを確かめる。
    await expect(
      page.getByTestId("approval-item").first().or(page.getByText("承認待ちはありません", { exact: true })),
    ).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId("approval-item").filter({ hasText: s.tableName })).toHaveCount(0);
    if (process.env.E2E_SHOTS_DIR) await page.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/workflow-delete-inbox-pc.png` });
    const again = (await (await page.request.get("/api/approvals")).json()) as { items: { record: { id: string } }[] };
    expect(again.items.some((i) => i.record.id === s.recordId)).toBe(false);
    const decided = await page.request.post(`/api/approvals/${item.approval_id}/decision`, {
      headers: await csrfHeaders(page),
      data: { decision: "approve", expected_rev: item.record.rev },
    });
    expect(decided.status(), await decided.text()).toBe(409);
    // 行は受付のまま（承認へ進まない）。
    const row2 = (await (await page.request.get(`/api/data/tables/${s.tableId}/records/${s.recordId}`)).json()) as {
      data: { status: string };
    };
    expect(row2.data.status).toBe("受付");
  } finally {
    const headers = await csrfHeaders(page);
    await cleanupAll(
      async () => {
        if (deleted) return;
        const del = await page.request.delete(`/api/artifacts/${s.workflowId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        const del = await page.request.delete(`/api/data/tables/${s.tableId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        const del = await page.request.delete(`/api/artifacts/${s.fsmId}`, { headers });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});
