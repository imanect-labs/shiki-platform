import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 有効化・同意フローの e2e（Task 10.4a/10.12 DoD）:
/// スケジュールトリガを UI で設定 → 保存 → 同意ダイアログ（日本語スコープ説明）→
/// 有効化 → バッジ表示 → 停止 → バッジ解除。

/// 作ったワークフローを、権限も取り消してオフにしてから消す（片付け）。
async function removeFlow(page: Page, id: string) {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const headers = { "x-csrf-token": csrf, "content-type": "application/json" };
  const off = await page.request.post(`/api/workflows/${id}/disable`, { headers, data: { revoke_delegations: true } });
  expect(off.status(), await off.text()).toBeLessThan(300);
  const del = await page.request.delete(`/api/artifacts/${id}`, { headers });
  expect(del.status(), await del.text()).toBeLessThan(300);
}

/// 作ったドライブの項目（フォルダー・ファイル）を消す（片付け）。
async function removeDriveItem(page: Page, kind: "folders" | "files", id: string) {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  const del = await page.request.delete(`/api/${kind}/${id}`, { headers: { "x-csrf-token": csrf } });
  expect(del.status(), await del.text()).toBeLessThan(300);
}

/// 試験で作ったものの片付け（作った逆の順に、互いに独立して走らせる・失敗した試験でも走る）。
let created: (() => Promise<void>)[] = [];
test.afterEach(async () => {
  const steps = created.reverse();
  created = [];
  await cleanupAll(...steps);
});

/// BFF 経由でワークフローを作る（エディタ検証は workflows-editor.spec に集約）。
async function createFlow(page: Page, name: string): Promise<string> {
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        // display_name も一意にする（開発 DB では過去実行の同名フローが残り strict 違反になる）。
        name,
        display_name: name,
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
    },
  });
  expect(res.ok()).toBeTruthy();
  const body = (await res.json()) as { id: string };
  return body.id;
}

test("有効化: スケジュール設定→同意→有効→停止", async ({ page }) => {
  test.setTimeout(180_000);
  await loginViaKeycloak(page);
  const name = uniqueName("enable-e2e");
  const id = await createFlow(page, name);
  created.push(() => removeFlow(page, id));
  await page.goto(`/workflows/${id}`);

  // トリガ（きっかけ）ノードをクリック → 設定パネルでスケジュールに切替。
  const canvas = page.locator(".react-flow");
  await canvas.getByText("手動で実行").first().click();
  await page
    .getByRole("complementary", { name: "きっかけの設定" })
    .getByRole("combobox", { name: "きっかけの種類" })
    .click();
  await page.getByRole("option", { name: "スケジュール" }).click();
  // cron プリセット（毎日 9:00 既定）の次の実行が出る（サーバーが発火と同じ読み方で数える・BUG-95）。
  await expect(page.getByText(/^次の実行（日本時間）:/)).toBeVisible({ timeout: 10_000 });

  // 保存してから有効化（dirty 中は有効化できないガードの裏取り）。
  const enableButton = page.getByRole("button", { name: "自動実行を有効化" });
  await expect(enableButton).toBeDisabled();
  await page.getByRole("button", { name: "保存", exact: true }).click();
  // toast は aria-live 領域にも複製されるため first() で strict 違反を避ける。
  await expect(page.getByText(/保存しました/).first()).toBeVisible({ timeout: 15_000 });
  await enableButton.click();

  // 同意ダイアログ: 何ができるかの日本語説明 → 同意してオンにする。
  const dialog = page.locator('[role="dialog"]');
  await expect(dialog.getByText("自動実行の設定")).toBeVisible();
  await expect(dialog.getByText("このワークフローができること")).toBeVisible();
  await dialog.getByRole("button", { name: "同意してオンにする" }).click();
  await expect(page.getByText(/自動実行をオンにしました/).first()).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(/自動実行 有効/)).toBeVisible({ timeout: 10_000 });

  // 一覧にも有効バッジが出る。
  await page.goto("/workflows");
  // 行ボタンに限定する（削除ボタンの aria-label「「<name>」を削除」も名前を含むため先頭一致）。
  const row = page.getByRole("button", { name: new RegExp(`^${name}`) });
  await expect(row.getByText("有効", { exact: true })).toBeVisible();

  // 停止: 設定ダイアログからオフにする（確認でオフにする）→ バッジが消える。
  await row.click();
  await page.waitForURL(/\/workflows\/[0-9a-f-]+$/i);
  await page.getByRole("button", { name: "自動実行の設定" }).click();
  await page
    .locator('[role="dialog"]')
    .getByRole("button", { name: "自動実行をオフにする" })
    .click();
  await page.getByTestId("enable-off-confirm").getByRole("button", { name: "オフにする", exact: true }).click();
  await expect(page.getByText(/自動実行をオフにしました/).first()).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(/自動実行 有効/)).toHaveCount(0);
});

