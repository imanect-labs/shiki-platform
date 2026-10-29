import { expect, test, type Page } from "@playwright/test";

import { cleanupAll, loginViaKeycloak, removeWorkflowApi, uniqueName } from "./helpers";

/// 「ファイルを読む」（storage.read）で PDF の本文を読む e2e（D-111・BUG-101 の残り）:
/// PDF・Word は、社内文書の検索に取り込まれた本文を読む（原本は読み直さない）。「読むファイル」の注記はそのことと、
/// 取り込みが終わるまで読めないことを言い、「詳しく」で本文の字数の上限（Rust の定数から生成した値）と、
/// 取り込みを待つ組み方を言う。
///
/// 社内文書の検索が有効な構成（dev-up.sh --rag。取り込みの worker が要る）では、PDF を上げて取り込みを待ち、
/// 「ファイルを読む」の結果に本文が入ることを確かめる。無効な構成（CI の web-e2e と、手元の検証のスタック・
/// SHIKI__RAG__ENABLED=false）では取り込みが走らないので、PDF は「社内文書の検索が使えない」分かる誤り（やり直さない）で
/// 失敗し、実行履歴に理由が出ることを確かめる（取り込み済みの本文を読む経路は、api の結合テスト
/// workflow_document_read_it が取り込みの行を積んで確かめる）。

const SHOTS = process.env.E2E_SHOTS_DIR;

