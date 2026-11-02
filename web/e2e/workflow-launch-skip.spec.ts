import { expect, test, type Page } from "@playwright/test";

import {
  cleanupQueue,
  deleteFgaTuples,
  loginViaKeycloak,
  removeWorkflowApi,
  uniqueName,
  type FgaTuple,
} from "./helpers";

/// 自動実行の起動の見送りを作成者に見せる e2e（GAP-67）:
/// - スケジュールでオンにしたワークフローの、自動実行の主体が自分の定義を読めなくなると（BUG-146）、スケジュールの
///   起動を見送る。ワークフローの画面の自動実行の状態に「自動実行を見送っています」と、理由と直し方を出す。
/// - ワークフローの一覧でも「有効」ではなく「自動実行を見送り中」と出す。
/// - 自動実行の設定には、理由といつから何回見送ったかを出す。「同意してオンにする」でオンにし直すと消える。
///
/// 見送りを起こすため、毎分のスケジュールでオンにした後、自動実行の主体の閲覧のタプルを OpenFGA から外す（画面や
/// API に入口の無い食い違い）。ブロックは script.run だけで、実 LLM を呼ばない。オンにし直した後の実行は片付けで
/// 止めて消す。

const SHOTS = process.env.E2E_SHOTS_DIR;
const cleanup = cleanupQueue();
test.afterEach(() => cleanup.run());

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type Registration = {
  status: string;
  launch_skip: { code: string; message: string; fix: string; count: number } | null;
};

async function registration(page: Page, id: string): Promise<Registration> {
  const res = await page.request.get(`/api/workflows/${id}/registration`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return (await res.json()) as Registration;
}

test("自動実行: 起動を見送り続けているとき、理由と直し方を出し、オンにし直すと消える（GAP-67）", async ({ page }) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const me = (await (await page.request.get("/api/me")).json()) as { tenant_id: string };
  const name = uniqueName("launch-skip");
  const created = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: [],
        // 毎分（見送りをすぐ起こす）。
        triggers: [{ kind: "schedule", cron: "* * * * *", tz: "Asia/Tokyo", catchup: "skip" }],
        nodes: [
          {
            id: "calc",
            type: "script.run",
            label: "計算する",
            params: { source: { inline: "function main(i){ return {}; }" } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const id = ((await created.json()) as { id: string }).id;
  const selfRead: FgaTuple = {
    user: `workflow:${me.tenant_id}|${id}`,
    relation: "viewer",
    object: `artifact:${me.tenant_id}|${id}`,
  };
  // 片付けは互いに独立（オフにして権限も取り消す・実行を止めて消す・自分の定義を読むタプルを消す）。
  cleanup.add(() => deleteFgaTuples(page, [selfRead]));
  cleanup.add(() => removeWorkflowApi(page, id));
  cleanup.add(async () => {
    const off = await page.request.post(`/api/workflows/${id}/disable`, {
      headers: await csrfHeaders(page),
      data: { revoke_delegations: true },
    });
    expect([200, 404], await off.text()).toContain(off.status());
  });

  const on = await page.request.post(`/api/workflows/${id}/enable`, {
    headers: await csrfHeaders(page),
    data: { version: 1, consent: true, grants: [] },
  });
  expect(on.ok(), await on.text()).toBeTruthy();
  // 自動実行の主体が自分の定義を読めなくなる（オンにした後に認可だけが食い違った状態・BUG-146）。
  await deleteFgaTuples(page, [selfRead]);
  await expect
    .poll(async () => (await registration(page, id)).launch_skip?.code ?? null, {
      timeout: 150_000,
      intervals: [5_000],
      message: "スケジュールの起動を見送ったことが記録される",
    })
    .toBe("definition_unreadable");
  expect((await registration(page, id)).status).toBe("enabled");

  // 一覧: オンのまま見送り続けているワークフローを「有効」と言わない（GAP-67）。
  await page.goto("/workflows");
  const row = page.getByTestId("workflow-row").filter({ hasText: name });
  await expect(row.getByTestId("workflow-row-launch-skip")).toHaveText("自動実行を見送り中");
  await expect(row.getByText("有効", { exact: true })).toHaveCount(0);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/launch-skip-pc-list.png` });

  // PC: 自動実行の状態に出す。理由と直し方はツールチップにも出し、押すと自動実行の設定を開く。
  await page.goto(`/workflows/${id}`);
  const badge = page.getByTestId("workflow-launch-skip");
  await expect(badge).toHaveText("自動実行を見送っています");
  await expect(page.getByText(/自動実行 有効/)).toHaveCount(0);
  await badge.hover();
  await expect(page.getByRole("tooltip")).toContainText(
    "自動実行のとき、このワークフローを読み込めませんでした。「自動実行の設定」を開いて、もう一度オンにしてください。",
  );
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/launch-skip-pc-badge.png` });
  await badge.click();
  const dialog = page.getByRole("dialog");
  const notice = dialog.getByTestId("launch-skip-notice");
  await expect(notice).toContainText("自動実行を見送っています");
  await expect(notice).toContainText("自動実行のとき、このワークフローを読み込めませんでした。");
  await expect(notice).toContainText(/\d{4}\/\d{2}\/\d{2} \d{2}:\d{2} (に|から)/);
  // オンにし直せば直るので、ダイアログの中では直し方を言わない。
  await expect(notice.getByTestId("launch-skip-fix")).toHaveCount(0);
  // 内部の言葉・code を出さない。
  await expect(notice).not.toContainText("definition_unreadable");
  await expect(notice).not.toContainText("委譲");
  await expect(dialog.getByRole("button", { name: "同意してオンにする" })).toBeEnabled();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/launch-skip-pc-dialog.png` });
  await dialog.getByRole("button", { name: "閉じる" }).click();
  await expect(dialog).toBeHidden();

  // スマホ: 一覧のバッジは横にはみ出さない。
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/workflows");
  await expect(row.getByTestId("workflow-row-launch-skip")).toBeVisible();
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth),
    "一覧が横にはみ出さない",
  ).toBeLessThanOrEqual(390);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/launch-skip-phone-list.png` });

  // スマホ: タップできないツールチップの代わりに、理由と直し方をバッジの近くに文字で出す。
  await page.goto(`/workflows/${id}`);
  await expect(page.getByTestId("workflow-mobile-flow")).toBeVisible();
  await expect(page.getByTestId("workflow-launch-skip")).toBeVisible();
  await expect(page.getByTestId("workflow-launch-skip-reason")).toHaveText(
    "自動実行のとき、このワークフローを読み込めませんでした。「自動実行の設定」を開いて、もう一度オンにしてください。",
  );
  const width = await page.evaluate(() => document.documentElement.scrollWidth);
  expect(width, "横にはみ出さない").toBeLessThanOrEqual(390);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/launch-skip-phone-badge.png` });
  await page.getByTestId("workflow-launch-skip").click();
  await expect(page.getByRole("dialog").getByTestId("launch-skip-notice")).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/launch-skip-phone-dialog.png` });

  // オンにし直すと、自動実行の主体が自分の定義を読めるようになり、見送りの表示は消える。
  await page.getByRole("dialog").getByRole("button", { name: "同意してオンにする" }).click();
  await expect(page.getByText("自動実行をオンにしました").first()).toBeVisible({ timeout: 15_000 });
  await expect(page.getByTestId("workflow-launch-skip")).toHaveCount(0);
  expect((await registration(page, id)).launch_skip).toBeNull();
  await expect(page.getByText("自動実行 有効（v1）")).toBeVisible();
  // 一覧も「有効」に戻る。
  await page.goto("/workflows");
  await expect(row.getByText("有効", { exact: true })).toBeVisible();
  await expect(row.getByTestId("workflow-row-launch-skip")).toHaveCount(0);
});

