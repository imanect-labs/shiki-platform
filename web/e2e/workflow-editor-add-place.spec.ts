import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// ブロックを足す場所と、線のないブロック（BUG-106・付録 D の N2.9・N2.10）。
///
/// - パレットのブロックを押すと流れの最後に入る。分岐の後では最初の出口（「はい」）の側に入るので、どこに入ったかを
///   通知で言う（画面で決めた場所はキャンバスの線で裏取りする）。
/// - パレットからドラッグで置いたブロックは線がないので、保存はできるが、実行の始めに動くことを注意で言う。
/// - スマホ幅の一覧は、前から線が来ないブロックを「ここからも始まる流れ」として出し、線が輪になったブロックの見出しは
///   出さない（線のないブロックは輪ではない）。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub（このテストは実行しない）。作ったワークフローは
/// 最後に消す。

async function csrf(page: Page): Promise<Record<string, string>> {
  const token = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": token, "content-type": "application/json" };
}

type SavedIr = {
  nodes: { id: string; type: string }[];
  edges: { from: string; from_port?: string | null; to: string }[];
};

async function savedIr(page: Page, id: string): Promise<SavedIr> {
  const res = await page.request.get(`/api/workflows/${id}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { ir: SavedIr }).ir;
}

/// 金額で分けるだけのワークフロー（分岐の後ろはまだ何もない）。
async function createBranchFlow(page: Page): Promise<string> {
  const name = uniqueName("wf-add-place");
  const res = await page.request.post("/api/workflows", {
    headers: await csrf(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "interactive" }],
        input_schema: {
          type: "object",
          properties: { amount: { type: "number", title: "金額" } },
          required: ["amount"],
        },
        nodes: [
          {
            id: "branch",
            type: "control.branch",
            label: "金額で分ける",
            params: { condition: { cmp: { left: { $from: "input", path: "/amount" }, op: "gt", right: 100 } } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.status(), await res.text()).toBe(201);
  return ((await res.json()) as { id: string }).id;
}

const UNCONNECTED = "線でつながっていないので、実行の始めにほかのブロックと同時に動きます。";

test("パレット: 押すと流れの最後に入り、分岐の後ではどの出口の後ろかを言う。ドラッグで置いたブロックは注意する（BUG-106）", async ({ page }) => {
  test.setTimeout(150_000);
  await loginViaKeycloak(page);
  let id = "";
  try {
    id = await createBranchFlow(page);
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`/workflows/${id}`);
    await expect(page.getByTestId("rf__node-branch")).toBeVisible({ timeout: 15_000 });
    await page.getByRole("button", { name: "ブロック", exact: true }).click();
    const palette = page.getByRole("complementary", { name: "ブロック一覧" });
    await expect(palette).toContainText("押すと、流れの最後に追加します。ドラッグで置いたブロックは、線でつないでください。");
    await expect(palette.getByRole("searchbox", { name: "ブロックを検索" })).toHaveAttribute("placeholder", "ブロックを検索");

    // 準備中のブロックは閉じた見出しで出し、開けることが分かる印を付ける。検索を消しても、開けた状態は勝手に閉じない。
    const upcoming = palette.locator("details").filter({ has: page.locator("summary", { hasText: "準備中" }) });
    const isOpen = () => upcoming.evaluate((el) => (el as HTMLDetailsElement).open);
    await expect.poll(isOpen).toBe(false);
    await expect(upcoming.locator("summary svg")).toBeVisible();
    await upcoming.locator("summary").click();
    await expect.poll(isOpen).toBe(true);
    const search = palette.getByRole("searchbox", { name: "ブロックを検索" });
    await search.fill("該当しない語");
    await expect(upcoming).toHaveCount(0);
    await search.fill("");
    await expect.poll(isOpen).toBe(true);
    await upcoming.locator("summary").click();
    await expect.poll(isOpen).toBe(false);

    // 押して足す: 分岐の後ろでは最初の出口（はい）の側に入る。通知で場所を言い、線で裏取りする。
    await palette.getByRole("button", { name: /AI に聞く/ }).click();
    await expect(page.getByText("「AI に聞く」を追加しました").first()).toBeVisible();
    await expect(page.getByText("「金額で分ける」の「はい」の後ろに入れました。").first()).toBeVisible();
    await expect(page.locator('[data-testid="rf__edge-e:branch|true|invoke_1"]')).toHaveCount(1);
    await page.screenshot({ path: "test-results/add-place-palette-pc.png" });

    // ドラッグで置く: 線のないブロックになり、保存はできるが実行の始めに動くことを注意する。
    const item = palette.getByRole("button", { name: /結果を画面に返す/ });
    const pane = page.locator(".react-flow__pane");
    const box = (await pane.boundingBox())!;
    const dt = await page.evaluateHandle(() => new DataTransfer());
    await item.dispatchEvent("dragstart", { dataTransfer: dt });
    const at = { clientX: box.x + box.width * 0.5, clientY: box.y + box.height * 0.85 };
    await pane.dispatchEvent("dragover", { dataTransfer: dt, ...at });
    await pane.dispatchEvent("drop", { dataTransfer: dt, ...at });
    await expect(page.getByTestId("rf__node-reply_1")).toBeVisible();
    // 注意はキャンバスの上の「確認が必要な…」から開く一覧に、ブロックごとに出る。
    await page.getByTestId("workflow-warnings").click({ timeout: 15_000 });
    await expect(page.getByText(UNCONNECTED).first()).toBeVisible({ timeout: 15_000 });
    await page.screenshot({ path: "test-results/add-place-unconnected-pc.png" });
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const ir = await savedIr(page, id);
    expect(ir.nodes.map((n) => n.id).sort()).toEqual(["branch", "invoke_1", "reply_1"]);
    expect(ir.edges).toEqual([{ from: "branch", from_port: "true", to: "invoke_1" }]);

    // スマホ幅の一覧: 線のないブロックは「ここからも始まる流れ」に並び、注意の印が付く。輪の見出しは出さない。
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`/workflows/${id}`);
    const toList = page.getByRole("button", { name: "一覧で表示" });
    if (await toList.isVisible()) await toList.click();
    const flow = page.getByTestId("workflow-mobile-flow");
    await expect(flow).toBeVisible({ timeout: 15_000 });
    await expect(flow.getByText("ここからも始まる流れ", { exact: true })).toBeVisible();
    await expect(flow.getByText("前から線が来ないブロックは、実行の始めにほかと同時に動きます。")).toBeVisible();
    await expect(
      flow.getByRole("button").filter({ hasText: "結果を画面に返す" }).filter({ hasText: "注意" }),
    ).toBeVisible();
    await expect(flow.getByTestId("mobile-flow-cycle")).toHaveCount(0);
    await expect(
      flow.getByRole("button", { name: "「金額で分ける」と「AI に聞く」の間にブロックを追加" }),
    ).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
    await flow
      .getByText("ここからも始まる流れ", { exact: true })
      .evaluate((el) => el.scrollIntoView({ block: "center" }));
    await page.screenshot({ path: "test-results/add-place-mobile-flow-phone.png" });
  } finally {
    await cleanupAll(async () => {
      if (!id) return;
      const res = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrf(page) });
      expect(res.status(), await res.text()).toBeLessThan(300);
    });
  }
});
