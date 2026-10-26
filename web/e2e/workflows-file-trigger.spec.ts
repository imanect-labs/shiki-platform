import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ファイル保存トリガの e2e（BUG-28・D-38）:
/// - きっかけの設定で、フォルダへの保存が新しい版を含むこと・続く保存を 1 回にまとめることを示す。
/// - 有効化したワークフローが、ドライブで保存した新しい版で起動する。同じファイルの保存が続いたら、途切れて
///   から最後の版で 1 回だけ起動する（run 入力の op は update・version は最後の版・parent_id はフォルダ）。
/// 前提: shiki-server が SHIKI__WORKFLOW__ENABLED=true・LLM は stub。まとめる待ち（60 秒）があるので長い。

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type RunInput = { op?: string; version?: number; file_id?: string; parent_id?: string };

test("ファイル保存トリガ: ドライブで新しい版を続けて保存すると、最後の版で 1 回だけ起動する（BUG-28・D-38）", async ({
  page,
}) => {
  test.setTimeout(240_000);
  await loginViaKeycloak(page);
  const folderName = uniqueName("e2e-版の受付");
  const created = await page.request.post("/api/folders", {
    headers: await csrfHeaders(page),
    data: { name: folderName, parent_id: null },
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  const folder = (await created.json()) as { id: string };

  const name = uniqueName("file-version-trigger");
  const res = await page.request.post("/api/workflows", {
    headers: await csrfHeaders(page),
    data: {
      ir: {
        ir_version: 1,
        name,
        display_name: name,
        declared_scopes: ["storage.read"],
        triggers: [{ kind: "event", source: "storage.write", scope: { folder: folder.id } }],
        nodes: [
          {
            id: "pick",
            type: "script.run",
            label: "版を受け取る",
            params: { source: { inline: "function main(input) { return { version: input.version }; }" } },
          },
        ],
        edges: [],
      },
    },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
  const id = ((await res.json()) as { id: string }).id;

  // きっかけの設定に、新しい版も含むこと・続く保存をまとめることが出る。
  await page.goto(`/workflows/${id}`);
  const canvas = page.locator(".react-flow");
  await expect(canvas.getByText("版を受け取る").first()).toBeVisible({ timeout: 15_000 });
  await canvas.getByText("ファイルが保存されたとき").first().click();
  const triggerPanel = page.getByRole("complementary", { name: "きっかけの設定" });
  await expect(triggerPanel.getByText("（新しい版を含む）", { exact: false })).toBeVisible();
  await expect(triggerPanel.getByText("同じファイルの続く保存は 1 回にまとめ", { exact: false })).toBeVisible();

  // 有効化（トリガのフォルダの閲覧を委譲する。委譲が無いと保存されたファイルを読めず起動しない・D-50）。
  const enabled = await page.request.post(`/api/workflows/${id}/enable`, {
    headers: await csrfHeaders(page),
    data: {
      version: 1,
      grants: [{ scope: "storage.read", object_type: "folder", object_id: folder.id, relation: "viewer" }],
    },
  });
  expect(enabled.ok(), await enabled.text()).toBeTruthy();

  // ドライブのフォルダにファイルを置き、行メニューの「新しいバージョン」で 2 回続けて版を重ねる。
  await page.goto(`/drive?folder=${folder.id}`);
  const fileName = `${uniqueName("議事録")}.txt`;
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "text/plain",
    buffer: Buffer.from("v1\n"),
  });
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });
  for (const body of ["v2 追記\n", "v3 さらに追記\n"]) {
    await page.getByRole("button", { name: `「${fileName}」の操作` }).click();
    const [chooser] = await Promise.all([
      page.waitForEvent("filechooser"),
      page.getByRole("menuitem", { name: "新しいバージョン" }).click(),
    ]);
    await chooser.setFiles({ name: fileName, mimeType: "text/plain", buffer: Buffer.from(body) });
    // トースト本体＋aria-live で 2 要素マッチするため first（drive-versions と同じ）。
    await expect(page.getByText("新しいバージョンをアップロードしました").first()).toBeVisible({
      timeout: 20_000,
    });
  }

  // 新しい版の保存は、同じファイルの保存が途切れてから（60 秒＋ relay の間隔）最後の版で 1 回だけ起動する。
  const updateRuns = async (): Promise<RunInput[]> => {
    const list = (await (await page.request.get(`/api/workflows/${id}/runs`)).json()) as {
      items: { run_id: string; trigger_kind: string }[];
    };
    const inputs: RunInput[] = [];
    for (const r of list.items.filter((r) => r.trigger_kind === "event")) {
      const detail = (await (await page.request.get(`/api/workflows/${id}/runs/${r.run_id}`)).json()) as {
        input: RunInput;
      };
      if (detail.input.op === "update") inputs.push(detail.input);
    }
    return inputs;
  };
  await expect
    .poll(async () => (await updateRuns()).length, { timeout: 150_000, intervals: [5_000] })
    .toBeGreaterThan(0);
  const runs = await updateRuns();
  expect(runs).toHaveLength(1);
  expect(runs[0].version).toBe(3);
  expect(runs[0].parent_id).toBe(folder.id);
});
