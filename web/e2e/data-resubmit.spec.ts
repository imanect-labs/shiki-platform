import { expect, test, type Browser, type BrowserContext, type Page } from "@playwright/test";

import { csrfHeaders } from "./approval-fixture";
import { cleanupAll, deleteArtifactApi, deleteTableApi, loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// 「追加だけ」の申請者の出し直し（D-127・GAP-52）:
/// - alice（持ち主）が状態の進め方の画面で作成者の列を足し、「申請・承認のひな形」を使う。差し戻し → 申請中は
///   「レコードを追加した人」が進め、「追加だけ」の人にできることを画面と「詳しく」で言う。
/// - bob を「追加だけ」にして、bob が申請 → alice が差し戻し → bob が表のセルで直して申請中へ出し直す → alice が承認。
///   申請中・承認の後は直せない（セルが開かない・API でも 403）。PC（1440px）とスマホ（390px）で同じ流れを通す。
///
/// `E2E_SHOTS_DIR` があれば画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

/// グリッド（glide-data-grid の canvas）の座標（行の印の幅・列の既定の幅・見出しと行の高さ）。
const ROW_MARKER = 32;
const COL_WIDTH = 160;
const HEADER = 36;
const ROW = 34;

type Me = { id: string; tenant_id: string };
type Rec = { id: string; rev: number; data: Record<string, unknown> };

async function me(page: Page): Promise<Me> {
  return (await (await page.request.get("/api/me")).json()) as Me;
}

/// 申請のテーブル（状態は「申請中」だけ・承認者はメンバーの列）。
async function createTable(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: {
      name,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text", indexed: true },
          { key: "amount", display_name: "金額", type: "number" },
          { key: "approver", display_name: "承認者", type: "user_ref", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: ["申請中"], indexed: true },
        ],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  return ((await res.json()) as { id: string }).id;
}

async function fsmIdOf(page: Page, tableId: string): Promise<string | null> {
  const res = await page.request.get(`/api/data/tables/${tableId}`);
  if (!res.ok()) return null;
  const t = (await res.json()) as { schema: { fsm_ref?: { artifact_id: string } | null } };
  return t.schema.fsm_ref?.artifact_id ?? null;
}

async function addRecord(page: Page, tableId: string, data: Record<string, unknown>): Promise<Rec> {
  const res = await page.request.post(`/api/data/tables/${tableId}/records`, { headers: await csrfHeaders(page), data: { data } });
  expect(res.status(), await res.text()).toBe(201);
  return (await res.json()) as Rec;
}

async function recordOf(page: Page, tableId: string, id: string): Promise<Rec> {
  const res = await page.request.get(`/api/data/tables/${tableId}/records/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()) as Rec;
}

async function move(page: Page, tableId: string, id: string, to: string): Promise<Rec> {
  const rev = (await recordOf(page, tableId, id)).rev;
  const res = await page.request.post(`/api/data/tables/${tableId}/records/${id}/transition`, {
    headers: await csrfHeaders(page),
    data: { to, expected_rev: rev },
  });
  expect(res.status(), await res.text()).toBe(200);
  return (await res.json()) as Rec;
}

async function phoneOf(browser: Browser, from: BrowserContext): Promise<BrowserContext> {
  return browser.newContext({
    storageState: await from.storageState(),
    viewport: { width: 390, height: 844 },
    deviceScaleFactor: 2,
    isMobile: true,
    hasTouch: true,
  });
}

/// 表の一番上の行（自分の申請）を、bob が表のセルで直して、状態のセルから申請中へ出し直す。
async function fixAndResubmit(p: Page, tableId: string, recId: string, amount: string, shot: string) {
  const grid = p.getByTestId("data-grid");
  await expect(grid).toHaveAttribute("data-own-edit", "1", { timeout: 15_000 });
  const box = (await grid.boundingBox())!;
  const point = (col: number, row: number) => ({
    x: box.x + ROW_MARKER + col * COL_WIDTH + COL_WIDTH / 2,
    y: box.y + HEADER + row * ROW + ROW / 2,
  });
  // 金額のセルを開いて直す。
  const cell = point(1, 0);
  await p.mouse.click(cell.x, cell.y);
  await p.keyboard.press("Enter");
  const editor = p.locator("#portal textarea");
  await expect(editor).toBeVisible();
  await editor.fill(amount);
  const patched = p.waitForResponse((r) => r.url().includes(`/records/${recId}`) && r.request().method() === "PATCH");
  await p.keyboard.press("Enter");
  expect((await patched).status()).toBe(200);
  // 状態のセル（金額から 2 つ右）を開くと、進め先は「申請中」だけ。
  await p.mouse.click(cell.x, cell.y);
  await p.keyboard.press("ArrowRight");
  await p.keyboard.press("ArrowRight");
  await p.keyboard.press("Enter");
  const menu = p.getByTestId("transition-editor");
  await expect(menu.getByRole("option")).toHaveText(["申請中"], { timeout: 15_000 });
  if (SHOTS) await p.screenshot({ path: `${SHOTS}/${shot}` });
  const moved = p.waitForResponse((r) => r.url().endsWith(`/records/${recId}/transition`) && r.request().method() === "POST");
  await menu.getByRole("option", { name: "申請中" }).click();
  expect((await moved).status()).toBe(200);
}

/// 表の一番上の行の金額のセルを開こうとしても、編集の欄が出ない（直せない状態の自分の申請）。
async function expectCellLocked(p: Page) {
  const box = (await p.getByTestId("data-grid").boundingBox())!;
  const x = box.x + ROW_MARKER + COL_WIDTH + COL_WIDTH / 2;
  const y = box.y + HEADER + ROW / 2;
  await p.mouse.click(x, y);
  await p.keyboard.press("Enter");
  // 開くなら Enter のすぐ後に出る。少し待ってから、出ていないことを確かめる。
  await p.waitForTimeout(500);
  await expect(p.locator("#portal textarea")).toHaveCount(0);
  await p.keyboard.press("Escape");
}

test("「追加だけ」の申請者が、差し戻された自分の申請を直して出し直せる（D-127）", async ({ page, browser }) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  // bob を先にログインさせる（ディレクトリに載せる）。
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  await loginViaKeycloak(page); // alice
  const alice = await me(page);
  const bobMe = await me(bob);
  let tableId = "";
  let fsmId: string | null = null;
  let phone: BrowserContext | null = null;
  try {
    tableId = await createTable(page, uniqueName("e2e-出し直し"));

    // alice: 状態の進め方。作成者の列を足し、ひな形を使う。
    await page.goto(`/data/${tableId}?panel=states`);
    const editor = page.getByTestId("state-flow-editor");
    await expect(editor.getByTestId("state-flow-state")).toHaveCount(1, { timeout: 15_000 });
    const addOnlyBox = editor.getByTestId("state-flow-add-only");
    await expect(addOnlyBox.getByTestId("state-flow-add-only-now")).toHaveText(
      "いまの進め方では、追加だけの人は書き換えも状態を進めることもできません。",
    );
    const added = page.waitForResponse((r) => r.url().endsWith(`/data/tables/${tableId}/schema`) && r.request().method() === "PUT");
    await editor.getByTestId("state-flow-add-creator").click();
    expect((await added).status()).toBe(200);
    await expect(editor.getByRole("combobox", { name: "申請者の列" })).toContainText("作成者（作成者の列）");
    const template = editor.getByTestId("state-flow-template");
    await expect(template).toContainText("承認する人が判断する進め方、追加した人が直して出し直す進め方が入ります。");
    // ひな形を使うと、いまの状態の並びが替わることを言う（申請者の列はもう作成者の列なので、替わるとは言わない）。
    await expect(template).toContainText("いまの状態の並びは、ひな形に合わせて替わります。");
    if (SHOTS) {
      await template.scrollIntoViewIfNeeded();
      await page.screenshot({ path: `${SHOTS}/resubmit-template-pc.png` });
      const tp = await (await phoneOf(browser, page.context())).newPage();
      try {
        await tp.goto(`/data/${tableId}?panel=states`);
        const t = tp.getByTestId("state-flow-template");
        await expect(t).toBeVisible({ timeout: 15_000 });
        await t.scrollIntoViewIfNeeded();
        const pe = tp.getByTestId("state-flow-editor");
        expect(await pe.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
        await tp.screenshot({ path: `${SHOTS}/resubmit-template-phone.png` });
      } finally {
        await tp.context().close();
      }
    }
    await editor.getByTestId("state-flow-use-template").click();
    await expect(editor.getByTestId("state-flow-state")).toHaveCount(4);
    await expect(template).toHaveCount(0);
    const moves = editor.getByTestId("state-flow-move");
    await expect(moves).toHaveCount(4);
    for (const i of [0, 1, 2]) {
      await expect(moves.nth(i)).toContainText("「承認者」の人");
      await expect(moves.nth(i).getByRole("switch", { name: `進め方 ${i + 1}で申請した本人は除く` })).toBeChecked();
    }
    const resubmit = moves.nth(3);
    await expect(resubmit).toContainText("レコードを追加した人");
    await expect(resubmit.getByRole("switch", { name: "進め方 4で申請した本人は除く" })).not.toBeChecked();
    await expect(resubmit.getByTestId("state-flow-add-only-move")).toHaveText(
      "「追加だけ」で共有した人も、自分が追加したレコードなら進められます。",
    );
    // 本人が進めるための進め方なので、「申請した本人も進められます」の注意は出さない。
    await expect(resubmit.getByTestId("state-flow-self-allowed")).toHaveCount(0);
    const addOnlyNow = addOnlyBox.getByTestId("state-flow-add-only-now");
    await expect(addOnlyNow).toHaveText(
      "いまの進め方では、自分が追加したレコードが「差し戻し」のとき、追加だけの人も、そのレコードを進められます。「差し戻し」のあいだは直せます。",
    );
    await expect(editor.getByTestId("state-flow-write-lock")).toBeChecked();
    const lockHint = "テーブルを編集できる人は、自分が追加したレコードを、「申請中」と「差し戻し」のあいだだけ書き換えたり削除したりできます。";
    await expect(editor.getByTestId("state-flow-applicant")).toContainText(lockHint);
    // 取り下げのように、審査しない状態へ進めるだけの進め方を作成者に許しても、そのあいだは直せない（承認 → 却下 を
    // 「レコードを追加した人」に足すと、承認のときも進められるが、直せるのは差し戻しのあいだだけ。外すと戻る）。
    await editor.getByTestId("state-flow-add-move").click();
    await expect(moves).toHaveCount(5);
    const extra = moves.nth(4);
    await extra.getByRole("combobox", { name: "進め方 5のいまの状態" }).click();
    await page.getByRole("option", { name: "承認", exact: true }).click();
    await extra.getByRole("combobox", { name: "進め方 5の次の状態" }).click();
    await page.getByRole("option", { name: "却下", exact: true }).click();
    await extra.getByRole("button", { name: "進め方 5の進められる人を追加" }).click();
    await page.getByRole("menuitem", { name: "レコードを追加した人" }).click();
    await extra.getByRole("button", { name: /承認者.*を外す/ }).click();
    await expect(extra.getByRole("list", { name: "進め方 5の進められる人" })).toHaveText("レコードを追加した人");
    await expect(addOnlyNow).toHaveText(
      "いまの進め方では、自分が追加したレコードが「承認」と「差し戻し」のとき、追加だけの人も、そのレコードを進められます。「差し戻し」のあいだは直せます。",
    );
    await expect(editor.getByTestId("state-flow-applicant")).toContainText(lockHint);
    await extra.getByRole("button", { name: /進め方 5（「承認」から「却下」）を削除/ }).first().click();
    await expect(moves).toHaveCount(4);
    await expect(addOnlyNow).toHaveText(
      "いまの進め方では、自分が追加したレコードが「差し戻し」のとき、追加だけの人も、そのレコードを進められます。「差し戻し」のあいだは直せます。",
    );
    await addOnlyBox.getByRole("button", { name: "詳しく" }).click();
    const addOnlyMore = addOnlyBox.getByTestId("state-flow-add-only-more");
    await expect(addOnlyMore).toContainText(
      "最初の状態と、出し直した先の審査中の状態（例: 申請中）のあいだは直せません。",
    );
    await expect(addOnlyMore).toContainText(
      "削除と取り込みはできません。ほかの人のレコードは、個別に共有されたものだけ見られます。直すことはできません。",
    );
    await addOnlyBox.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/resubmit-flow-pc.png` });
    await resubmit.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/resubmit-moves-pc.png` });
    await editor.getByTestId("state-flow-save").click();
    await expect(editor).toBeHidden({ timeout: 15_000 });
    fsmId = await fsmIdOf(page, tableId);

    // 裏取り: 差し戻し → 申請中は作成者の列で本人を指し、書き込みの決まりは差し戻しのあいだ作成者に書き換えを許す。
    const saved = (await (await page.request.get(`/api/data/tables/${tableId}/fsm`)).json()) as {
      fsm: { body: { states: string[]; transitions: { from: string; to: string; actor: unknown }[] } };
      applicant_write_lock: string | null;
    };
    expect(saved.applicant_write_lock).toBe("created_by");
    expect(saved.fsm.body.states).toEqual(["申請中", "承認", "却下", "差し戻し"]);
    expect(saved.fsm.body.transitions.at(-1)).toEqual({
      from: "差し戻し",
      to: "申請中",
      actor: { field_cmp: { field: "created_by", op: "eq", value: "user_id" } },
    });
    const table = (await (await page.request.get(`/api/data/tables/${tableId}`)).json()) as {
      schema: { row_policy: { write: unknown } };
    };
    expect(table.schema.row_policy.write).toEqual({
      all: [
        "public",
        {
          any: [
            { field_cmp: { field: "status", op: "eq", value: { lit: "申請中" } } },
            { field_cmp: { field: "created_by", op: "ne", value: "user_id" } },
            {
              all: [
                { field_cmp: { field: "status", op: "in", value: { lit: ["差し戻し"] } } },
                { field_cmp: { field: "created_by", op: "eq", value: "user_id" } },
              ],
            },
          ],
        },
      ],
    });

    // bob を「追加だけ」にする。bob が申請し、alice が差し戻す。
    const share = await page.request.put(`/api/data/tables/${tableId}/shares`, {
      headers: await csrfHeaders(page),
      data: { target: { type: "user", id: bobMe.id }, role: "submitter" },
    });
    expect(share.status(), await share.text()).toBe(204);
    const req1 = await addRecord(bob, tableId, { title: "bob の出張", amount: 1000, approver: alice.id });
    // 申請中のあいだは、bob は直せない（API でも 403）。
    const early = await bob.request.patch(`/api/data/tables/${tableId}/records/${req1.id}`, {
      headers: await csrfHeaders(bob),
      data: { patch: { amount: 1 }, expected_rev: req1.rev },
    });
    expect(early.status()).toBe(403);
    await move(page, tableId, req1.id, "差し戻し");

    // bob（PC）: 案内と、自分の差し戻しの申請を表のセルで直して出し直す。
    await bob.goto(`/data/${tableId}`);
    await expect(bob.getByTestId("data-grid")).toHaveAttribute("data-loaded", "1", { timeout: 15_000 });
    await expect(bob.getByTestId("data-add-only-note")).toContainText(
      "自分が追加したレコードは、「差し戻し」のあいだ直せ、「状態」のセルを開いて進められます。",
    );
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/resubmit-bob-sent-back-pc.png` });
    await fixAndResubmit(bob, tableId, req1.id, "1200", "resubmit-bob-menu-pc.png");
    let now = await recordOf(page, tableId, req1.id);
    expect(now.data).toMatchObject({ amount: 1200, status: "申請中", created_by: bobMe.id });
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/resubmit-bob-done-pc.png` });
    // 出し直した後は直せない（セルも開かない）。alice が承認する。own_records は表の単位なので "1" のまま。セルは、
    // 行の状態を直せる状態と照らして開かない。
    await expect(bob.getByTestId("data-grid")).toHaveAttribute("data-own-edit", "1");
    await expectCellLocked(bob);
    const late = await bob.request.patch(`/api/data/tables/${tableId}/records/${req1.id}`, {
      headers: await csrfHeaders(bob),
      data: { patch: { amount: 1 }, expected_rev: now.rev },
    });
    expect(late.status()).toBe(403);
    await move(page, tableId, req1.id, "承認");
    now = await recordOf(page, tableId, req1.id);
    expect(now.data).toMatchObject({ amount: 1200, status: "承認" });

    // スマホ: もう 1 件を申請 → 差し戻し → bob がスマホで直して出し直す → alice が承認。
    const req2 = await addRecord(bob, tableId, { title: "bob の研修", amount: 3000, approver: alice.id });
    await move(page, tableId, req2.id, "差し戻し");
    phone = await phoneOf(browser, bobCtx);
    const p = await phone.newPage();
    await p.goto(`/data/${tableId}`);
    await expect(p.getByTestId("data-grid")).toHaveAttribute("data-loaded", "2", { timeout: 15_000 });
    await expect(p.getByTestId("data-add-only-note")).toContainText("「差し戻し」のあいだ直せ、「状態」のセルを開いて進められます。");
    expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/resubmit-bob-sent-back-phone.png` });
    await fixAndResubmit(p, tableId, req2.id, "2800", "resubmit-bob-menu-phone.png");
    await move(page, tableId, req2.id, "承認");
    const done2 = await recordOf(page, tableId, req2.id);
    expect(done2.data).toMatchObject({ amount: 2800, status: "承認" });
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/resubmit-bob-done-phone.png` });

    // alice のスマホ幅の状態の進め方（ひな形で作った進め方のカード）。
    const ap = await (await phoneOf(browser, page.context())).newPage();
    try {
      await ap.goto(`/data/${tableId}?panel=states`);
      const pe = ap.getByTestId("state-flow-editor");
      await expect(pe.getByTestId("state-flow-move")).toHaveCount(4, { timeout: 15_000 });
      await pe.getByTestId("state-flow-move").nth(3).scrollIntoViewIfNeeded();
      expect(await pe.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await ap.screenshot({ path: `${SHOTS}/resubmit-moves-phone.png` });
    } finally {
      await ap.context().close();
    }
  } finally {
    await cleanupAll(
      async () => {
        fsmId ??= tableId ? await fsmIdOf(page, tableId) : null;
      },
      () => (tableId ? deleteTableApi(page, tableId) : Promise.resolve()),
      () => (fsmId ? deleteArtifactApi(page, fsmId) : Promise.resolve()),
      () => (phone ? phone.close() : Promise.resolve()),
      () => bobCtx.close(),
    );
  }
});
