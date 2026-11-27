import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, uniqueName } from "./helpers";

/// ドライブのテキストを Shift_JIS のまま直す e2e（BUG-141・D-119）:
/// - 文字コードはサーバーが決める（以前はブラウザーの判定をもう 1 つ持っていた）。Shift_JIS のファイルは文字化け
///   せずに開き、見出しに「Shift_JIS」と出る。
/// - 編集の画面は、保存しても Shift_JIS のままで、Shift_JIS にない文字は保存できないことを言う。
/// - 直して保存すると、中身は Shift_JIS と改行コード（CRLF）のまま新しい版になる（以前は BOM の無い UTF-8 になった）。
/// - Shift_JIS にない文字（絵文字）を入れて保存すると、理由を言って断り、ファイルは変わらない。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

/// 「交通費の精算\r\n期限は月末\r\n」の Shift_JIS（CP932）。
const ORIGINAL_SJIS = "8cf092ca94ef82cc90b88e5a0d0a8afa8cc082cd8c8e96960d0a";
/// 末尾に「（改訂）」を足したものの Shift_JIS。
const EDITED_SJIS = "8cf092ca94ef82cc90b88e5a0d0a8afa8cc082cd8c8e96960d0a816989fc92f9816a";

test("ドライブ: Shift_JIS のテキストを直して保存すると Shift_JIS のまま新しい版になる（BUG-141）", async ({ browser, page }) => {
  test.setTimeout(180_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  // 保存していない変更を残して画面を移るときの確認（beforeunload）は、移る側を選ぶ。
  page.on("dialog", (d) => void d.accept());
  let folderId = "";
  let fileId = "";
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: uniqueName("e2e-文字コード"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;

    // ドライブのフォルダに Shift_JIS のテキストを置く（Windows のメモ帳や古いシステムの書き出し）。
    await page.goto(`/drive?folder=${folderId}`);
    const name = `${uniqueName("精算メモ")}.txt`;
    await page
      .locator('input[type="file"][multiple]')
      .setInputFiles({ name, mimeType: "text/plain", buffer: Buffer.from(ORIGINAL_SJIS, "hex") });
    await expect(page.getByText(name, { exact: true })).toBeVisible({ timeout: 20_000 });
    await expect
      .poll(
        async () => {
          const res = await page.request.get(`/api/nodes?parent_id=${folderId}`);
          const children = (await res.json()) as { items?: { id: string; name: string }[] };
          fileId = children.items?.find((c) => c.name === name)?.id ?? "";
          return fileId;
        },
        { timeout: 30_000 },
      )
      .not.toBe("");

    // 開くと文字化けせずに読め、見出しに文字コードが出る。
    await page.goto(`/file/${fileId}`);
    const preview = page.getByTestId("text-preview");
    await expect(preview.getByText("交通費の精算")).toBeVisible({ timeout: 20_000 });
    await expect(preview.getByText(/Shift_JIS/)).toBeVisible();
    await expect(preview.getByText(/CRLF/)).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-text-sjis-view-pc.png` });

    // 編集の画面は、Shift_JIS のまま保存することと、保存できない文字があることを言う。
    await page.getByTestId("text-edit").click();
    await expect(page.getByTestId("text-encoding-note")).toHaveText(
      "Shift_JIS のファイルです。保存しても Shift_JIS のままで、Shift_JIS にない文字（絵文字など）は保存できません。",
    );
    const editor = page.locator(".cm-content");
    await expect(editor).toBeVisible({ timeout: 20_000 });
    await editor.click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type("（改訂）");
    await page.getByTestId("text-save").click();
    await expect(page.getByText("保存しました").first()).toBeVisible({ timeout: 20_000 });
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-text-sjis-saved-pc.png` });

    // 保存した中身は Shift_JIS と CRLF のまま（BOM も付けない）。
    const text = (await (await page.request.get(`/api/files/${fileId}/text`)).json()) as {
      text: string;
      encoding: string | null;
      bom: boolean;
      version: number;
    };
    expect(text).toMatchObject({
      text: "交通費の精算\r\n期限は月末\r\n（改訂）",
      encoding: "Shift_JIS",
      bom: false,
    });
    const download = (await (await page.request.get(`/api/files/${fileId}/download-url`)).json()) as { url: string };
    const raw = await (await page.request.get(download.url)).body();
    expect(raw.toString("hex")).toBe(EDITED_SJIS);

    // Shift_JIS にない文字は保存できず、理由を言う（編集した内容は残り、ファイルは変わらない）。
    await editor.click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type("😀");
    await page.getByTestId("text-save").click();
    await expect(
      page.getByText("「😀」は、このファイルの文字コード（Shift_JIS）では保存できません。別の文字に置き換えてください。", {
        exact: true,
      }),
    ).toBeVisible({ timeout: 20_000 });
    await expect(editor).toContainText("😀");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/drive-text-sjis-refused-pc.png` });
    const after = (await (await page.request.get(`/api/files/${fileId}/text`)).json()) as { version: number };
    expect(after.version).toBe(text.version);

    // スマホでも同じように開けて、編集の注記が横にはみ出さない。
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
      await phone.goto(`/file/${fileId}`);
      const phonePreview = phone.getByTestId("text-preview");
      await expect(phonePreview.getByText("（改訂）")).toBeVisible({ timeout: 20_000 });
      await expect(phonePreview.getByText(/Shift_JIS/)).toBeVisible();
      await phone.getByTestId("text-edit").click();
      await expect(phone.getByTestId("text-encoding-note")).toBeVisible({ timeout: 20_000 });
      await expect(phone.locator(".cm-content")).toContainText("（改訂）", { timeout: 20_000 });
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/drive-text-sjis-edit-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      async () => {
        if (!fileId) return;
        const del = await page.request.delete(`/api/files/${fileId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
      async () => {
        if (!folderId) return;
        const del = await page.request.delete(`/api/folders/${folderId}`, { headers: await csrfHeaders(page) });
        expect(del.status(), await del.text()).toBeLessThan(300);
      },
    );
  }
});