async function csrfHeaders(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

const HINT =
  "テキストのファイル（.txt、.md、.csv など）は中身を、PDF や Word は社内文書の検索に取り込まれた本文を読みます。PDF や Word は、取り込みが終わるまで読めません。";
/// 「詳しく」の本文の字数の上限（STORAGE_READ_MAX_DOCUMENT_CHARS = 100,000）。
const LIMIT = "PDF や Word の本文は、先頭から 100,000 字までを読みます。";
const NO_SEARCH = "この環境では社内文書の検索が使えないため、PDF や Word の本文を読めません。";

/// 1 ページに `text`（ASCII）を書いた PDF（Helvetica・取り込みの worker が本文を取り出せる最小の形）。
function textPdf(text: string): Buffer {
  const content = `BT /F1 18 Tf 72 720 Td (${text}) Tj ET`;
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    `<< /Length ${content.length} >>\nstream\n${content}\nendstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  let body = "%PDF-1.4\n";
  const offsets: number[] = [];
  objects.forEach((o, i) => {
    offsets.push(body.length);
    body += `${i + 1} 0 obj\n${o}\nendobj\n`;
  });
  const xref = body.length;
  body += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const off of offsets) body += `${String(off).padStart(10, "0")} 00000 n \n`;
  body += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(body, "latin1");
}

type RunDetail = {
  status: string;
  steps: { node_id: string; status: string; error?: { code?: string; message?: string; retryable?: boolean } | null }[];
  failure?: { category: string; message: string } | null;
  resume?: { advice: string; offer: boolean; note: string } | null;
};

test("ファイルを読む: PDF は社内文書の検索に取り込まれた本文を読む（D-111）", async ({ browser, page }) => {
  test.setTimeout(300_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  await loginViaKeycloak(page);
  const probe = await page.request.post("/api/search", { headers: await csrfHeaders(page), data: { query: "probe" } });
  const rag = probe.status() !== 503;
  test.info().annotations.push({
    type: "社内文書の検索",
    description: rag ? "有効: 取り込みを待って本文を読む" : "無効: 取り込みが走らない構成なので、分かる誤りを確かめる",
  });
  let folderId = "";
  let id = "";
  let pdfId = "";
  try {
    const folder = await page.request.post("/api/folders", {
      headers: await csrfHeaders(page),
      data: { name: uniqueName("e2e-文書を読む"), parent_id: null },
    });
    expect(folder.ok(), await folder.text()).toBeTruthy();
    folderId = ((await folder.json()) as { id: string }).id;

    // ドライブの画面から PDF を上げる。本文の目印は、ほかの文書と混ざらない英数字にする。
    const token = `shiki${Date.now().toString(36)}`;
    await page.goto(`/drive?folder=${folderId}`);
    const pdfName = `${uniqueName("請求書")}.pdf`;
    await page.locator('input[type="file"][multiple]').setInputFiles({
      name: pdfName,
      mimeType: "application/pdf",
      buffer: textPdf(`Invoice ${token} total 12000 yen`),
    });
    await expect(page.getByText(pdfName, { exact: true })).toBeVisible({ timeout: 20_000 });
    await expect
      .poll(
        async () => {
          const res = await page.request.get(`/api/nodes?parent_id=${folderId}`);
          const children = (await res.json()) as { items?: { id: string; name: string }[] };
          pdfId = children.items?.find((c) => c.name === pdfName)?.id ?? "";
          return pdfId;
        },
        { timeout: 30_000 },
      )
      .not.toBe("");

    const name = uniqueName("read-document");
    const created = await page.request.post("/api/workflows", {
      headers: await csrfHeaders(page),
      data: {
        ir: {
          ir_version: 1,
          name,
          display_name: name,
          triggers: [{ kind: "interactive" }],
          nodes: [{ id: "rd", type: "storage.read", label: "請求書を読む", params: { file: pdfId } }],
          edges: [],
        },
      },
    });
    expect(created.ok(), await created.text()).toBeTruthy();
    id = ((await created.json()) as { id: string }).id;

    // 「読むファイル」の注記と「詳しく」（本文の字数の上限・取り込みを待つ組み方・読める種類）。
    await page.goto(`/workflows/${id}`);
    await page.getByTestId("rf__node-rd").click();
    const panel = page.getByRole("complementary", { name: "ブロックの設定" });
    await expect(panel.getByText(HINT)).toBeVisible();
    await expect(panel.getByText(pdfName)).toBeVisible({ timeout: 15_000 });
    const more = panel.getByTestId("storage-read-more");
    await more.getByRole("button", { name: "詳しく" }).click();
    await expect(more.getByText(LIMIT)).toBeVisible();
    await expect(more.getByText(/「リトライ回数」を増やすと、少し待ってから読み直します。/)).toBeVisible();
    await expect(more.getByText(/画像や古い形式（.doc、.xls）は読めません。/)).toBeVisible();
    await more.scrollIntoViewIfNeeded();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/storage-read-document-hint-pc.png` });

    // 社内文書の検索が有効なら、取り込みが終わる（検索に出る）まで待ってから読む。
    if (rag) {
      await expect
        .poll(
          async () => {
            const res = await page.request.post("/api/search", {
              headers: await csrfHeaders(page),
              data: { query: token },
            });
            if (!res.ok()) return false;
            const body = (await res.json()) as { results?: { file_id?: string }[] };
            return (body.results ?? []).some((r) => r.file_id === pdfId);
          },
          { timeout: 180_000, intervals: [3_000] },
        )
        .toBe(true);
    }

    const started = await page.request.post(`/api/workflows/${id}/runs`, {
      headers: await csrfHeaders(page),
      data: { input: {} },
    });
    expect(started.ok(), await started.text()).toBeTruthy();
    const runId = ((await started.json()) as { run_id: string }).run_id;
    const detail = async () =>
      (await (await page.request.get(`/api/workflows/${id}/runs/${runId}`)).json()) as RunDetail;
    await expect
      .poll(async () => (await detail()).status, { timeout: 60_000, intervals: [1_000] })
      .toMatch(/^(succeeded|failed)$/);
    const finished = await detail();

    await page.goto(`/workflows/${id}/runs?run=${runId}`);
    const sheet = page.getByRole("dialog");
    if (rag) {
      // 取り込まれた本文が結果に入る（文字化けした原本の中身ではない・切っていない・文字コードは無い）。
      expect(finished.status, JSON.stringify(finished.steps)).toBe("succeeded");
      const step = (await (await page.request.get(`/api/workflows/${id}/runs/${runId}/steps?path=rd`)).json()) as {
        output?: { text?: string; truncated?: boolean; encoding?: string; name?: string };
      };
      expect(step.output?.text).toContain(token);
      expect(step.output?.text).not.toContain("%PDF");
      expect(step.output?.truncated).toBe(false);
      expect(step.output?.encoding).toBeUndefined();
      expect(step.output?.name).toBe(pdfName);
      await sheet.getByRole("button", { name: /請求書を読む/ }).click();
      await expect(sheet.getByText(new RegExp(token)).first()).toBeVisible({ timeout: 20_000 });
    } else {
      // 社内文書の検索が使えない環境では、分かる誤り（やり直さない）で失敗し、原因を直してから再開する案内が出る。
      expect(finished.status).toBe("failed");
      const step = finished.steps.find((s) => s.node_id === "rd");
      expect(step?.error).toMatchObject({ code: "not_text_file", message: NO_SEARCH, retryable: false });
      expect(finished.failure?.category).toBe("configuration");
      expect(finished.resume).toMatchObject({ advice: "fix_first", offer: true });
      await expect(sheet.getByText(NO_SEARCH).first()).toBeVisible({ timeout: 30_000 });
      await expect(sheet.getByTestId("run-failure")).toHaveText(finished.failure!.message);
    }
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/storage-read-document-run-pc.png` });

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
      // スマホの一覧から「請求書を読む」を開いても、同じ注記と「詳しく」が横にはみ出さずに出る。
      await phone.goto(`/workflows/${id}`);
      const flow = phone.getByTestId("workflow-mobile-flow");
      await expect(flow).toBeVisible({ timeout: 15_000 });
      await flow.getByRole("button", { name: /請求書を読む/ }).filter({ hasText: "請求書を読む" }).first().click();
      const phoneSheet = phone.getByRole("dialog");
      await expect(phoneSheet.getByText(HINT)).toBeVisible();
      const phoneMore = phoneSheet.getByTestId("storage-read-more");
      await phoneMore.getByRole("button", { name: "詳しく" }).click();
      await expect(phoneMore.getByText(LIMIT)).toBeVisible();
      await phoneMore.scrollIntoViewIfNeeded();
      expect(await phoneSheet.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/storage-read-document-hint-phone.png` });

      await phone.goto(`/workflows/${id}/runs?run=${runId}`);
      const runSheet = phone.getByRole("dialog");
      if (rag) await runSheet.getByRole("button", { name: /請求書を読む/ }).click();
      await expect(runSheet.getByText(rag ? new RegExp(token) : NO_SEARCH).first()).toBeVisible({ timeout: 30_000 });
      expect(await phone.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
      if (SHOTS) await phone.screenshot({ path: `${SHOTS}/storage-read-document-run-phone.png` });
    } finally {
      await phoneCtx.close();
    }
  } finally {
    await cleanupAll(
      async () => {
        if (id) await removeWorkflowApi(page, id);
      },
      async () => {
        if (!pdfId) return;
        const del = await page.request.delete(`/api/files/${pdfId}`, { headers: await csrfHeaders(page) });
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
