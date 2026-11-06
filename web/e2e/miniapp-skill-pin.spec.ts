import { test, expect, type Page } from "@playwright/test";

import {
  cleanupQueue,
  createFolderApi,
  deleteArtifactApi,
  loginViaKeycloak,
  trackCreatedResources,
  trashFolderApi,
  uniqueName,
} from "./helpers";

/// アプリのエディターで、束ねたスキルの版を上げる（GAP-86）の E2E。
/// - 選択欄は、固定しているバージョン（v1）を見せる（最新の v2 の番号で見せない）。「なし」にしてから選び直しても、
///   保存してあるバージョンに据え置く。
/// - 新しいバージョンがあれば、スキルの何が変わるか（AI への指示・参照するフォルダーとファイル）と、使う人に要る権限の
///   増減（参照するフォルダーを見る権限）を出し、「v2 にする」で上げられる。保存の前なら元に戻せる。
/// - 上げて保存すると、下書きのスキルは v2 になる。
/// - 束ねたスキルが読めるかは、新しいバージョンの確かめの答えで決め、版の本文を別に読まない（GAP-98）。
/// PC（1440px）とスマホ（390px）で確かめる。作ったもの（アプリ・スキル・フォルダー）は最後に消す。
/// `E2E_SHOTS_DIR` を渡すとスクリーンショットを撮る。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.beforeEach(({ page }) => trackCreatedResources(page, cleanup));
test.afterEach(() => cleanup.run());

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

async function api<T = Record<string, unknown>>(page: Page, method: "GET" | "POST" | "PUT", path: string, body?: unknown) {
  const res = await page.request.fetch(`/api${path}`, {
    method,
    headers: { "Content-Type": "application/json", "X-CSRF-Token": await csrf(page) },
    ...(body === undefined ? {} : { data: body }),
  });
  expect(res.ok(), `${method} ${path} → ${res.status()} ${await res.text()}`).toBeTruthy();
  return (await res.json()) as T;
}

type Fixture = { appId: string; skillId: string; skill: string; folder: string };

/// スキル（v1）を束ねたアプリを作り、その後でスキルを v2 に改訂する（AI への指示と参照するフォルダーを変える）。
async function setup(page: Page): Promise<Fixture> {
  const folder = uniqueName("e2e-経費の規程");
  const folderId = await createFolderApi(page, folder);
  cleanup.add(() => trashFolderApi(page, folderId));
  const skill = uniqueName("e2e-経費の手引き");
  const body = {
    description: "経費の規程に沿って答えるスキル",
    instructions: "経費の規程に沿って答えます。",
    knowledge_scope: null,
    allowed_tools: null,
    model: null,
  };
  const s = await api<{ id: string }>(page, "POST", "/skills", { name: skill, body });
  cleanup.add(() => deleteArtifactApi(page, s.id));
  const app = await api<{ id: string }>(page, "POST", "/mini-apps", {
    name: uniqueName("skill-pin-app"),
    body: {
      description: "経費の相談",
      screen: "経費について、AI に聞けます。\n",
      skill: { artifact_id: s.id, version: 1 },
      workflows: [],
    },
  });
  cleanup.add(() => deleteArtifactApi(page, app.id));
  await api(page, "PUT", `/skills/${s.id}`, {
    body: {
      ...body,
      instructions: "経費の規程に沿って、根拠の条文を添えて答えます。",
      knowledge_scope: { folders: [folderId], files: [] },
    },
    expected_version: 1,
  });
  return { appId: app.id, skillId: s.id, skill, folder };
}

async function openEditor(page: Page, appId: string) {
  await page.goto(`/apps/${appId}`);
  await page.getByRole("button", { name: "編集" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "アプリを編集" })).toBeVisible();
  return dialog;
}