test("有効化: 実行の入力でテーブルを決めるブロックはテーブルを選ばせず、スケジュールでは動かないことを示す（C-P2-2）", async ({ page }) => {
  await loginViaKeycloak(page);
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const name = uniqueName("enable-input-table");
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: ["data.read"],
        triggers: [{ kind: "schedule", cron: "0 9 * * *", tz: "Asia/Tokyo", catchup: "skip" }],
        nodes: [
          { id: "q", type: "data.query", label: "選んだ表を調べる", params: { table: { $from: "input", path: "/table" }, query: { limit: 5 } } },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  created.push(() => removeFlow(page, id));
  await page.goto(`/workflows/${id}`);
  await page.getByRole("button", { name: /自動実行を有効化/ }).click();
  const dialog = page.locator('[role="dialog"]');
  await expect(dialog.getByText("自動実行の設定")).toBeVisible();
  // テーブルの選択は出さず、理由（入力で決まる・スケジュールでは入力が無い）を示す。
  await expect(dialog.getByText(/「選んだ表を調べる」のテーブルは実行の入力で決まるため/)).toBeVisible();
  await expect(dialog.getByText(/スケジュールの実行には入力が無いので/)).toBeVisible();
  await expect(dialog.getByLabel("渡すテーブル")).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "同意してオンにする" })).toBeEnabled();
});

test("有効化: 実行の入力でファイルを決めるブロックはファイルを選ばせず、固定のファイルは名前で示す", async ({ page }) => {
  await loginViaKeycloak(page);
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const fileName = `${uniqueName("wf-enable-file")}.csv`;
  const file = await page.request.post("/api/tabular/save", {
    headers: { "x-csrf-token": csrf },
    data: { parent_id: null, name: fileName, csv: "a,b\n1,2\n" },
  });
  expect(file.ok(), await file.text()).toBeTruthy();
  const fileId = ((await file.json()) as { node_id: string }).node_id;
  created.push(() => removeDriveItem(page, "files", fileId));
  const name = uniqueName("enable-input-file");
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: ["storage.read"],
        triggers: [{ kind: "schedule", cron: "0 9 * * *", tz: "Asia/Tokyo", catchup: "skip" }],
        nodes: [
          { id: "r1", type: "storage.read", label: "受け取ったファイルを読む", params: { file: { $from: "input", path: "/file_id" } } },
          { id: "r2", type: "storage.read", label: "台帳を読む", params: { file: fileId } },
        ],
        edges: [{ from: "r1", from_port: "out", to: "r2" }],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  created.push(() => removeFlow(page, id));
  await page.goto(`/workflows/${id}`);
  await page.getByRole("button", { name: /自動実行を有効化/ }).click();
  const dialog = page.locator('[role="dialog"]');
  await expect(dialog.getByText("自動実行の設定")).toBeVisible();
  // 入力で決まるファイルは選ばせず、理由を示す（「対象を選択してください」で止まらない）。
  await expect(dialog.getByText(/「受け取ったファイルを読む」のファイルは実行の入力で決まるため/)).toBeVisible();
  await expect(dialog.getByRole("button", { name: /渡すファイルを/ })).toHaveCount(0);
  // 固定のファイルは ID ではなく名前で出る。
  await expect(dialog.getByText(`ファイル「${fileName}」`)).toBeVisible({ timeout: 10_000 });
  await expect(dialog.getByRole("button", { name: "同意してオンにする" })).toBeEnabled();
});

