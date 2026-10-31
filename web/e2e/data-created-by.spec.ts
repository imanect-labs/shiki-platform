import { expect, test, type Browser, type BrowserContext, type Page } from "@playwright/test";

import { csrfHeaders } from "./approval-fixture";
import { cleanupAll, deleteArtifactApi, deleteTableApi, grantTableEditor, loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// 作成者の列（D-122・BUG-148）:
/// - 状態の進め方の画面で、メンバーの列（申請者）で判定していると迂回できることを言い、ボタン 1 つで作成者の列を
///   足して申請者の列にできる。「詳しく」で代わりに申請する業務の扱いを言う。
/// - 作成者の列で「申請した本人は除く」を付けると、承認者を兼ねる編集者（bob）が申請者の列をほかの人にして
///   作った申請でも、bob は自分では承認できない（表の状態のセルにも進める先が出ない）。ほかの人の申請は承認できる。
/// - 作成者の列は書けない（レコードの追加のダイアログに出ない・API で書くと 400）。定義を読めない編集者にも、
///   追加のダイアログで最初の状態の名前が出る（BUG-154）。
///
/// `E2E_SHOTS_DIR` があれば、PC（1440px）とスマホ（390px）の画面を撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

/// グリッド（glide-data-grid の canvas）の座標（行の印の幅・列の既定の幅・見出しと行の高さ）。
const ROW_MARKER = 32;
const COL_WIDTH = 160;
const HEADER = 36;
const ROW = 34;

type Me = { id: string; tenant_id: string };

async function me(page: Page): Promise<Me> {
  return (await (await page.request.get("/api/me")).json()) as Me;
}

/// 申請のテーブル（作成者の列はまだ無い・申請者はメンバーの列）。
async function createTable(page: Page, name: string): Promise<string> {
  const res = await page.request.post("/api/data/tables", {
    headers: await csrfHeaders(page),
    data: {
      name,
      schema: {
        fields: [
          { key: "title", display_name: "件名", type: "text", indexed: true },
          { key: "applicant", display_name: "申請者", type: "user_ref" },
          { key: "approver", display_name: "承認者", type: "user_ref", indexed: true },
          { key: "status", display_name: "状態", type: "select", options: ["申請中", "承認", "却下"], indexed: true },
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

async function addRecord(page: Page, tableId: string, data: Record<string, unknown>): Promise<{ id: string; rev: number; data: Record<string, unknown> }> {
  const res = await page.request.post(`/api/data/tables/${tableId}/records`, { headers: await csrfHeaders(page), data: { data } });
  expect(res.status(), await res.text()).toBe(201);
  return (await res.json()) as { id: string; rev: number; data: Record<string, unknown> };
}

async function recordOf(page: Page, tableId: string, id: string): Promise<{ rev: number; data: Record<string, unknown> }> {
  const res = await page.request.get(`/api/data/tables/${tableId}/records/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()) as { rev: number; data: Record<string, unknown> };
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

test("作成者の列を画面で足して状態の進め方を作ると、申請者の列を書き換えても自分で承認できない（D-122・BUG-148）", async ({
  page,
  browser,
}) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  // bob（承認者を兼ねる編集者）を先にログインさせる（ディレクトリに載せる＝メンバーの列に入れられる）。
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  await loginViaKeycloak(page);
  const alice = await me(page);
  const bobMe = await me(bob);
  let tableId = "";
  let fsmId: string | null = null;
  let revokeBob: (() => Promise<void>) | null = null;
  let phone: BrowserContext | null = null;
  try {
    tableId = await createTable(page, uniqueName("e2e-作成者の申請"));
    revokeBob = await grantTableEditor(page, alice.tenant_id, bobMe.id, tableId);

    // 画面: 状態の進め方。申請者の列はメンバーの列が見当で入り、迂回できることと、作成者の列に替えるボタンが出る。
    await page.goto(`/data/${tableId}?panel=states`);
    const editor = page.getByTestId("state-flow-editor");
    await expect(editor.getByTestId("state-flow-state")).toHaveCount(3, { timeout: 15_000 });
    const section = editor.getByTestId("state-flow-applicant");
    await expect(editor.getByRole("combobox", { name: "申請者の列" })).toContainText("申請者（メンバーの列）");
    await expect(section.getByTestId("state-flow-member-bypass")).toContainText(
      "申請者の列をほかの人にできます。そうすると、自分の申請でも承認できます。作成者の列に替えると防げます。",
    );
    await section.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/created-by-member-pc.png` });

    // ボタン 1 つで作成者の列を足す（列はすぐ保存され、申請者の列が作成者の列になる）。
    const added = page.waitForResponse((r) => r.url().endsWith(`/data/tables/${tableId}/schema`) && r.request().method() === "PUT");
    await section.getByTestId("state-flow-add-creator").click();
    expect((await added).status()).toBe(200);
    await expect(page.getByText("「作成者」の列を追加しました").first()).toBeVisible();
    await expect(editor.getByRole("combobox", { name: "申請者の列" })).toContainText("作成者（作成者の列）");
    await expect(section.getByTestId("state-flow-member-bypass")).toHaveCount(0);
    await expect(section.getByTestId("state-flow-applicant-creator")).toContainText(
      "「作成者」には、レコードを追加した人が自動で入ります。誰も書き換えられないので、レコードを追加した人を、承認する人から除けます。",
    );
    await section.getByRole("button", { name: "詳しく" }).click();
    await expect(section.getByTestId("state-flow-applicant-more")).toContainText(
      "代わりに申請する業務（レコードを追加する人と、申請する人が違う）では、除かれるのはレコードを追加した人です。",
    );
    await expect(section.getByTestId("state-flow-add-creator")).toHaveCount(0);
    await expect(editor.getByTestId("state-flow-write-lock")).toBeChecked();

    // 進め方: 申請中 → 承認・却下。進められる人は「承認者」の人、申請した本人は除く（既定）。
    await editor.getByTestId("state-flow-add-move").click();
    await editor.getByTestId("state-flow-add-move").click();
    const moves = editor.getByTestId("state-flow-move");
    await expect(moves).toHaveCount(2);
    for (const i of [0, 1]) {
      await expect(moves.nth(i)).toContainText("「承認者」の人");
      await expect(moves.nth(i).getByRole("switch", { name: `進め方 ${i + 1}で申請した本人は除く` })).toBeChecked();
    }
    await section.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/created-by-pc.png` });
    await editor.getByTestId("state-flow-save").click();
    await expect(editor).toBeHidden({ timeout: 15_000 });
    await expect(page.getByText("状態の進め方を保存しました").first()).toBeVisible();

    // 裏取り: 本人を除く比べと書き込みの決まりは作成者の列で入った。
    const saved = (await (await page.request.get(`/api/data/tables/${tableId}/fsm`)).json()) as {
      fsm: { body: { transitions: { from: string; to: string; actor: unknown }[] } };
      applicant_write_lock: string | null;
    };
    fsmId = await fsmIdOf(page, tableId);
    const creatorKey = saved.applicant_write_lock;
    expect(creatorKey).toBe("created_by");
    const guard = {
      all: [
        { field_cmp: { field: "approver", op: "eq", value: "user_id" } },
        { field_cmp: { field: "created_by", op: "ne", value: "user_id" } },
      ],
    };
    expect(saved.fsm.body.transitions).toEqual([
      { from: "申請中", to: "承認", actor: guard },
      { from: "申請中", to: "却下", actor: guard },
    ]);

    // alice の申請（承認者は bob）と、bob の申請（申請者の列を alice にし、承認者は自分）。bob の申請を最後に作る。
    const others = await addRecord(page, tableId, { title: "alice の出張", applicant: alice.id, approver: bobMe.id });
    const own = await addRecord(bob, tableId, { title: "bob の出張", applicant: alice.id, approver: bobMe.id });
    expect(own.data.created_by).toBe(bobMe.id);
    expect(others.data.created_by).toBe(alice.id);
    // 作成者の列は書けない（API でも 400）。
    const forged = await bob.request.post(`/api/data/tables/${tableId}/records`, {
      headers: await csrfHeaders(bob),
      data: { data: { title: "偽", created_by: alice.id } },
    });
    expect(forged.status()).toBe(400);
    expect(((await forged.json()) as { message: string }).message).toBe(
      "「作成者」には書き込めません。作成者の列には、レコードを追加した人が自動で入ります",
    );

    // bob の表: 自分の申請（いちばん上の行）の状態のセルには、進める先が出ない。
    await bob.goto(`/data/${tableId}`);
    const grid = bob.getByTestId("data-grid");
    await expect(grid).toHaveAttribute("data-loaded", "2", { timeout: 15_000 });
    const box = (await grid.boundingBox())!;
    const cell = (col: number, row: number) => ({
      x: box.x + ROW_MARKER + col * COL_WIDTH + COL_WIDTH / 2,
      y: box.y + HEADER + row * ROW + ROW / 2,
    });
    const status = cell(3, 0);
    await bob.mouse.click(status.x, status.y);
    await bob.keyboard.press("Enter");
    const menu = bob.getByTestId("transition-editor");
    await expect(menu).toContainText("この状態から、あなたが進められる先はありません。", { timeout: 15_000 });
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/created-by-own-blocked-pc.png` });
    await bob.keyboard.press("Escape");
    // 画面を通さず進めても断られる。申請者の列を書き換え直しても同じ（作成者は変わらない）。
    const forced = await bob.request.post(`/api/data/tables/${tableId}/records/${own.id}/transition`, {
      headers: await csrfHeaders(bob),
      data: { to: "承認", expected_rev: own.rev },
    });
    expect(forced.status()).toBe(403);
    const rewritten = await bob.request.patch(`/api/data/tables/${tableId}/records/${own.id}`, {
      headers: await csrfHeaders(bob),
      data: { patch: { applicant: bobMe.id }, expected_rev: own.rev },
    });
    expect(rewritten.status(), await rewritten.text()).toBe(200);
    const again = await bob.request.post(`/api/data/tables/${tableId}/records/${own.id}/transition`, {
      headers: await csrfHeaders(bob),
      data: { to: "承認", expected_rev: (await recordOf(bob, tableId, own.id)).rev },
    });
    expect(again.status()).toBe(403);
    expect((await recordOf(page, tableId, own.id)).data.status).toBe("申請中");
    // ほかの人（alice）の申請は、bob が承認できる。
    const approved = await bob.request.post(`/api/data/tables/${tableId}/records/${others.id}/transition`, {
      headers: await csrfHeaders(bob),
      data: { to: "承認", expected_rev: others.rev },
    });
    expect(approved.status(), await approved.text()).toBe(200);

    // レコードの追加のダイアログ（bob は状態の進め方の定義を読めない編集者）: 作成者の列は出ず、最初の状態の名前が出る。
    await bob.getByTestId("add-record").click();
    const dialog = bob.getByTestId("record-dialog");
    await expect(dialog.getByTestId("record-dialog-initial-state")).toHaveText(
      "「状態」は最初の状態（申請中）で作られます。作った後に表のセルから進められます。",
    );
    await expect(dialog.getByText("件名", { exact: true })).toBeVisible();
    await expect(dialog.getByText("作成者", { exact: true })).toHaveCount(0);
    if (SHOTS) await bob.screenshot({ path: `${SHOTS}/created-by-record-dialog-pc.png` });
    await bob.keyboard.press("Escape");

    // 列の編集（alice・持ち主）: 作成者の列の設定は説明だけ（必須・重複なし・索引は切り替えない）。新しい列の型にも出る。
    await page.goto(`/data/${tableId}`);
    await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "2", { timeout: 15_000 });
    await page.getByTestId("schema-editor-open").click();
    const schemaEditor = page.getByTestId("schema-editor");
    await schemaEditor.getByRole("button", { name: "「作成者」の設定" }).click();
    await expect(schemaEditor.getByTestId("schema-created-by-hint")).toHaveText(
      "レコードを追加した人が自動で入り、誰も書き換えられません。この列を足す前に追加したレコードにも、追加した人が入ります。",
    );
    await expect(schemaEditor.getByRole("switch", { name: "「作成者」の索引" })).toBeDisabled();
    await expect(schemaEditor.getByRole("switch", { name: "「作成者」の索引" })).toBeChecked();
    await expect(schemaEditor.getByText("必須", { exact: true })).toHaveCount(0);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/created-by-schema-pc.png` });
    await page.getByTestId("schema-add-field").click();
    await page.getByLabel("新しい列の型").click();
    await expect(page.getByRole("option", { name: "作成者", exact: true })).toBeVisible();
    await page.keyboard.press("Escape");
    await page.getByTestId("schema-add-cancel").click();
    await page.keyboard.press("Escape");
    await expect(schemaEditor).toBeHidden();

    // スマホ: 状態の進め方の「申請した本人の扱い」と表が横にはみ出さない。
    phone = await phoneOf(browser, page.context());
    const p = await phone.newPage();
    await p.goto(`/data/${tableId}?panel=states`);
    const phoneEditor = p.getByTestId("state-flow-editor");
    await expect(phoneEditor.getByTestId("state-flow-state")).toHaveCount(3, { timeout: 15_000 });
    await expect(phoneEditor.getByRole("combobox", { name: "申請者の列" })).toContainText("作成者（作成者の列）");
    await phoneEditor.getByTestId("state-flow-applicant").scrollIntoViewIfNeeded();
    expect(await phoneEditor.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/created-by-applicant-phone.png` });
    await p.keyboard.press("Escape");
    await p.goto(`/data/${tableId}`);
    await expect(p.getByTestId("data-grid")).toHaveAttribute("data-loaded", "2", { timeout: 15_000 });
    expect(await p.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    if (SHOTS) await p.screenshot({ path: `${SHOTS}/created-by-grid-phone.png` });
  } finally {
    await cleanupAll(
      () => (revokeBob ? revokeBob() : Promise.resolve()),
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

/// 状態の進め方の「作成者の列を追加」は、押した時点のテーブルの定義に列を足す。画面のテーブルが古くても（購読が
/// 届いていない）、ほかで変えた列の設定を元に戻さない（送る中身と版の番号を同じ時点のものにする）。作成者の列の
/// あるテーブルでは、同じ値の「作成者」（レコードの作成者の擬似列）を列の候補に出さない（同じ名前を 2 つ並べない）。
test("作成者の列を足しても、ほかで変えた列の設定を元に戻さない。列の候補の「作成者」は 1 つ（D-122）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  let tableId = "";
  try {
    tableId = await createTable(page, uniqueName("e2e-作成者の列の追加"));
    // 購読を止めて、ほかでの変更が画面に届かない（画面のテーブルが古いままの）状態にする。
    await page.route(`**/api/data/tables/${tableId}/stream**`, (r) => r.abort());
    await page.goto(`/data/${tableId}`);
    await expect(page.getByTestId("data-grid")).toHaveAttribute("data-loaded", "0", { timeout: 15_000 });

    // ほか（別のタブ・チャットの AI）で、列の表示名を変える。
    type Field = { key: string; display_name?: string; type: string };
    const before = (await (await page.request.get(`/api/data/tables/${tableId}`)).json()) as {
      schema: { fields: Field[] } & Record<string, unknown>;
      schema_version: number;
    };
    const renamed = {
      ...before.schema,
      fields: before.schema.fields.map((f) => (f.key === "title" ? { ...f, display_name: "件名（変更）" } : f)),
    };
    const put = await page.request.put(`/api/data/tables/${tableId}/schema`, {
      headers: await csrfHeaders(page),
      data: { schema: renamed, expected_schema_version: before.schema_version },
    });
    expect(put.status(), await put.text()).toBe(200);

    // 状態の進め方を開いて、作成者の列を足す。
    await page.getByTestId("state-flow-open").click();
    const editor = page.getByTestId("state-flow-editor");
    await expect(editor.getByTestId("state-flow-state")).toHaveCount(3, { timeout: 15_000 });
    const added = page.waitForResponse((r) => r.url().endsWith(`/data/tables/${tableId}/schema`) && r.request().method() === "PUT");
    await editor.getByTestId("state-flow-applicant").getByTestId("state-flow-add-creator").click();
    expect((await added).status()).toBe(200);
    await expect(page.getByText("「作成者」の列を追加しました").first()).toBeVisible();
    await expect(editor.getByTestId("state-flow-add-creator-error")).toHaveCount(0);
    // 裏取り: ほかで変えた表示名は残り、作成者の列が足された。
    const after = (await (await page.request.get(`/api/data/tables/${tableId}`)).json()) as { schema: { fields: Field[] } };
    expect(after.schema.fields.find((f) => f.key === "title")?.display_name).toBe("件名（変更）");
    expect(after.schema.fields.filter((f) => f.type === "created_by")).toHaveLength(1);

    // 閉じる（申請者の列の変更は保存しない）。Escape は先に出ている知らせを閉じるので、下のボタンで閉じる。
    await editor.getByRole("button", { name: "閉じる", exact: true }).filter({ hasText: "閉じる" }).click();
    await page.getByRole("button", { name: "変更を捨てて閉じる" }).click();
    await expect(editor).toBeHidden();

    // 列の候補: 「作成者」は作成者の列の 1 つだけ（擬似列の「作成者」を並べない）。
    await page.getByTestId("data-columns-button").click();
    await expect(page.getByRole("textbox", { name: "「件名（変更）」の見出し" })).toBeVisible();
    await expect(page.getByRole("textbox", { name: "「作成者」の見出し" })).toHaveCount(1);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/created-by-columns-pc.png` });
    await page.keyboard.press("Escape");
  } finally {
    await cleanupAll(() => (tableId ? deleteTableApi(page, tableId) : Promise.resolve()));
  }
});
