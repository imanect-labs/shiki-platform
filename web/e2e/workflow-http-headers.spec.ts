import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 「外部 API 呼び出し」の「ヘッダー」の e2e（GAP-19・BUG-184）:
/// - ヘッダーの名前と値の組を足して保存すると、IR の params.headers に入る（欄の無かったころの定義には headers が無い）。
/// - Shiki が決めるヘッダー（Host）を書くと、保存の前の確かめで欄の下に理由が出る（サーバーの決まり・nodes/http_headers.rs）。
/// - シークレットを付けるヘッダーの名前に Host を書くと、使うシークレットの欄の下に理由が出る（BUG-184）。
/// - スマホ幅のシートでも、ヘッダーの行が横にはみ出さない。
/// - Authorization など鍵の名前のヘッダーに値をそのまま書くと、保存は止めずに欄の下で知らせる（値は映し返さない・
///   ir.plain_key_header）。
/// 送ったヘッダーが相手に届くこと・送ってはいけないヘッダーを実行で断ることは、Rust の結合テスト
/// （workflow_nodes_it の http_request_node_sends_author_headers_through_the_production_port）と
/// executor_it で確かめる（e2e は外部へ送らない）。
///
/// 作ったワークフローは finally で片付ける。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 次のライブの確かめ（保存の前の検証）のうち、送った定義が `sent` に当たるものの応答を待つ。確かめは打鍵の 600ms 後に
/// 送るので、前の編集の分が待ち受けの後に届くことがある（その応答では解決しない）。
function nextValidation(page: Page, sent: (body: string) => boolean) {
  return page.waitForResponse(
    (r) =>
      r.url().endsWith("/api/workflows/validate") &&
      r.request().method() === "POST" &&
      sent(r.request().postData() ?? ""),
  );
}

