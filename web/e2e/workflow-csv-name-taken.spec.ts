import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// 「CSV を保存」で同じ名前のファイルがあるときの e2e（BUG-97・D-124）:
/// - 「同じ名前があるとき」が 番号を付けて保存 なら、2 回目は「名前 (2).csv」で保存する。番号は「ファイルを保存」・
///   ドライブの新規作成と同じ 1 か所（StorageService）が付けるので、同じフォルダーの「ファイルを保存」は続きの番号に
///   なる。選んだ文字コード（UTF-8（BOM 付き））は番号を付けても変わらない（D-120）。
/// - 欄が無い既存の定義は今のまま失敗にし、失敗の理由は .csv を付けた名前の「同じ名前のファイル」と言う。

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

type RunDetail = {
  status: string;
  steps: { node_id: string; status: string; error?: { code?: string; message?: string } | null }[];
};

const CSV = "品目,金額\n交通費,1200\n";

test("CSV を保存: 同じ名前があるときは番号を付け、欄の無い定義は今のまま失敗する（D-124）", async ({ page }) => {
  test.setTimeout(240_000);
  await loginViaKeycloak(page);
  let folderId = "";
  const workflowIds: string[] = [];
  const fileIds = new Set<string>();
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: uniqueName("e2e-CSV の同じ名前"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;
    const base = uniqueName("発注書");

    const create = async (type: string, params: Record<string, unknown>): Promise<string> => {
      const name = uniqueName("csv-name");
      const res = await page.request.post("/api/workflows", {
        headers: await csrfHeaders(page),
        data: {
          ir: {
            ir_version: 1,
            name,
            display_name: name,
            triggers: [{ kind: "interactive" }],
            nodes: [{ id: "save", type, label: "発注書を保存", params: { folder: folderId, content: CSV, ...params } }],
            edges: [],
          },
        },
      });
      expect(res.ok(), await res.text()).toBeTruthy();
      const id = ((await res.json()) as { id: string }).id;
      workflowIds.push(id);
      return id;
    };
    const numbered = await create("csv.write", { name: base, on_name_taken: "add_number", encoding: "utf8_bom" });
    const legacy = await create("csv.write", { name: base });
    const file = await create("storage.write", { name: `${base}.csv`, on_name_taken: "add_number" });

    const run = async (id: string): Promise<{ runId: string; detail: RunDetail }> => {
      const started = await page.request.post(`/api/workflows/${id}/runs`, {
        headers: await csrfHeaders(page),
        data: { input: {} },
      });
      expect(started.ok(), await started.text()).toBeTruthy();
      const runId = ((await started.json()) as { run_id: string }).run_id;
      const get = async () => (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as RunDetail;
      await expect
        .poll(async () => (await get()).status, { timeout: 60_000, intervals: [1_000] })
        .toMatch(/^(succeeded|failed)$/);
      return { runId, detail: await get() };
    };
    const output = async (id: string, runId: string) => {
      const res = await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=save`);
      const out = ((await res.json()) as { output: { file_id: string; name: string } }).output;
      fileIds.add(out.file_id);
      return out;
    };

    // 1 回目は .csv を付けた名前、2 回目は番号を付けた名前。どちらも選んだ文字コードのまま。
    const first = await run(numbered);
    expect(first.detail.status).toBe("succeeded");
    const firstOut = await output(numbered, first.runId);
    expect(firstOut.name).toBe(`${base}.csv`);
    const second = await run(numbered);
    expect(second.detail.status).toBe("succeeded");
    const secondOut = await output(numbered, second.runId);
    expect(secondOut.name).toBe(`${base} (2).csv`);
    const text = (await (await page.request.get(`/api/files/${secondOut.file_id}/text`)).json()) as {
      text: string;
      encoding: string;
      bom: boolean;
    };
    expect(text).toMatchObject({ text: CSV, encoding: "UTF-8", bom: true });

    // 「ファイルを保存」も同じ規則で続きの番号を付ける（番号の規則は 1 か所）。
    const third = await run(file);
    expect(third.detail.status).toBe("succeeded");
    expect((await output(file, third.runId)).name).toBe(`${base} (3).csv`);

    // 欄の無い既存の定義は失敗する。理由は .csv を付けた名前の同じ名前のファイル。
    const failed = await run(legacy);
    expect(failed.detail.status).toBe("failed");
    const step = failed.detail.steps.find((s) => s.node_id === "save");
    expect(step?.error?.code).toBe("name_taken");
    expect(step?.error?.message).toContain(`同じ名前のファイル「${base}.csv」がすでにあります`);
  } finally {
    await cleanupAll(
      ...[...fileIds].map((id) => async () => {
        if (!id) return;
        const del = await page.request.delete(`/api/files/${id}`, { headers: await csrfHeaders(page) });
        expect([204, 404], `DELETE /files/${id}`).toContain(del.status());
      }),
      ...workflowIds.map((id) => async () => {
        const del = await page.request.delete(`/api/artifacts/${id}`, { headers: await csrfHeaders(page) });
        expect([204, 404], `DELETE /artifacts/${id}`).toContain(del.status());
      }),
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, { headers: await csrfHeaders(page) });
        expect([204, 404], `DELETE /folders/${folderId}`).toContain(del.status());
      },
    );
  }
});