test("有効化: 集計でつなぐ CSV を実行の入力で決めるブロックは、そのファイルを選ばせない（#1037・C-P2-2）", async ({ page }) => {
  test.setTimeout(120_000);
  await loginViaKeycloak(page);
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const fileName = `${uniqueName("wf-enable-join")}.csv`;
  const file = await page.request.post("/api/tabular/save", {
    headers: { "x-csrf-token": csrf },
    data: { parent_id: null, name: fileName, csv: "品目,数量\nりんご,3\n" },
  });
  expect(file.ok(), await file.text()).toBeTruthy();
  const fileId = ((await file.json()) as { node_id: string }).node_id;
  created.push(() => removeDriveItem(page, "files", fileId));
  // 集計するファイルは固定で、つなぐ CSV の 1 件のファイル・つなぐ CSV の並びそのものを実行の入力で決める。
  const shapes = [
    { label: "受け取った単価とつなぐ", tables: [{ name: "master", file: { $from: "input", path: "/master" } }] },
    { label: "受け取った表とつなぐ", tables: { $from: "input", path: "/tables" } },
  ];
  for (const shape of shapes) {
    const name = uniqueName("enable-input-join");
    const res = await page.request.post("/api/workflows", {
      headers: { "x-csrf-token": csrf, "content-type": "application/json" },
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          declared_scopes: ["csv.read"],
          triggers: [{ kind: "schedule", cron: "0 9 * * *", tz: "Asia/Tokyo", catchup: "skip" }],
          nodes: [
            {
              id: "q",
              type: "csv.query",
              label: shape.label,
              params: { file: fileId, sql: "SELECT * FROM data", tables: shape.tables },
            },
          ],
          edges: [],
        },
      },
    });
    expect(res.ok(), await res.text()).toBeTruthy();
    const id = ((await res.json()) as { id: string }).id;
    created.push(() => removeFlow(page, id));
    await page.goto(`/workflows/${id}`);
    await page.getByRole("button", { name: /自動実行を有効化/ }).click();
    const dialog = page.locator('[role="dialog"]');
    await expect(dialog.getByText("自動実行の設定")).toBeVisible();
    // 入力で決まるファイルは選ばせず、理由を示す（関係のないファイルを選ぶまで止めない）。
    await expect(dialog.getByText(new RegExp(`「${shape.label}」のファイルは実行の入力で決まるため`))).toBeVisible();
    await expect(dialog.getByRole("button", { name: /渡すファイルを/ })).toHaveCount(0);
    // 固定のファイルは名前で出る。
    await expect(dialog.getByText(`ファイル「${fileName}」`)).toBeVisible({ timeout: 10_000 });
    await expect(dialog.getByRole("button", { name: "同意してオンにする" })).toBeEnabled();
  }
});