test("外部 API 呼び出し: ヘッダーを名前と値で足し、書けないヘッダーは欄の下で理由を言う（GAP-19・BUG-184）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  page.on("dialog", (d) => void d.accept());
  let id = "";
  try {
    const name = uniqueName("http-headers");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "hook",
              type: "http.request",
              label: "受付を知らせる",
              params: { method: "POST", url: "https://hooks.example.com/notify", fail_on_error_status: true },
            },
          ],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    id = ((await created.json()) as { id: string }).id;

    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-hook").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByText("ヘッダー（省略可）", { exact: true })).toBeVisible();
    await expect(panel.getByText("「ヘッダーを追加」で、ヘッダーの名前と値の組を足します。", { exact: true })).toBeVisible();
    await panel.getByTestId("http-headers-rule").getByRole("button", { name: "詳しく" }).click();
    await expect(panel.getByText(/20 個まで、値は 1 つ 4 KB まで、名前と値を合わせて 8 KB までです。/)).toBeVisible();
    await expect(
      panel.getByText(
        "書いたヘッダーを付けて送ります。値には、入力や前のブロックの結果を差し込めます。ここに書いた値はワークフローを見られる人に見えるので、API キーなどの鍵はシークレットで付けてください。",
        { exact: true },
      ),
    ).toBeVisible();
    await expect(
      panel.getByText(
        "ここに書いた値はバージョン履歴にも残り、送り先の応答に同じ文字が返っても隠しません（隠すのは、「使用するシークレット」で付けた値だけです）。鍵はシークレットに入れて、「使用するシークレット」で付けてください。",
        { exact: true },
      ),
    ).toBeVisible();

    // 1 行目: X-Request-Id（打った値のまま入る）。
    await panel.getByRole("button", { name: "ヘッダーを追加" }).click();
    const rows = panel.getByTestId("compose-row");
    await rows.nth(0).getByRole("textbox", { name: "ヘッダーの名前" }).fill("X-Request-Id");
    await expect(rows.nth(0).getByTestId("compose-row-issue")).toHaveText("値を入れるまで、このヘッダーは保存しません。");
    await rows.nth(0).getByRole("textbox", { name: "「X-Request-Id」の値" }).fill("expense-1");
    await expect(rows.nth(0).getByTestId("compose-row-issue")).toHaveCount(0);

    // 2 行目: Host は Shiki が決めるので書けない（サーバーの確かめで欄の下に出る）。
    await panel.getByRole("button", { name: "ヘッダーを追加" }).click();
    await rows.nth(1).getByRole("textbox", { name: "ヘッダーの名前" }).fill("Host");
    const refused = nextValidation(page, (body) => body.includes("internal.example.com"));
    await rows.nth(1).getByRole("textbox", { name: "「Host」の値" }).fill("internal.example.com");
    expect(JSON.stringify(await (await refused).json())).toContain("ヘッダー「Host」は Shiki が決めるため");
    const hostError = panel.getByText("ヘッダー「Host」は Shiki が決めるため、指定できません。", { exact: true });
    await expect(hostError).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/http-headers-host-pc.png` });

    // Host の行を外すと理由が消え、保存すると IR に入る。
    const cleared = nextValidation(page, (body) => !body.includes("internal.example.com"));
    await rows.nth(1).getByRole("button", { name: "「Host」を外す" }).click();
    await cleared;
    await expect(hostError).toHaveCount(0);
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
      ir: { nodes: { id: string; params: Record<string, unknown> }[] };
    };
    expect(saved.ir.nodes.find((n) => n.id === "hook")?.params.headers).toEqual({ "X-Request-Id": "expense-1" });
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/http-headers-saved-pc.png` });

    // シークレットを付けるヘッダーの名前も、Shiki が決めるヘッダーにはできない（BUG-184）。
    await panel.getByRole("textbox", { name: "使用するシークレット（省略可）" }).fill("webhook-token");
    await panel.getByRole("combobox", { name: "シークレットの付け方" }).click();
    await page.getByRole("option", { name: "決めたヘッダーに付ける" }).click();
    const secretHeader = panel.getByRole("textbox", { name: "シークレットを付けるヘッダーの名前" });
    await expect(secretHeader).toHaveValue("X-Api-Key");
    const secretRefused = nextValidation(page, (body) => body.includes('"header":"Host"'));
    await secretHeader.fill("Host");
    expect(JSON.stringify(await (await secretRefused).json())).toContain(
      "シークレットを付けるヘッダー「Host」は Shiki が決めるため、指定できません。",
    );
    await expect(
      panel.getByText("シークレットを付けるヘッダー「Host」は Shiki が決めるため、指定できません。", { exact: true }),
    ).toBeVisible();
    if (SHOTS) await panel.screenshot({ path: `${SHOTS}/http-headers-secret-host-pc.png` });

    // スマホの一覧から開いても、保存したヘッダーの行が見え、横にはみ出さない。
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
      phone.on("dialog", (d) => void d.accept());
      await phone.goto(`/workflows/${id}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /受付を知らせる/ }).filter({ hasText: "受付を知らせる" }).first().click();
      const sheet = phone.getByRole("dialog");
      const nameBox = sheet.getByRole("textbox", { name: "ヘッダーの名前" });
      await expect(nameBox).toHaveValue("X-Request-Id");
      await nameBox.evaluate((el) => el.scrollIntoView({ block: "center" }));
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/http-headers-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(async () => {
      if (id) await removeWorkflowApi(page, id);
    });
  }
});

const KEY_WARNING =
  "ヘッダー「Authorization」に、値をそのまま書いています。この値は、ワークフローを見られる人に見え、バージョン履歴にも残ります。API キーなどの鍵は、シークレットに入れて「使用するシークレット」で付けてください。";

test("外部 API 呼び出し: 鍵の名前のヘッダーに値をそのまま書くと、保存は止めずに欄の下で知らせる（GAP-19）", async ({
  browser,
  page,
}) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  page.on("dialog", (d) => void d.accept());
  let id = "";
  try {
    const name = uniqueName("http-key-header");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [
            {
              id: "hook",
              type: "http.request",
              label: "受付を知らせる",
              params: { method: "POST", url: "https://hooks.example.com/notify", fail_on_error_status: true },
            },
          ],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    id = ((await created.json()) as { id: string }).id;

    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-hook").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await panel.getByRole("button", { name: "ヘッダーを追加" }).click();
    const row = panel.getByTestId("compose-row").nth(0);
    await row.getByRole("textbox", { name: "ヘッダーの名前" }).fill("Authorization");
    const warned = nextValidation(page, (body) => body.includes("sk-e2e-0123456789"));
    await row.getByRole("textbox", { name: "「Authorization」の値" }).fill("Bearer sk-e2e-0123456789");
    const body = (await (await warned).json()) as { warnings: { code: string; message: string }[] };
    const warning = body.warnings.find((w) => w.code === "ir.plain_key_header");
    expect(warning?.message).toBe(KEY_WARNING);
    expect(JSON.stringify(body.warnings)).not.toContain("sk-e2e");
    // 欄の下に出し（注記の代わり）、注意の帯には出さない。
    const fieldWarning = panel.locator("[data-field-warning]").filter({ hasText: KEY_WARNING });
    await expect(fieldWarning).toBeVisible();
    await expect(panel.getByText(KEY_WARNING, { exact: true })).toHaveCount(1);
    await fieldWarning.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/http-key-header-warning-pc.png` });

    // 保存は止めない（IR に書いたとおりに入る）。
    await page.getByRole("button", { name: "保存", exact: true }).click();
    await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
    const saved = (await (await page.request.get(`/api/workflows/${id}`)).json()) as {
      ir: { nodes: { id: string; params: Record<string, unknown> }[] };
    };
    expect(saved.ir.nodes.find((n) => n.id === "hook")?.params.headers).toEqual({
      Authorization: "Bearer sk-e2e-0123456789",
    });

    // 値を認証の方式の語だけにすると、注意は消える（鍵の値を IR に書いていない）。
    const cleared = nextValidation(page, (b) => !b.includes("sk-e2e-0123456789"));
    await row.getByRole("textbox", { name: "「Authorization」の値" }).fill("Bearer ");
    await cleared;
    await expect(fieldWarning).toHaveCount(0);
    await row.getByRole("textbox", { name: "「Authorization」の値" }).fill("Bearer sk-e2e-0123456789");
    await expect(fieldWarning).toBeVisible();

    // スマホの一覧から開いても、欄の下に注意が出て、横にはみ出さない。
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
      phone.on("dialog", (d) => void d.accept());
      await phone.goto(`/workflows/${id}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /受付を知らせる/ }).filter({ hasText: "受付を知らせる" }).first().click();
      const sheet = phone.getByRole("dialog");
      const shown = sheet.locator("[data-field-warning]").filter({ hasText: KEY_WARNING });
      await expect(shown).toBeVisible({ timeout: 15_000 });
      await shown.evaluate((el) => el.scrollIntoView({ block: "center" }));
      expect(await sheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/http-key-header-warning-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(async () => {
      if (id) await removeWorkflowApi(page, id);
    });
  }
});
