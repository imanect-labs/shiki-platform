import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// グリッドの表示（検証 B の UI 側・c1d96eef）: ユーザーの列はディレクトリの表示名、スモールセル抑制で伏せた
/// 集計は「件数が少ないため非表示」と理由を出し（空欄にしない）、見えていた行が消えたら件数を取り直す。
///
/// グリッドは canvas で文字を DOM に出さないので、表示はグリッドと同じ表示の部品（`displayCell`）で描く
/// カードのビューの DOM で確かめる。

const BOB = "00000000-0000-0000-0000-000000000002";

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function post(page: Page, path: string, data: unknown) {
  const res = await page.request.post(`/api${path}`, { headers: { "X-CSRF-Token": await csrf(page) }, data });
  expect(res.status(), await res.text()).toBeLessThan(300);
  return (await res.json()) as { id: string; rev: number };
}

test("ユーザーの列はディレクトリの表示名で出し、件数の少ない集計は理由を出す", async ({ page }) => {
  await loginViaKeycloak(page);
  const child = await post(page, "/data/tables", {
    name: uniqueName("e2e-明細"),
    schema: { fields: [{ key: "title", display_name: "題名", type: "text", indexed: true }] },
  });
  const item = await post(page, `/data/tables/${child.id}/records`, { data: { title: "明細1" } });
  const parent = await post(page, "/data/tables", {
    name: uniqueName("e2e-親"),
    schema: {
      fields: [
        { key: "name", display_name: "件名", type: "text", indexed: true },
        { key: "who", display_name: "担当", type: "user_ref" },
        { key: "items", display_name: "明細", type: "record_ref", ref_table: child.id, link: { cardinality: "many_to_many" } },
        { key: "n", display_name: "明細の数", type: "rollup", rollup: { link: "items", op: "count" } },
      ],
    },
  });
  await post(page, `/data/tables/${parent.id}/records`, { data: { name: "親1", who: BOB, items: [item.id] } });

  await page.goto(`/data/${parent.id}`);
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1");
  await page.getByRole("tab", { name: "カード" }).click();
  const gallery = page.getByTestId("gallery-grid");
  await expect(gallery.getByText("親1")).toBeVisible();
  // 1. ユーザーの列は UUID ではなく表示名。
  await expect(gallery.getByText("Bob", { exact: true })).toBeVisible();
  // 2. リンクは参照先のタイトル列。
  await expect(gallery.getByText("明細1", { exact: true })).toBeVisible();
  // 3. 参照先の行が既定の閾値（5 行）より少ない集計は、空欄ではなく理由を出す。
  await expect(gallery.getByText("件数が少ないため非表示")).toBeVisible();
  await expect(gallery.getByText(BOB)).toHaveCount(0);
});

test("見えていた行が消えたら件数を取り直す（削除の SSE）", async ({ page, browser }) => {
  await loginViaKeycloak(page);
  const t = await post(page, "/data/tables", {
    name: uniqueName("e2e-件数"),
    schema: { fields: [{ key: "name", display_name: "件名", type: "text", indexed: true }] },
  });
  await post(page, `/data/tables/${t.id}/records`, { data: { name: "残す" } });
  const gone = await post(page, `/data/tables/${t.id}/records`, { data: { name: "消える" } });
  await page.goto(`/data/${t.id}`);
  await expect(page.getByTestId("data-count")).toHaveText("2 件");

  // 別のブラウザ（別の人）が消す。こちらのグリッドは record.removed を受けて行を外し、件数を取り直す。
  const otherCtx = await browser.newContext();
  const other = await otherCtx.newPage();
  await loginViaKeycloak(other);
  const del = await other.request.delete(`/api/data/tables/${t.id}/records/${gone.id}?expected_rev=${gone.rev}`, {
    headers: { "X-CSRF-Token": await csrf(other) },
  });
  expect(del.status()).toBe(204);
  await expect(page.getByTestId("data-count")).toHaveText("1 件", { timeout: 15_000 });
  await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1");
  await otherCtx.close();
});