/// 保存は通るが、そのままでは動かせない設定（D-40・D-43）: 必須の文字の欄が空のブロックと、スケジュールで動くのに
/// 保存先のフォルダを省いた書込。有効化のダイアログは理由をブロックの名前つきで示し、有効化のボタンを押せない。
/// サーバの有効化も同じ理由で 409 `not_ready` を返す。
test("有効化: 指示が空のブロックと保存先を省いた書込があると、理由を示して有効化させない（D-40・D-43）", async ({ page }) => {
  await loginViaKeycloak(page);
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const name = uniqueName("enable-not-ready");
  const res = await page.request.post("/api/workflows", {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        triggers: [{ kind: "schedule", cron: "0 9 * * *", tz: "Asia/Tokyo", catchup: "skip" }],
        nodes: [
          { id: "ask", type: "llm.invoke", label: "日報をまとめる", params: { prompt: "" } },
          {
            id: "save",
            type: "storage.write",
            label: "まとめを保存",
            params: { name: "日報.md", content: { $from: "nodes.ask.output", path: "/text" } },
          },
        ],
        edges: [{ from: "ask", from_port: "out", to: "save" }],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const saved = (await res.json()) as { id: string; warnings: { code: string; path?: string }[] };
  created.push(() => removeFlow(page, saved.id));
  // 保存は止めず、空の欄は path つきの警告で知らせる。
  expect(saved.warnings).toEqual(
    expect.arrayContaining([expect.objectContaining({ code: "ir.required_text_empty", path: "/params/prompt" })]),
  );

  await page.goto(`/workflows/${saved.id}`);
  await page.getByRole("button", { name: /自動実行を有効化/ }).click();
  const dialog = page.locator('[role="dialog"]');
  await expect(dialog.getByText("自動実行の設定")).toBeVisible();
  const blockers = dialog.getByTestId("enable-blockers");
  await expect(blockers).toContainText("このままでは有効化できません");
  await expect(blockers).toContainText("「日報をまとめる」「AI への指示」が空です");
  await expect(blockers).toContainText("「まとめを保存」保存先のフォルダを指定してください");
  await expect(dialog.getByRole("button", { name: "設定を直してください（2 件）" })).toBeDisabled();

  // 画面を通さずに有効化しても、同じ理由で止まる（有効化は記録されない）。
  const enable = await page.request.post(`/api/workflows/${saved.id}/enable`, {
    headers: { "x-csrf-token": csrf, "content-type": "application/json" },
    data: { version: 1, grants: [] },
  });
  expect(enable.status()).toBe(409);
  const body = (await enable.json()) as { code: string; errors: { code: string; node_id: string }[] };
  expect(body.code).toBe("not_ready");
  expect(body.errors.map((e) => [e.code, e.node_id])).toEqual([
    ["ir.required_text_empty", "ask"],
    ["ir.write_folder_required", "save"],
  ]);
  const reg = await page.request.get(`/api/workflows/${saved.id}/registration`);
  expect(((await reg.json()) as { status: string }).status).toBe("none");

  // 有効化する人のアカウントでは使えないモデルの理由（D-196）は、エディタでは直せないので別の見出しで言う。
  // 制限は起動の設定でしか付けられないので、同意の計画の応答に理由を 1 つ足して確かめる（文はサーバーの文と同じ形）。
  await page.route(`**/api/workflows/${saved.id}/versions/1/consent-plan`, async (route) => {
    const res = await route.fetch();
    const plan = (await res.json()) as { blockers: Record<string, unknown>[] };
    plan.blockers.push({
      code: "ir.model_unavailable_for_user",
      message:
        "このスキル（tidy@1）はあなたがインストールしていないため、組織の既定のモデル（default）で動くかを確かめられません。このモデルはあなたのアカウントでは使えないため、自動実行をオンにできません。スキルをインストールするか、管理者に相談してください。自動実行は、このモデルを使える人にオンにしてもらうこともできます",
      node_id: "ask",
      edge: null,
      path: "/params/skill",
    });
    await route.fulfill({ response: res, json: plan });
  });
  await page.reload();
  await page.getByRole("button", { name: /自動実行を有効化/ }).click();
  await expect(dialog.getByText("自動実行の設定")).toBeVisible();
  const settings = dialog.getByTestId("enable-blockers-settings");
  const account = dialog.getByTestId("enable-blockers-account");
  await expect(settings).toContainText("エディタで次の設定を直してから、もう一度開いてください。");
  await expect(settings).toContainText("「日報をまとめる」「AI への指示」が空です");
  await expect(settings).not.toContainText("インストールしていない");
  await expect(account).toContainText("あなたのアカウントでは使えないモデルで自動実行されるため、有効化できません。");
  await expect(account).toContainText("「日報をまとめる」このスキル（tidy@1）はあなたがインストールしていないため");
  await expect(account).not.toContainText("エディタで");
  await expect(dialog.getByRole("button", { name: "有効化できません（3 件）" })).toBeDisabled();
  if (process.env.E2E_SHOTS_DIR) {
    await dialog.screenshot({ path: `${process.env.E2E_SHOTS_DIR}/enable-blockers-account.png` });
  }
  await page.unrouteAll({ behavior: "ignoreErrors" });
});

/// ファイル保存のきっかけで決まるファイル（`$from: trigger`）は「前のブロックの結果」ではなく、きっかけの
/// フォルダに渡す閲覧の権限で読める（BUG-38）。ファイルを選ばせない。
test("有効化: きっかけで保存されたファイルを読むブロックは、きっかけのフォルダの権限で読めると示す（BUG-38）", async ({ page }) => {
  await loginViaKeycloak(page);
  const cookies = await page.context().cookies();
  const csrf = cookies.find((c) => c.name === "shiki_csrf")?.value ?? "";
  const headers = { "x-csrf-token": csrf, "content-type": "application/json" };
  const folder = await page.request.post("/api/folders", {
    headers,
    data: { name: uniqueName("e2e-受付の箱"), parent_id: null },
  });
  expect(folder.ok(), await folder.text()).toBeTruthy();
  const folderId = ((await folder.json()) as { id: string }).id;
  created.push(() => removeDriveItem(page, "folders", folderId));
  const name = uniqueName("enable-trigger-file");
  const res = await page.request.post("/api/workflows", {
    headers,
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: ["storage.read"],
        triggers: [{ kind: "event", source: "storage.write", scope: { folder: folderId } }],
        nodes: [
          {
            id: "read",
            type: "storage.read",
            label: "届いたファイルを読む",
            params: { file: { $from: "trigger", path: "/file_id" } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;
  created.push(() => removeFlow(page, id));
  await page.goto(`/workflows/${id}`);
  await page.getByRole("button", { name: /自動実行を有効化/ }).click();
  const dialog = page.locator('[role="dialog"]');
  await expect(dialog.getByText("自動実行の設定")).toBeVisible();
  await expect(
    dialog.getByText(
      "「届いたファイルを読む」のファイルはきっかけ（保存されたファイル）で決まるため、ここでは選びません。きっかけのフォルダに渡す閲覧の権限で読めます。",
    ),
  ).toBeVisible();
  await expect(dialog.getByText(/前のブロックの結果で決まる/)).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: /渡すファイルを/ })).toHaveCount(0);
  await expect(dialog.getByTestId("enable-blockers")).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "同意してオンにする" })).toBeEnabled();
});
