import { expect, test, type Page } from "@playwright/test";

import { cleanupQueue, deleteArtifactApi, loginAs, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 共有の操作は持ち主にだけ出す（BUG-120・BUG-78 の残り）。共有（相手の一覧と付与・解除）は持ち主だけができるので、
/// 編集の共有を受けた人には、スキルの一覧のメニューの「共有」「削除」と、ワークフローのエディターの上のバーの
/// 「共有」を出さない（出すと、相手の一覧が空に見え、付与が断られる）。
/// 共有の欄が共有している相手を 403 以外で読み込めなかったときは、空の一覧（まだ誰にも共有していません）に見せず、
/// 読み込めなかったことと「もう一度読み込む」を出す。
/// 作ったスキル・ワークフローは最後に消す。`E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function api(page: Page, method: "POST" | "PUT", path: string, body: unknown) {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.fetch(`/api${path}`, {
    method,
    headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
    data: body,
  });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return res;
}

/// alice のスキルとワークフローを作り、bob に編集で共有する。
async function setup(page: Page, bobId: string): Promise<{ skill: string; skillId: string; flow: string; flowId: string }> {
  const skill = uniqueName("e2e-共有のスキル");
  const s = await api(page, "POST", "/skills", {
    name: skill,
    body: {
      description: "共有の操作を確かめるスキル",
      instructions: "あなたは E2E テストのアシスタントです。",
      knowledge_scope: null,
      allowed_tools: null,
      model: null,
    },
  });
  const skillId = ((await s.json()) as { id: string }).id;
  cleanup.add(() => deleteArtifactApi(page, skillId));
  // name は英小文字の名前（保存時に検証する）。画面に出る名前は display_name。
  const flow = uniqueName("e2e-共有のワークフロー");
  const w = await api(page, "POST", "/workflows", {
    ir: {
      ir_version: 1,
      name: uniqueName("e2e-share-flow"),
      display_name: flow,
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
  });
  const flowId = ((await w.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, flowId));
  for (const id of [skillId, flowId]) {
    await api(page, "PUT", `/artifacts/${id}/shares`, { target: { type: "user", id: bobId }, role: "editor" });
  }
  return { skill, skillId, flow, flowId };
}

test("共有の操作は持ち主にだけ出し、共有の相手を読み込めなければ空の一覧に見せない（BUG-120）", async ({
  page,
  browser,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  cleanup.add(() => bobCtx.close());
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  const bobId = ((await (await bob.request.get("/api/me")).json()) as { id: string }).id;
  const s = await setup(page, bobId);

  // bob（編集の共有を受けた人）: スキルの一覧のメニューに「編集」はあり、「共有」「削除」は無い。
  await bob.goto("/skills");
  await expect(bob.getByRole("heading", { name: s.skill })).toBeVisible({ timeout: 15_000 });
  await bob.getByRole("button", { name: `${s.skill} のその他の操作` }).click();
  await expect(bob.getByRole("menuitem", { name: "編集" })).toBeVisible();
  await expect(bob.getByRole("menuitem", { name: "バージョン履歴" })).toBeVisible();
  await expect(bob.getByRole("menuitem", { name: "共有" })).toHaveCount(0);
  await expect(bob.getByRole("menuitem", { name: "削除" })).toHaveCount(0);
  if (SHOTS) await bob.screenshot({ path: `${SHOTS}/bug120-skills-menu-editor-pc.png` });
  await bob.keyboard.press("Escape");

  // bob: ワークフローのエディターの上のバーに「共有」は無い（ほかの操作は出る）。
  await bob.goto(`/workflows/${s.flowId}`);
  await expect(bob.getByRole("button", { name: "バージョン履歴" })).toBeVisible({ timeout: 15_000 });
  await expect(bob.getByTestId("workflow-share")).toHaveCount(0);
  await expect(bob.getByRole("button", { name: "共有", exact: true })).toHaveCount(0);
  if (SHOTS) await bob.screenshot({ path: `${SHOTS}/bug120-workflow-bar-editor-pc.png` });

  // alice（持ち主）: エディターの上のバーに「共有」が出る。
  await page.goto(`/workflows/${s.flowId}`);
  await expect(page.getByTestId("workflow-share")).toBeVisible({ timeout: 15_000 });

  // alice: スキルの共有を開いたとき、共有している相手の読み込みが 403 以外で失敗したら、空の一覧に見せない。
  let fail = true;
  await page.route(`**/api/artifacts/${s.skillId}/shares`, (route) =>
    fail && route.request().method() === "GET"
      ? route.fulfill({ status: 500, json: { message: "boom" } })
      : route.continue(),
  );
  await page.goto("/skills");
  await expect(page.getByRole("heading", { name: s.skill })).toBeVisible({ timeout: 15_000 });
  await page.getByRole("button", { name: `${s.skill} のその他の操作` }).click();
  await expect(page.getByRole("menuitem", { name: "削除" })).toBeVisible();
  await page.getByRole("menuitem", { name: "共有" }).click();
  const dialog = page.getByRole("dialog");
  const failed = dialog.getByTestId("share-load-failed");
  await expect(failed).toBeVisible({ timeout: 15_000 });
  await expect(failed).toContainText("共有している相手を読み込めませんでした");
  await expect(dialog.getByText("まだ誰にも共有していません。")).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/bug120-share-load-failed-pc.png` });
  // もう一度読み込むと、共有している相手（bob）が出る。
  fail = false;
  await failed.getByRole("button", { name: "もう一度読み込む" }).click();
  await expect(dialog.getByTestId("share-entry")).toHaveCount(1, { timeout: 15_000 });
  await expect(failed).toHaveCount(0);
  await page.unroute(`**/api/artifacts/${s.skillId}/shares`);
});

test("スマホ: 共有の相手を読み込めなかったことは、シートの中でも横にはみ出さずに出る（BUG-120）", async ({
  page,
  browser,
}) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const bobId = await (async () => {
    const ctx = await browser.newContext();
    try {
      const p = await ctx.newPage();
      await loginAs(p, "bob");
      return ((await (await p.request.get("/api/me")).json()) as { id: string }).id;
    } finally {
      await ctx.close();
    }
  })();
  const s = await setup(page, bobId);
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
  await phone.route(`**/api/artifacts/${s.skillId}/shares`, (route) =>
    route.request().method() === "GET" ? route.fulfill({ status: 500, json: {} }) : route.continue(),
  );
  await phone.goto("/skills");
  await expect(phone.getByRole("heading", { name: s.skill })).toBeVisible({ timeout: 15_000 });
  await phone.getByRole("button", { name: `${s.skill} のその他の操作` }).click();
  await phone.getByRole("menuitem", { name: "共有" }).click();
  const sheet = phone.getByRole("dialog");
  await expect(sheet.getByTestId("share-load-failed")).toBeVisible({ timeout: 15_000 });
  expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  if (SHOTS) await phone.screenshot({ path: `${SHOTS}/bug120-share-load-failed-phone.png` });
});
