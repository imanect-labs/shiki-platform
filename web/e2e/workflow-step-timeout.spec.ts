import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// ブロックの時間の上限（ノードの timeout_sec・BUG-96・D-82）の e2e:
/// エンジンが打ち切るようになったので、打ち切る種のブロックにだけ「時間の上限（秒・省略可）」を出す。
/// 注記の既定と最大はカタログから。上限を超える値は保存時の誤りになる。流れの制御には出さない。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrf(page: Page): Promise<string> {
  return (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
}

test("時間の上限: AI に聞くは既定 120 秒・最大 600 秒で、超える値は保存できず、分岐には欄が無い（BUG-96）", async ({ browser, page }) => {
  test.setTimeout(150_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("step-timeout");
  let id = "";
  try {
    const res = await page.request.post("/api/workflows", {
      headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            { id: "ask", type: "llm.invoke", label: "要約する", params: { prompt: "要点をまとめて" } },
            {
              id: "check",
              type: "control.branch",
              label: "長いか",
              params: { condition: { cmp: { left: { $from: "nodes.ask.output", path: "/text" }, op: "exists" } } },
            },
          ],
          edges: [{ from: "ask", to: "check" }],
        },
      },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    id = ((await res.json()) as { id: string }).id;
    await page.goto(`/workflows/${id}`);

    await page.getByTestId("rf__node-ask").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    const limit = panel.getByRole("spinbutton", { name: /時間の上限/ });
    await expect(limit).toBeVisible();
    await expect(panel.getByText("空欄なら 120 秒（2 分）で、最大 600 秒です。過ぎると、このブロックは失敗します。")).toBeVisible();
    await expect(panel.getByText("時間制限（秒）")).toHaveCount(0);

    // 上限を超える値は保存時の誤り（ライブ検証で、上の帯ではなくこの欄の下に出る・付録 D の N1.11）。
    await limit.fill("900");
    await expect(limit).toHaveAccessibleDescription("「時間の上限」は 1〜600 秒にしてください。", { timeout: 10_000 });
    await expect(panel.getByText("「時間の上限」は 1〜600 秒にしてください。")).toHaveCount(1);
    await panel.getByText("「時間の上限」は 1〜600 秒にしてください。").scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/step-timeout-invalid-pc.png` });
    await limit.fill("300");
    await expect(panel.getByText("「時間の上限」は 1〜600 秒にしてください。")).toHaveCount(0, { timeout: 10_000 });
    await expect(limit).toHaveAccessibleDescription(
      "空欄なら 120 秒（2 分）で、最大 600 秒です。過ぎると、このブロックは失敗します。",
    );
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
      ir: { nodes: { id: string; timeout_sec?: number }[] };
    };
    expect(saved.ir.nodes.find((n) => n.id === "ask")?.timeout_sec).toBe(300);
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/step-timeout-pc.png` });

    // 流れの制御（条件で分ける）は打ち切らないので欄を出さない。
    await page.getByTestId("rf__node-check").click();
    await expect(panel.getByRole("spinbutton", { name: /時間の上限/ })).toHaveCount(0);

    // スマホ幅のシートでも欄と注記が出て、横にはみ出さない。
    const phoneCtx = await browser.newContext({
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      hasTouch: true,
      isMobile: true,
      deviceScaleFactor: 3,
      ignoreHTTPSErrors: true,
    });
    try {
      const phone = await phoneCtx.newPage();
      await phone.goto(`/workflows/${id}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      // ブロックのカード（名前の前に種類の「AI」が付く。カードの間の＋は「「要約する」の前に挿入」）。
      await flow.getByRole("button", { name: /^AI\s*要約する/ }).click({ timeout: 10_000 });
      const sheet = phone.getByRole("dialog");
      const phoneLimit = sheet.getByRole("spinbutton", { name: /時間の上限/ });
      await phoneLimit.scrollIntoViewIfNeeded({ timeout: 10_000 });
      await expect(phoneLimit).toHaveValue("300");
      await expect(sheet.getByText("空欄なら 120 秒（2 分）で、最大 600 秒です。過ぎると、このブロックは失敗します。")).toBeVisible();
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/step-timeout-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(async () => {
      if (!id) return;
      const del = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": await csrf(page) } });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});

test("時間の上限: AI エージェントは「AI の作業時間の上限」より短くできず、誤りは欄の下に出る（BUG-96）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("step-timeout-agent");
  let id = "";
  try {
    const res = await page.request.post("/api/workflows", {
      headers: { "x-csrf-token": await csrf(page), "content-type": "application/json" },
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "research",
              type: "agent.invoke",
              label: "規程を調べる",
              params: { instruction: "旅費規程の要点をまとめて", allowed_tools: ["rag.search"] },
            },
          ],
          edges: [],
        },
      },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    id = ((await res.json()) as { id: string }).id;
    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-research").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });

    // 作業時間の欄は、時間の上限の注記と誤りが指す名前で出る（既定と範囲は agent の設定から）。
    const duration = panel.getByRole("spinbutton", { name: "AI の作業時間の上限（秒・省略可）" });
    await expect(duration).toHaveAccessibleDescription(/^空欄なら 600 秒（10 分）で、10〜3,600 秒にできます。/, {
      timeout: 10_000,
    });
    const limit = panel.getByRole("spinbutton", { name: /時間の上限（秒/ }).last();
    await expect(limit).toHaveAccessibleDescription(
      "空欄なら 600 秒（10 分）で、最大 3,600 秒です。「AI の作業時間の上限」より短くはできません。過ぎると、このブロックは失敗します。",
    );

    // 作業時間（省略は 600 秒）より短い時間の上限は、保存時の誤りとして欄の下に出る。
    await limit.fill("300");
    const tooShort = "「時間の上限」は「AI の作業時間の上限」（600 秒）より短くできません。600 秒以上にするか、空欄にしてください。";
    await expect(limit).toHaveAccessibleDescription(tooShort, { timeout: 10_000 });
    await panel.getByText(tooShort).scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/step-timeout-agent-pc.png` });
    // 作業時間を短くすると通る。
    await duration.fill("120");
    await expect(panel.getByText(tooShort)).toHaveCount(0, { timeout: 10_000 });
  } finally {
    await cleanupAll(async () => {
      if (!id) return;
      const del = await page.request.delete(`/api/artifacts/${id}`, { headers: { "x-csrf-token": await csrf(page) } });
      expect(del.status(), await del.text()).toBeLessThan(300);
    });
  }
});