test("束ねたスキルは固定した版を見せ、新しいバージョンの差を見て上げられる（GAP-86）", async ({ page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const f = await setup(page);
  // 束ねたスキルが読めるかは、新しいバージョンの確かめ（pin-updates）の答えで決め、版の本文を別に読まない（GAP-98）。
  // 数えるのはエディターを開いてから（アプリの画面が読むものは数えない）。
  await page.goto(`/apps/${f.appId}`);
  await expect(page.getByRole("button", { name: "編集" })).toBeVisible({ timeout: 15_000 });
  await page.waitForLoadState("networkidle");
  const asked: string[] = [];
  page.on("request", (req) => {
    const path = new URL(req.url()).pathname;
    if (path.startsWith(`/api/skills/${f.skillId}`) || path === "/api/mini-apps/pin-updates" || path === `/api/artifacts/${f.skillId}`) {
      asked.push(`${req.method()} ${path}`);
    }
  });
  await page.getByRole("button", { name: "編集" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "アプリを編集" })).toBeVisible();

  // 選択欄は固定している v1 を見せる（名前は、読めると分かってから引く）。
  const select = dialog.getByLabel("スキル（任意）");
  await expect(select.locator("option:checked")).toHaveText(`${f.skill}（v1）`, { timeout: 15_000 });
  // 版の本文も、名前も別に読まない（スキルの一覧にあるので、選択欄は一覧の名前で出る）。
  await page.waitForLoadState("networkidle");
  expect(asked).toEqual(["POST /api/mini-apps/pin-updates"]);
  const pin = dialog.getByTestId("miniapp-skill-pin");
  const update = pin.getByTestId("miniapp-pin-update");
  await expect(update).toContainText("新しいバージョン v2 があります。v2 にすると、次のように変わります。", {
    timeout: 15_000,
  });
  await expect(update.getByTestId("miniapp-pin-changes").getByRole("listitem")).toHaveText([
    "AI への指示が変わります",
    "参照するフォルダーとファイルが変わります",
    `使う人に、フォルダー「${f.folder}」の中を見る権限が要るようになります`,
  ]);
  await expect(update.getByTestId("miniapp-pin-access-note")).toHaveText(
    "使う人に足りるかは、公開するときの確認で、共有している相手ごとに確かめられます。",
  );
  await expect(update.getByRole("link", { name: "「スキル」の画面" })).toHaveAttribute("href", "/skills");
  await pin.scrollIntoViewIfNeeded();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-skill-pin-update-pc.png` });

  // 「なし」にしてから選び直しても、保存してある v1 に据え置く（勝手に最新へ上げない）。
  await select.selectOption("");
  await expect(pin).toHaveCount(0);
  await select.selectOption({ label: `${f.skill}（v1）` });
  await expect(select.locator("option:checked")).toHaveText(`${f.skill}（v1）`);
  await expect(pin.getByTestId("miniapp-pin-update")).toBeVisible();

  // v2 に上げると、選択欄も v2 を見せ、元に戻せる。戻してから、もう一度上げる。
  await update.getByRole("button", { name: "v2 にする" }).click();
  await expect(select.locator("option:checked")).toHaveText(`${f.skill}（v2）`);
  const changed = pin.getByTestId("miniapp-pin-changed");
  await expect(changed).toContainText("v1 から v2 にしました");
  await expect(pin.getByTestId("miniapp-pin-update")).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/miniapp-skill-pin-changed-pc.png` });
  await changed.getByRole("button", { name: "v1 に戻す" }).click();
  await expect(select.locator("option:checked")).toHaveText(`${f.skill}（v1）`);
  await expect(pin.getByTestId("miniapp-pin-changed")).toHaveCount(0);
  await pin.getByTestId("miniapp-pin-update").getByRole("button", { name: "v2 にする" }).click();

  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  await expect(page.getByText("v2 を下書きとして保存しました", { exact: true })).toBeVisible({ timeout: 15_000 });
  const saved = await api<{ body: { skill: { version: number } | null } }>(
    page,
    "GET",
    `/mini-apps/${f.appId}/resolved?version=2`,
  );
  expect(saved.body.skill?.version).toBe(2);
});

test("スマホ幅でも、束ねたスキルの新しいバージョンの差が横にはみ出さない（GAP-86）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const f = await setup(page);
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 2,
    ignoreHTTPSErrors: true,
  });
  try {
    const phone = await ctx.newPage();
    const dialog = await openEditor(phone, f.appId);
    const pin = dialog.getByTestId("miniapp-skill-pin");
    await expect(pin.getByTestId("miniapp-pin-update")).toContainText("新しいバージョン v2 があります", {
      timeout: 15_000,
    });
    await pin.scrollIntoViewIfNeeded();
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/miniapp-skill-pin-update-phone.png` });
    const overflow = await phone.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  } finally {
    await ctx.close();
  }
});
