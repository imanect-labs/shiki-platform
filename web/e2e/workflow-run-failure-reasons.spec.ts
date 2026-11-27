import { expect, test, type Browser, type Page } from "@playwright/test";

import { cleanupQueue, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 実行の詳細で、失敗の理由と再開の案内が本当の原因に合うこと（D-121・D-116・D-125）。
///
/// - 「script を実行」で、使ってよい操作の外の `Shiki.storage.read` を捕まえずに投げ返すと、権限の失敗として言い、
///   この実行のバージョンの設定で決まるので、再開も同じ入力の実行も勧めずにエディターへ案内する（BUG-136。以前は
///   分類できない失敗で「そのまま再開できる」と言っていた）。
/// - 作成者の script が投げた誤りは、設定の問題として言い、再開を勧めない（BUG-137）。再開の API も 409 で断る
///   （BUG-138）。
/// - この環境で構成されていない機能（社内文書の検索の無い環境の「社内検索」）の失敗は、この環境で使えない機能と言い、
///   作成者でなく管理者に連絡するよう言う（BUG-175。以前は権限が足りないと言っていた）。
/// - 「ファイルを読む」で PDF の取り込みを待っている間、ブロックは「ファイルの取り込みを待っています」と言い、次に
///   読む時刻と待つ上限を出す（D-125・GAP-50）。手元の検証のスタックには取り込みの worker が無いので、詳細の答えの
///   ブロックだけを、取り込みを待っている形に差し替えて画面を確かめる（待ち方そのものは api の結合テスト
///   workflow_document_read_it が確かめる）。

const SHOTS = process.env.E2E_SHOTS_DIR;

const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type RunDetail = {
  status: string;
  failure?: { category: string; message: string } | null;
  resume?: { advice: string; offer: boolean; offer_rerun: boolean; note: string } | null;
  steps: {
    node_id: string;
    status: string;
    attempt: number;
    error?: { code?: string } | null;
    next_retry_at?: string | null;
    waiting_for?: string | null;
  }[];
};

/// script を 1 つ置いたワークフローを API で作り、実行して終わるまで待つ（ワークフローと run の id・詳細）。
async function runScript(
  page: Page,
  prefix: string,
  declaredScopes: string[],
  source: string,
): Promise<{ id: string; runId: string; detail: RunDetail }> {
  const name = uniqueName(prefix);
  const created = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: declaredScopes,
        triggers: [{ kind: "interactive" }],
        nodes: [{ id: "sc", type: "script.run", label: "書類を確かめる", params: { source: { inline: source } } }],
        edges: [],
      },
    },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const id = ((await created.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  const started = await page.request.post(`/api/workflows/${id}/runs`, {
    headers: await csrfHeaders(page),
    data: { input: { file: "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" } },
  });
  expect(started.ok(), await started.text()).toBeTruthy();
  const runId = ((await started.json()) as { run_id: string }).run_id;
  const get = async () => (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as RunDetail;
  await expect
    .poll(async () => (await get()).status, { timeout: 60_000, intervals: [1_000] })
    .toMatch(/^(succeeded|failed)$/);
  return { id, runId, detail: await get() };
}

/// スマホ幅（390px）の画面で、同じ実行の詳細を開く（PC の page と同じログインの状態）。
async function openOnPhone(browser: Browser, page: Page, url: string, prepare?: (p: Page) => Promise<void>) {
  const ctx = await browser.newContext({
    storageState: await page.context().storageState(),
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
    deviceScaleFactor: 3,
    ignoreHTTPSErrors: true,
  });
  const phone = await ctx.newPage();
  if (prepare) await prepare(phone);
  await phone.goto(url);
  return { ctx, phone };
}

test("script の誤りは原因どおりに言い、再開しても同じ結果になる失敗は API でも再開を断る（D-121・D-116）", async ({
  browser,
  page,
}) => {
  test.setTimeout(240_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);

  // 使ってよい操作に「ファイルを読む」を含めずに、script が Shiki.storage.read を呼ぶ。
  const scope = await runScript(page, "script-scope", [], "function main(input) { return Shiki.storage.read(input.file); }");
  expect(scope.detail.status).toBe("failed");
  expect(scope.detail.steps.find((s) => s.node_id === "sc")?.error?.code).toBe("out_of_scope");
  expect(scope.detail.failure?.category).toBe("permission");
  expect(scope.detail.resume).toMatchObject({ advice: "same_result", offer: false, offer_rerun: false });

  await page.goto(`/workflows/${scope.id}/runs?run=${scope.runId}`);
  const sheet = page.getByRole("dialog");
  await expect(sheet.getByTestId("run-failure")).toHaveText(scope.detail.failure!.message, { timeout: 30_000 });
  await expect(sheet.getByTestId("run-failure")).toContainText("権限が足りない");
  await expect(sheet.getByTestId("run-resume-note")).toContainText("この実行のバージョンの設定で決まる失敗");
  await expect(sheet.getByRole("button", { name: "失敗したところから再開" })).toHaveCount(0);
  await expect(sheet.getByRole("button", { name: "同じ入力でもう一度実行" })).toHaveCount(0);
  await expect(sheet.getByRole("link", { name: "エディターを開く" })).toBeVisible();
  // ブロックの行は、分類できない失敗（internal）ではなく元の理由のコードを添える（作成者向け）。
  await expect(sheet.getByTestId("step-error-code")).toHaveText("（out_of_scope）");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/run-failure-script-scope-pc.png` });

  // 作成者の script が投げた誤り。
  const thrown = await runScript(
    page,
    "script-throw",
    ["storage.read"],
    "function main(input) { if (!input.amount) { throw new Error('金額がありません'); } return 1; }",
  );
  expect(thrown.detail.status).toBe("failed");
  expect(thrown.detail.steps.find((s) => s.node_id === "sc")?.error?.code).toBe("script_error");
  expect(thrown.detail.failure?.category).toBe("configuration");
  expect(thrown.detail.resume).toMatchObject({ advice: "same_result", offer: false, offer_rerun: true });

  // 再開の API も、画面と同じ定義で断る（run は失敗のまま）。
  const refused = await page.request.post(`/api/workflows/${thrown.id}/runs/${thrown.runId}/retry`, {
    headers: await csrfHeaders(page),
    data: { mode: "resume" },
  });
  expect(refused.status()).toBe(409);
  expect(await refused.json()).toMatchObject({ code: "same_result", message: thrown.detail.resume!.note });

  const url = `/workflows/${thrown.id}/runs?run=${thrown.runId}`;
  await page.goto(url);
  await expect(sheet.getByTestId("run-failure")).toHaveText(
    "ワークフローの設定に問題があり、処理を完了できませんでした。作成者に連絡してください。",
    { timeout: 30_000 },
  );
  await expect(sheet.getByTestId("run-resume-note")).toHaveText(thrown.detail.resume!.note);
  await expect(sheet.getByRole("button", { name: "失敗したところから再開" })).toHaveCount(0);
  await expect(sheet.getByRole("button", { name: "同じ入力でもう一度実行" })).toBeVisible();
  await expect(sheet.getByTestId("step-error-code")).toHaveText("（script_error）");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/run-failure-script-throw-pc.png` });

  const { ctx, phone } = await openOnPhone(browser, page, url);
  try {
    const phoneSheet = phone.getByRole("dialog");
    await expect(phoneSheet.getByTestId("run-failure")).toBeVisible({ timeout: 30_000 });
    await expect(phoneSheet.getByRole("button", { name: "同じ入力でもう一度実行" })).toBeVisible();
    await expect(phoneSheet.getByRole("button", { name: "失敗したところから再開" })).toHaveCount(0);
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/run-failure-script-throw-phone.png` });
  } finally {
    await ctx.close();
  }
});

test("PDF の取り込みを待っているブロックは、取り込みを待っていると言い、次に読む時刻と待つ上限を出す（D-125）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);

  // 実行を 1 つ作り、詳細の答えのブロックを、取り込みを待って読み直しを待つ形に差し替える。
  const done = await runScript(page, "index-wait", [], "function main(input) { return { ok: true }; }");
  expect(done.detail.status).toBe("succeeded");
  const detailUrl = `/api/workflows/${done.id}/runs/${done.runId}`;
  const nextRead = new Date(Date.now() + 90_000).toISOString();
  const waiting = async (p: Page) => {
    await p.route(`**${detailUrl}`, async (route) => {
      const res = await route.fetch();
      const body = (await res.json()) as RunDetail & { finished_at?: string | null };
      body.status = "running";
      body.finished_at = null;
      body.steps = body.steps.map((s) =>
        s.node_id === "sc"
          ? { ...s, status: "ready", attempt: 0, error: null, next_retry_at: nextRead, waiting_for: "index" }
          : s,
      );
      await route.fulfill({ response: res, json: body });
    });
    // 実行中の詳細は進み具合を受け取り続ける。終わった実行の流れが閉じて読み直しにならないよう、流れは空のまま開いておく。
    await p.route(`**${detailUrl}/events/stream**`, (route) =>
      route.fulfill({ status: 200, headers: { "content-type": "text/event-stream" }, body: ": keep\n\n" }),
    );
  };
  await waiting(page);
  await page.goto(`/workflows/${done.id}/runs?run=${done.runId}`);
  const sheet = page.getByRole("dialog");
  await expect(sheet.getByText("ファイルの取り込みを待っています")).toBeVisible({ timeout: 30_000 });
  await expect(sheet.getByTestId("step-retry-at")).toContainText("にもう一度読みます（最大 30 分待ちます）");
  // 待つ間は試す回数を使わないので、何回目かの印は出さない。失敗の帯も出さない。
  await expect(sheet.getByTestId("run-failure")).toHaveCount(0);
  await expect(sheet.getByText(/回目$/)).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/run-index-wait-pc.png` });

  const { ctx, phone } = await openOnPhone(browser, page, `/workflows/${done.id}/runs?run=${done.runId}`, waiting);
  try {
    const phoneSheet = phone.getByRole("dialog");
    await expect(phoneSheet.getByText("ファイルの取り込みを待っています")).toBeVisible({ timeout: 30_000 });
    await expect(phoneSheet.getByTestId("step-retry-at")).toBeVisible();
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/run-index-wait-phone.png` });
  } finally {
    await ctx.close();
  }
});

test("社内文書の検索の無い環境の失敗は、この環境で使えない機能と言い、管理者に連絡するよう言う（BUG-175）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 手元の検証のスタックは社内文書の検索を構成しない（RAG なし）。「社内検索」を置いて実行する。
  const name = uniqueName("rag-not-configured");
  const created = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: ["rag.query"],
        triggers: [{ kind: "interactive" }],
        nodes: [{ id: "sc", type: "rag.search", label: "規程を探す", params: { query: "就業規則" } }],
        edges: [],
      },
    },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const id = ((await created.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  const started = await page.request.post(`/api/workflows/${id}/runs`, {
    headers: await csrfHeaders(page),
    data: { input: {} },
  });
  expect(started.ok(), await started.text()).toBeTruthy();
  const runId = ((await started.json()) as { run_id: string }).run_id;
  const get = async () => (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as RunDetail;
  await expect
    .poll(async () => (await get()).status, { timeout: 60_000, intervals: [1_000] })
    .toBe("failed");
  const detail = await get();
  expect(detail.steps.find((s) => s.node_id === "sc")?.error?.code).toBe("not_configured");
  expect(detail.failure).toEqual({
    category: "environment",
    message: "この環境では使えない機能を使おうとしたため、処理を完了できませんでした。管理者に連絡してください。",
    reference: runId.replace(/-/g, "").slice(0, 8),
  });
  // 管理者が構成した後なら再開で通るので、原因を直してから再開するよう言う。
  expect(detail.resume).toMatchObject({ advice: "fix_first", offer: true });

  const url = `/workflows/${id}/runs?run=${runId}`;
  await page.goto(url);
  const sheet = page.getByRole("dialog");
  await expect(sheet.getByTestId("run-failure")).toContainText(detail.failure!.message, { timeout: 30_000 });
  await expect(sheet.getByTestId("run-failure")).not.toContainText("作成者");
  await expect(sheet.getByText("この環境では社内文書の検索を使えません。管理者に連絡してください。").first()).toBeVisible();
  await expect(sheet.getByTestId("step-error-code")).toHaveText("（not_configured）");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/run-failure-not-configured-pc.png` });

  const { ctx, phone } = await openOnPhone(browser, page, url);
  try {
    const phoneSheet = phone.getByRole("dialog");
    await expect(phoneSheet.getByTestId("run-failure")).toContainText("管理者に連絡してください", { timeout: 30_000 });
    expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
    if (SHOTS) await phone.screenshot({ path: `${SHOTS}/run-failure-not-configured-phone.png` });
  } finally {
    await ctx.close();
  }
});