/// オンにしている版に実行できない設定がある見送り（not_ready）は、オンにし直しても直らないので、自動実行の設定の
/// ダイアログの中でもサーバーの直し方の文を出す（PC はツールチップを読まずにバッジを押せる）。見送りの記録は
/// 登録の応答を差し替えて作る（保存の確かめを通らない版を作る入口が画面と API に無い）。
test("自動実行: 実行できない設定で見送っているときは、ダイアログにも直し方を出す（GAP-67）", async ({ page }) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const name = uniqueName("launch-skip-not-ready");
  const created = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: [],
        triggers: [{ kind: "schedule", cron: "0 9 * * *", tz: "Asia/Tokyo", catchup: "skip" }],
        nodes: [
          {
            id: "calc",
            type: "script.run",
            label: "計算する",
            params: { source: { inline: "function main(i){ return {}; }" } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const id = ((await created.json()) as { id: string }).id;
  cleanup.add(() => removeWorkflowApi(page, id));
  cleanup.add(async () => {
    const off = await page.request.post(`/api/workflows/${id}/disable`, {
      headers: await csrfHeaders(page),
      data: { revoke_delegations: true },
    });
    expect([200, 404], await off.text()).toContain(off.status());
  });
  const on = await page.request.post(`/api/workflows/${id}/enable`, {
    headers: await csrfHeaders(page),
    data: { version: 1, consent: true, grants: [] },
  });
  expect(on.ok(), await on.text()).toBeTruthy();

  const fix = "エディターで設定を直して保存してから、「自動実行の設定」でもう一度オンにしてください。";
  await page.route(`**/api/workflows/${id}/registration`, async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const res = await route.fetch();
    const body = (await res.json()) as Record<string, unknown>;
    body.launch_skip = {
      code: "not_ready",
      message: "オンにしている v1 に、空の欄など実行できない設定があります。",
      fix,
      version: 1,
      first_at: "2026-11-02T00:00:00Z",
      last_at: "2026-11-02T01:00:00Z",
      count: 2,
    };
    await route.fulfill({ response: res, json: body });
  });
  await page.goto(`/workflows/${id}`);
  await page.getByTestId("workflow-launch-skip").click();
  const notice = page.getByRole("dialog").getByTestId("launch-skip-notice");
  await expect(notice).toContainText("オンにしている v1 に、空の欄など実行できない設定があります。");
  await expect(notice.getByTestId("launch-skip-fix")).toHaveText(fix);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/launch-skip-not-ready-pc-dialog.png` });
  await page.getByRole("dialog").getByRole("button", { name: "閉じる" }).click();
});
