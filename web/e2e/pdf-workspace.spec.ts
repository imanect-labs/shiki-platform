import {
  queuePdfAttachments,
  takePdfAttachments,
  clearPdfAttachmentsForTarget,
} from "../src/lib/pdf-attachment-handoff";
import { uploadNewFile } from "../src/lib/storage";
import {
  clearUserStorage,
  storageScopeGeneration,
} from "../src/lib/user-storage";
import type { DocBlock } from "../src/lib/storage";
import {
  withOcrContent,
  excerptContext,
  bindPdfSource,
  extractRegion,
} from "../src/lib/pdf-excerpts";
import {
  setPendingSelection,
  takePendingSelection,
  restorePendingSelection,
  clearPendingSelectionForTarget,
} from "../src/lib/selection-context";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { test, expect, type Page } from "@playwright/test";
import {
  PDFDocument,
  PDFName,
  PDFString,
  StandardFonts,
  degrees,
  rgb,
} from "pdf-lib";
import { searchDocument, buildPageText, findInText } from "../src/lib/pdf-text";
import { rectFromViewport, blockRect, tableCsv } from "../src/lib/pdf-excerpts";
import { boxToViewport, viewportDimensions } from "../src/lib/pdf-geometry";
import { editRegion, pdfRect, screenRect } from "../src/lib/pdf-region-edit";
import { fineContent } from "../src/lib/pdf-fine-selection";
import { checkPdfBudget, readPdfResponse } from "../src/lib/pdf-input";
import {
  mergePdfs,
  organizePdf,
  validatePageOrder,
} from "../src/lib/pdf-page-tools";
import { parsePageRange } from "../src/lib/pdf-page-tools";
import { readFile } from "node:fs/promises";

const id = "a0000000-0000-0000-0000-000000000001";
const blocks: DocBlock[] = [
  {
    ordinal: 0,
    type: "paragraph",
    level: null,
    list_marker: null,
    page: 1,
    text: "Quarterly results are ready for review.",
    prov: [{ page: 1, bbox: [40, 690, 420, 650], origin: "bottom_left" }],
  },
  {
    ordinal: 1,
    type: "table",
    level: null,
    list_marker: null,
    page: 1,
    text: "| Item | Revenue |\n| --- | --- |\n| Product A | 1200 |\n| Product B | 1800 |",
    prov: [{ page: 1, bbox: [40, 580, 420, 450], origin: "bottom_left" }],
  },
  {
    ordinal: 2,
    type: "paragraph",
    level: null,
    list_marker: null,
    page: 2,
    text: "スキャンされた日本語の決算資料",
    prov: [{ page: 2, bbox: [50, 50, 400, 100], origin: "top_left" }],
  },
];
async function fixture() {
  const doc = await PDFDocument.create();
  const font = await doc.embedFont(StandardFonts.Helvetica);
  const page = doc.addPage([600, 800]);
  page.setCropBox(20, 30, 550, 740);
  page.drawText("QUARTERLY REPORT", {
    x: 60,
    y: 735,
    size: 10,
    font,
    color: rgb(0.3, 0.5, 0.4),
  });
  page.drawText("Quarterly results are ready for review.", {
    x: 60,
    y: 700,
    size: 17,
    font,
  });
  page.drawText("Financial overview", { x: 60, y: 650, size: 13, font });
  page.drawRectangle({
    x: 60,
    y: 480,
    width: 380,
    height: 130,
    borderColor: rgb(0.8, 0.8, 0.8),
    borderWidth: 1,
  });
  for (let i = 0; i < 3; i++)
    page.drawText(
      [
        "Item                      Revenue",
        "Product A               1200",
        "Product B               1800",
      ][i],
      { x: 72, y: 582 - i * 40, size: 13, font },
    );
  // 第2ページは文字層無し・原本 /Rotate=90。OCR 応答は fixture の保存済みブロックで代用する。
  const scan = doc.addPage([550, 740]);
  scan.setRotation(degrees(90));
  scan.drawRectangle({
    x: 50,
    y: 640,
    width: 350,
    height: 50,
    color: rgb(0.9, 0.92, 0.9),
  });
  const field = doc.getForm().createTextField("stable-field");
  field.setText("keep-me");
  field.addToPage(page, { x: 60, y: 40, width: 140, height: 20 });
  const outlines = doc.context.obj({ Type: PDFName.of("Outlines"), Count: 1 });
  const outlinesRef = doc.context.register(outlines);
  const entry = doc.context.register(
    doc.context.obj({
      Title: PDFString.of("Overview"),
      Parent: outlinesRef,
      Dest: [page.ref, PDFName.of("Fit")],
    }),
  );
  outlines.set(PDFName.of("First"), entry);
  outlines.set(PDFName.of("Last"), entry);
  doc.catalog.set(PDFName.of("Outlines"), outlinesRef);
  return Buffer.from(await doc.save());
}
async function setup(page: Page, failedBlocks = false, emptyBlocks = false) {
  page.on("console", (message) => {
    if (message.type() === "error")
      console.log("PDF_CONSOLE_ERROR", message.text());
  });
  page.on("pageerror", (error) => console.log("PDF_PAGE_ERROR", error.message));
  const pdf = await fixture();
  const reads = new Map<number, number>();
  let fail = failedBlocks;
  await page.context().addCookies([
    {
      name: "shiki_session",
      value: "pdf-ui-fixture",
      domain: "localhost",
      path: "/",
    },
  ]);
  await page.route("**/fixture.pdf", (route) =>
    route.fulfill({ contentType: "application/pdf", body: pdf }),
  );
  await page.route("**/api/**", async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === "/api/me")
      return route.fulfill({
        json: {
          id: "alice",
          tenant_id: "pdf-test",
          org: "test",
          roles: [],
          email: "alice@example.com",
          is_admin: false,
          can_view_audit: false,
        },
      });
    if (url.pathname.endsWith("/download-url"))
      return route.fulfill({
        json: { url: "http://localhost:3000/fixture.pdf" },
      });
    if (url.pathname.endsWith("/blocks")) {
      const pageNumber = Number(url.searchParams.get("page"));
      reads.set(pageNumber, (reads.get(pageNumber) ?? 0) + 1);
      if (fail)
        return route.fulfill({
          status: 503,
          json: { error: "解析結果を取得できません" },
        });
      const p = Number(url.searchParams.get("page"));
      const end = Number(url.searchParams.get("page_end")) || p;
      return route.fulfill({
        json: {
          blocks: emptyBlocks
            ? []
            : blocks.filter(
                (b) => b.page != null && b.page >= p && b.page <= end,
              ),
          next_from: null,
        },
      });
    }
    if (url.pathname === `/api/files/${id}`)
      return route.fulfill({
        json: {
          id,
          kind: "file",
          name: "四半期レポート.pdf",
          content_type: "application/pdf",
          version: 1,
        },
      });
    if (url.pathname === "/api/threads")
      return route.fulfill({ json: { threads: [], next_cursor: null } });
    if (url.pathname === "/api/models")
      return route.fulfill({ json: { models: [] } });
    return route.fulfill({
      json: {
        items: [],
        nodes: [],
        threads: [],
        results: [],
        models: [],
        groups: [],
        workspaces: [],
        next_cursor: null,
      },
    });
  });
  await page.goto(`/pdf/${id}`);
  await expect(page.getByTestId("pdf-mode-select"))
    .toBeVisible({ timeout: 20000 })
    .catch(async (error) => {
      console.log("PDF_SETUP_BODY", await page.locator("body").innerText());
      await page.screenshot({ path: "/tmp/pdf-failure.png" });
      throw error;
    });
  await expect
    .poll(() =>
      page
        .locator('[data-testid="pdf-viewer"] [data-page="1"] canvas')
        .evaluate((el) => (el as HTMLCanvasElement).width),
    )
    .toBeGreaterThan(0);
  return {
    reads: () => reads.get(1) ?? 0,
    debugReads: () => Array.from(reads),
    recover: () => {
      fail = false;
    },
  };
}
async function dragRegion(
  page: Page,
  n: number,
  bounds: [number, number, number, number],
) {
  const layer = page.locator(
    `[data-testid="pdf-viewer"] [data-page="${n}"] [data-testid="pdf-region-layer"]`,
  );
  await page
    .locator(`[data-testid="pdf-viewer"] [data-page="${n}"]`)
    .scrollIntoViewIfNeeded();
  await expect(layer).toBeVisible();
  await layer.evaluate((el, bounds) => {
    const root = el.closest('[data-testid="pdf-viewer"]') as HTMLElement;
    const r = el.getBoundingClientRect(),
      v = root.getBoundingClientRect();
    root.scrollTop +=
      r.top +
      (r.height * (bounds[1] + bounds[3])) / 2 -
      v.top -
      root.clientHeight / 2;
    root.scrollLeft +=
      r.left +
      (r.width * (bounds[0] + bounds[2])) / 2 -
      v.left -
      root.clientWidth / 2;
  }, bounds);
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  const box = (await layer.boundingBox())!;
  const [l, t, r, b] = bounds;
  await page.mouse.move(box.x + box.width * l, box.y + box.height * t);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * r, box.y + box.height * b, {
    steps: 8,
  });
  await page.mouse.up();
}

test.use({ deviceScaleFactor: 2 });

test("CropBox の位置と選択範囲が一致し、回転・ズーム・スクロール後も同じ抜粋を指す", async ({
  page,
}) => {
  const source = await setup(page);
  await page.getByTestId("pdf-mode-select").click();
  await dragRegion(page, 1, [35 / 550, 45 / 740, 425 / 550, 95 / 740]);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
  await expect(page.getByTestId("pdf-excerpt-card")).toContainText(
    blocks[0].text,
  );
  const before = source.reads();
  const originalBox = await page
    .getByTestId("pdf-region-box")
    .getAttribute("data-bbox");
  await page.screenshot({ path: "test-results/pdf-selection-light.png" });
  await page.getByTestId("chrome-menu").click();
  await page.getByTestId("pdf-rotate").click();
  await expect(page.getByTestId("pdf-region-box")).toHaveAttribute(
    "data-bbox",
    originalBox!,
  );
  await page.getByRole("button", { name: "拡大（⌘＋）" }).click();
  await expect(page.getByTestId("pdf-region-box")).toHaveAttribute(
    "data-bbox",
    originalBox!,
  );
  await dragRegion(page, 1, [445 / 740, 35 / 550, 585 / 740, 425 / 550]);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(2);
  expect(source.reads()).toBe(before);
  await page.getByRole("button", { name: "抜粋2を削除" }).click();
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
  await page.screenshot({ path: "test-results/pdf-selection-transformed.png" });
});

test("OCR 文字層無しの /Rotate ページを検索・選択でき、表はCSVで保存できる", async ({
  page,
}) => {
  const source = await setup(page);
  await page.getByTestId("pdf-search-toggle").click();
  await page.getByTestId("pdf-search-input").fill("日本語");
  await expect(page.getByTestId("pdf-search-status"))
    .toContainText("1", { timeout: 15000 })
    .catch(async (error) => {
      console.log(
        "OCR_DEBUG",
        source.debugReads(),
        await page.locator(".pdf-text-layer").allTextContents(),
      );
      throw error;
    });
  const ocrBox = page.locator('[data-page="2"] [data-ocr-search-box]');
  await expect(ocrBox).toBeVisible();
  await expect(ocrBox).toContainText("OCR・段落／表単位");
  await expect(
    page.locator('[data-page="2"] .pdf-text-layer'),
  ).not.toContainText("日本語");
  const bounds = (await ocrBox.boundingBox())!;
  // /Rotate=90 のため段落の高さ50ptが画面上の幅、幅350ptが高さになる。
  expect(bounds.height / bounds.width).toBeCloseTo(7, 0);
  await page.screenshot({ path: "test-results/pdf-ocr-block-search.png" });
  await page.getByTestId("pdf-mode-select").click();
  await dragRegion(page, 2, [635 / 740, 45 / 550, 695 / 740, 405 / 550]);
  await expect(page.getByTestId("pdf-excerpt-card")).toContainText(
    blocks[2].text,
  );
  await page.getByTestId("pdf-search-toggle").click();
  await dragRegion(page, 1, [35 / 550, 155 / 740, 425 / 550, 295 / 740]);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(2);
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "CSV", exact: true }).click();
  const saved = await download;
  const csv = await readFile((await saved.path())!, "utf8");
  expect(csv).toContain('"Product A","1200"');
  await page.getByRole("button", { name: "新規チャットに添付" }).click();
  await expect(page).toHaveURL(/\/$/, { timeout: 30000 });
  await expect(page.getByTestId("selection-chip")).toContainText(
    "PDF の選択範囲",
    { timeout: 15000 },
  );
});

test("選択途中の中止・キーボード選択・クリア", async ({ page }) => {
  await setup(page);
  await page.getByTestId("pdf-mode-select").click();
  const layer = page.getByTestId("pdf-region-layer").first();
  await layer.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
  await page.getByRole("button", { name: "すべて解除" }).click();
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(0);
  const b = (await layer.boundingBox())!;
  await page.mouse.move(b.x + 50, b.y + 60);
  await page.mouse.down();
  await page.mouse.move(b.x + 200, b.y + 100);
  await page.keyboard.press("Escape");
  await page.mouse.up();
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(0);
});

test("ユーティリティは抽出と回転PDFを生成し、入力エラーを表示する", async ({
  page,
}) => {
  await setup(page);
  await page.getByTestId("pdf-mode-tools").click();
  await page.getByLabel("対象のページ").fill("3");
  await page.getByRole("button", { name: "PDF を保存", exact: true }).click();
  await expect(
    page.getByTestId("pdf-utilities").getByRole("alert"),
  ).toContainText("1〜2");
  await page.getByLabel("対象のページ").fill("2");
  const event = page.waitForEvent("download");
  await page.getByRole("button", { name: "PDF を保存", exact: true }).click();
  const file = await event;
  const out = await PDFDocument.load(await readFile((await file.path())!));
  expect(out.getPageCount()).toBe(1);
  expect(out.getPage(0).getRotation().angle).toBe(90);
  await page.getByRole("button", { name: /ページを回転/ }).click();
  const rotationEvent = page.waitForEvent("download");
  await page.getByRole("button", { name: "PDF を保存", exact: true }).click();
  const rotated = await rotationEvent;
  const result = await PDFDocument.load(
    await readFile((await rotated.path())!),
  );
  expect(result.getPageCount()).toBe(2);
  expect(result.getPage(0).getRotation().angle).toBe(0);
  expect(result.getPage(1).getRotation().angle).toBe(180);
  expect(result.getForm().getTextField("stable-field").getText()).toBe(
    "keep-me",
  );
  expect(result.catalog.has(PDFName.of("Outlines"))).toBe(true);
  await page.screenshot({ path: "test-results/pdf-utilities-light.png" });
  await page.evaluate(() => document.documentElement.classList.add("dark"));
  await page.screenshot({ path: "test-results/pdf-utilities-dark.png" });
});

test("OCR取得失敗から再試行できる", async ({ page }) => {
  const source = await setup(page, true);
  await page.getByTestId("pdf-mode-select").click();
  await dragRegion(page, 2, [635 / 740, 45 / 550, 695 / 740, 405 / 550]);
  await expect(
    page.getByTestId("pdf-excerpt-panel").getByRole("alert"),
  ).toBeVisible();
  source.recover();
  await page.getByRole("button", { name: "解析結果を再取得" }).click();
  await expect(page.getByTestId("pdf-excerpt-card")).toContainText(
    blocks[2].text,
  );
});

for (const theme of ["light", "dark"] as const) {
  test(`狭幅・${theme}で抜粋レビューとツールを操作できる`, async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.addInitScript(
      (value) => localStorage.setItem("theme", value),
      theme,
    );
    await setup(page);
    await expect(
      page.getByRole("button", { name: "アシスタント", exact: true }),
    ).toBeVisible();
    await page.getByTestId("pdf-mode-select").click();
    await dragRegion(page, 1, [35 / 550, 45 / 740, 425 / 550, 95 / 740]);
    await page.getByRole("button", { name: /抜粋 1 件/ }).click();
    await expect(
      page.locator('[data-testid="pdf-excerpt-card"]:visible'),
    ).toBeVisible();
    await expect
      .poll(() =>
        page
          .locator('img[alt="p.1 の選択範囲"]:visible')
          .evaluate(
            (image) =>
              (image as HTMLImageElement).complete &&
              (image as HTMLImageElement).naturalWidth > 0,
          ),
      )
      .toBe(true);
    await expect
      .poll(async () => {
        const bounds = (await page.getByRole("dialog").boundingBox())!;
        return Math.abs(bounds.x + bounds.width - 390);
      })
      .toBeLessThan(1);
    await page.screenshot({ path: `test-results/pdf-mobile-${theme}.png` });
  });
}

test("PDF座標の往復変換は全回転・倍率・CropBox・元のページ回転で一致する", async () => {
  const { getDocument } = await import("pdfjs-dist/legacy/build/pdf.mjs");
  const doc = await getDocument({
    data: new Uint8Array(await fixture()),
    useSystemFonts: true,
  }).promise;
  const expected: [number, number, number, number] = [40, 650, 420, 690];
  for (const n of [1, 2]) {
    const page = await doc.getPage(n);
    for (const rotation of [0, 90, 180, 270])
      for (const scale of [0.33, 1, 2.25]) {
        const vp = page.getViewport({
          scale,
          rotation: (page.rotate + rotation) % 360,
        });
        const css = boxToViewport(expected, false, vp);
        const result = rectFromViewport(
          vp,
          [css.left, css.top],
          [css.left + css.width, css.top + css.height],
        );
        result.forEach((v, i) => expect(v).toBeCloseTo(expected[i], 6));
        const top = {
          page: n,
          bbox: [
            40,
            viewportDimensions(vp).pageHeight - 690,
            420,
            viewportDimensions(vp).pageHeight - 650,
          ],
          origin: "top_left" as const,
        };
        expect(blockRect(top, viewportDimensions(vp).pageHeight)).toEqual(
          expected,
        );
      }
  }
  await doc.destroy();
});

test("CSVは引用符・改行・区切りを保持し、ページ指定は不正範囲を拒否する", async () => {
  expect(
    tableCsv('| A | B |\n| :--- | ---: |\n| He said "yes" | x\\|y<br>z |'),
  ).toBe('\uFEFF"A","B"\r\n"He said ""yes""","x|y\nz"');
  expect(parsePageRange("2, 1-3, 2", 3)).toEqual([1, 0, 2]);
  for (const invalid of ["", "0", "4", "3-1", "1,,2", "1-999999999"])
    expect(() => parsePageRange(invalid, 3)).toThrow();
});

test("文字層の部分重複でもOCR本文を失わず、補助平面文字の添付を切らない", async () => {
  const { getDocument } = await import("pdfjs-dist/legacy/build/pdf.mjs");
  const doc = await getDocument({
    data: new Uint8Array(await fixture()),
    useSystemFonts: true,
  }).promise;
  const page = await doc.getPage(1);
  const native = await page.getTextContent();
  const output = withOcrContent(
    native,
    [{ ...blocks[0], text: blocks[0].text + " OCRからのみ取得できる本文" }],
    1,
    page.getViewport({ scale: 1 }),
  );
  expect(
    output.items.some(
      (item) => "str" in item && item.str.includes("OCRからのみ取得できる本文"),
    ),
  ).toBe(true);
  const text = "😀".repeat(5000) + "最後の行";
  const context = excerptContext(id, "test.pdf", 1, [
    {
      id: "x",
      page: 1,
      bbox: [0, 0, 100, 100],
      text,
      tables: [],
      blockOrdinals: [],
    },
  ]);
  setPendingSelection(context);
  expect(takePendingSelection()?.excerpt).toContain("最後の行");
  expect(tableCsv("| A | B |\n| --- | --- |\n| --- option | 1 |")).toContain(
    "--- option",
  );
  await doc.destroy();
});

test("CSVのデータ行がハイフンのみでも保持する", () => {
  expect(
    tableCsv("| left | right |\n| --- | --- |\n| --- | --- |\n| x | y |"),
  ).toContain('"\'---","\'---"');
});

test("画像アップロードの同名検索中にログアウトしたらdeclareを送らない", async () => {
  const original = globalThis.fetch;
  const requests: { url: string; method: string }[] = [];
  let release!: (response: Response) => void;
  const pending = new Promise<Response>((resolve) => {
    release = resolve;
  });
  globalThis.fetch = async (input, init) => {
    requests.push({ url: String(input), method: init?.method ?? "GET" });
    if (String(input).startsWith("/api/nodes")) return pending;
    throw new Error("旧画像のアップロード要求を出してはいけない");
  };
  try {
    const uploading = uploadNewFile({
      file: new File(["old-user-image"], "抜粋.png", { type: "image/png" }),
    });
    await expect.poll(() => requests.length).toBe(1);
    clearUserStorage();
    release(Response.json({ items: [], next_cursor: null }));
    await expect(uploading).rejects.toThrow("ログイン状態が変わった");
    expect(requests.filter((r) => r.method === "POST")).toHaveLength(0);
  } finally {
    globalThis.fetch = original;
  }
});

test("抜粋をアシスタントへ添付し、指示入力と送信まで待つ", async ({ page }) => {
  await setup(page);
  let created = 0;
  page.on("request", (request) => {
    if (
      request.method() === "POST" &&
      new URL(request.url()).pathname === "/api/threads"
    )
      created++;
  });
  await page.getByTestId("pdf-mode-select").click();
  await dragRegion(page, 1, [35 / 550, 45 / 740, 425 / 550, 95 / 740]);
  await page.getByRole("button", { name: "アシスタントに添付" }).click();
  await expect(
    page.getByRole("complementary", { name: "PDF のアシスタント" }),
  ).toBeVisible();
  await expect(page.getByTestId("selection-chip")).toContainText(
    "PDF の選択範囲",
    { timeout: 15000 },
  );
  await expect(page.getByLabel("添付する PDF の抜粋")).toContainText(
    "Quarterly results are ready for review.",
  );
  await expect(page.getByLabel("メッセージを入力")).toBeEmpty();
  expect(created).toBe(0);
  await expect(page.getByTestId("panel-empty-hint")).toContainText(
    "添付した抜粋",
  );
  await expect(page.getByTestId("panel-empty-hint")).not.toContainText("編集");
  await page.screenshot({ path: "test-results/pdf-assistant-light.png" });
  await page.getByLabel("メッセージを入力").fill("この抜粋の要点を説明して");
  await page.getByTestId("pdf-mode-select").click();
  await expect(
    page.getByRole("complementary", { name: "PDF のアシスタント" }),
  ).toBeHidden();
  await page.getByTestId("pdf-mode-read").click();
  await page.getByRole("button", { name: "アシスタント", exact: true }).click();
  await expect(page.getByLabel("メッセージを入力")).toHaveText(
    "この抜粋の要点を説明して",
  );
  await page.getByLabel("メッセージを入力").fill("");
  await page
    .getByRole("button", { name: "新しいチャット", exact: true })
    .click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByTestId("selection-chip")).toHaveCount(0);
});

test("保存済み表はnativeと重複せず、跨ページのOCRは該当ページだけに載る", async () => {
  const native = {
    items: [
      {
        str: "Product A",
        dir: "ltr",
        transform: [12, 0, 0, 12, 10, 10],
        width: 80,
        height: 12,
        fontName: "f",
        hasEOL: true,
      },
    ],
    styles: {},
    lang: "en",
  };
  const viewport = {
    rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
  };
  const table: DocBlock = {
    ordinal: 1,
    type: "table",
    text: "| Product |\n| --- |\n| Product A |",
    prov: [{ page: 1, bbox: [0, 50, 200, 0], origin: "bottom_left" }],
  };
  const result = withOcrContent(native, [table], 1, viewport as never);
  expect(findInText(buildPageText(result).text, "Product A")).toHaveLength(1);
  const split: DocBlock = {
    ordinal: 2,
    type: "paragraph",
    text: "😀first second",
    prov: [
      { page: 1, bbox: [0, 50, 200, 0], charspan: [0, 6] },
      { page: 2, bbox: [0, 50, 200, 0], charspan: [7, 13] },
    ],
  };
  const empty = { items: [], styles: {}, lang: "" };
  expect(
    buildPageText(withOcrContent(empty, [split], 1, viewport as never)).text,
  ).not.toContain("second");
  expect(
    buildPageText(withOcrContent(empty, [split], 2, viewport as never)).text,
  ).toContain("second");
  const legacy = {
    ...split,
    prov: split.prov.map(({ charspan: _span, ...p }) => p),
  };
  expect(
    buildPageText(withOcrContent(empty, [legacy], 1, viewport as never)).text,
  ).not.toContain("second");
});

test("CSVは文書由来の式を文字列化し、符号付き数値は保持する", () => {
  const csv = tableCsv(
    "| value |\n| --- |\n| =1+1 |\n| +SUM(A1:A2) |\n| @SUM(A1:A2) |\n| -12.5 |\n| +20 |",
  );
  expect(csv).toContain('"\'=1+1"');
  expect(csv).toContain('"\'+SUM(A1:A2)"');
  expect(csv).toContain('"\'@SUM(A1:A2)"');
  expect(csv).toContain('"-12.5"');
  expect(csv).toContain('"+20"');
});

test("全ページ検索は16ページ窓・最大4並列で取得し、ページ順を保つ", async () => {
  const original = globalThis.fetch;
  let calls = 0,
    active = 0,
    peak = 0;
  globalThis.fetch = async (input) => {
    const url = new URL(String(input), "http://localhost");
    const from = Number(url.searchParams.get("page")),
      to = Number(url.searchParams.get("page_end"));
    expect(to - from).toBeLessThan(16);
    calls++;
    active++;
    peak = Math.max(peak, active);
    await new Promise((resolve) => setTimeout(resolve, 2));
    active--;
    return Response.json({ blocks: [], next_from: null });
  };
  const doc = {
    numPages: 65,
    getPage: async (number: number) => ({
      getTextContent: async () => ({
        items: [
          {
            str: `page ${number}`,
            dir: "ltr",
            transform: [12, 0, 0, 12, 0, 0],
            width: 80,
            height: 12,
            fontName: "f",
            hasEOL: true,
          },
        ],
        styles: {},
        lang: "en",
      }),
      getViewport: () => ({
        rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
      }),
    }),
  } as unknown as PDFDocumentProxy;
  try {
    bindPdfSource(doc, id, 1);
    const matches = await searchDocument(doc, "page", {
      signal: new AbortController().signal,
      onProgress: () => {},
    });
    expect(matches.map((m) => m.page)).toEqual(
      Array.from({ length: 65 }, (_, i) => i + 1),
    );
    expect(calls).toBe(5);
    expect(peak).toBeLessThanOrEqual(4);
    expect(peak).toBeGreaterThan(1);
  } finally {
    globalThis.fetch = original;
  }
});

test("和文のinline OCR断片をまたいでも本文に無い空白を挿入しない", () => {
  const text = "申出書は1か月前まで";
  const block: DocBlock = {
    ordinal: 0,
    type: "paragraph",
    text,
    page: 1,
    prov: [
      {
        page: 1,
        bbox: [10, 40, 80, 20],
        origin: "bottom_left",
        charspan: [0, 4],
      },
      {
        page: 1,
        bbox: [80, 40, 200, 20],
        origin: "bottom_left",
        charspan: [4, 10],
      },
    ],
  };
  const viewport = {
    rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
  };
  const output = withOcrContent(
    { items: [], styles: {}, lang: "ja" },
    [block],
    1,
    viewport as never,
  );
  expect(
    findInText(buildPageText(output).text, "申出書は1か月前"),
  ).toHaveLength(1);
});

test("原本内で90度回転したnative文字もOCRと重複しない", () => {
  const native = {
    items: [
      {
        str: "Vertical",
        dir: "ltr",
        transform: [0, 12, -12, 0, 100, 100],
        width: 40,
        height: 12,
        fontName: "f",
        hasEOL: true,
      },
    ],
    styles: {},
    lang: "en",
  };
  const viewport = {
    rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
  };
  const block: DocBlock = {
    ordinal: 0,
    type: "paragraph",
    text: "Vertical",
    page: 1,
    prov: [{ page: 1, bbox: [88, 140, 100, 100], origin: "bottom_left" }],
  };
  const output = withOcrContent(native, [block], 1, viewport as never);
  expect(findInText(buildPageText(output).text, "vertical")).toHaveLength(1);
});

test("同じ選択範囲でOCRがない領域のnative本文も取りこぼさない", async () => {
  const original = globalThis.fetch;
  const makeNative = (str: string, y: number) => ({
    str,
    dir: "ltr",
    transform: [12, 0, 0, 12, 10, y],
    width: 80,
    height: 12,
    fontName: "f",
    hasEOL: true,
  });
  const saved: DocBlock[] = [
    {
      ordinal: 0,
      type: "paragraph",
      text: "保存済み段落",
      page: 1,
      prov: [{ page: 1, bbox: [10, 112, 100, 90], origin: "bottom_left" }],
    },
    {
      ordinal: 1,
      type: "paragraph",
      text: "旧形式の跨ページ本文",
      page: 1,
      prov: [
        { page: 1, bbox: [10, 62, 100, 40], origin: "bottom_left" },
        { page: 2, bbox: [10, 62, 100, 40], origin: "bottom_left" },
      ],
    },
  ];
  globalThis.fetch = async () =>
    Response.json({ blocks: saved, next_from: null });
  const doc = {
    numPages: 2,
    getPage: async () => ({
      getViewport: () => ({
        rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
      }),
      getTextContent: async () => ({
        items: [
          makeNative("保存済み段落", 100),
          makeNative("nativeに残る本文", 50),
        ],
        styles: {},
        lang: "ja",
      }),
    }),
  } as unknown as PDFDocumentProxy;
  try {
    bindPdfSource(doc, id, 1);
    const excerpt = await extractRegion(doc, 1, [0, 0, 200, 200]);
    expect(excerpt.text).toContain("保存済み段落");
    expect(excerpt.text).toContain("nativeに残る本文");
    expect(excerpt.text.match(/保存済み段落/g)).toHaveLength(1);
    expect(excerpt.text).not.toContain("旧形式の跨ページ本文");
  } finally {
    globalThis.fetch = original;
  }
});

test("非表示のPDF入力欄は新規チャット宛の画像を消費できない", () => {
  const files = [{ node_id: id, name: "抜粋.png" }];
  queuePdfAttachments(files, "home");
  expect(takePdfAttachments(`pdf:${id}`)).toEqual([]);
  expect(takePdfAttachments("home")).toEqual(files);
  expect(takePdfAttachments("home")).toEqual([]);
  queuePdfAttachments(files, `pdf:${id}`);
  expect(takePdfAttachments("home")).toEqual([]);
  expect(takePdfAttachments(`pdf:${id}`)).toEqual(files);
});

test("2段組の抜粋は座標の行順より保存済みordinalの読み順を優先する", async () => {
  const original = globalThis.fetch;
  const saved: DocBlock[] = ["左上", "左下", "右上", "右下"].map(
    (text, ordinal) => ({
      ordinal,
      type: "paragraph",
      text,
      page: 1,
      prov: [
        {
          page: 1,
          bbox: [
            ordinal < 2 ? 20 : 300,
            ordinal % 2 ? 650 : 750,
            ordinal < 2 ? 220 : 500,
            ordinal % 2 ? 610 : 710,
          ],
          origin: "bottom_left",
        },
      ],
    }),
  );
  globalThis.fetch = async () =>
    Response.json({ blocks: saved, next_from: null });
  const doc = {
    numPages: 1,
    getPage: async () => ({
      getViewport: () => ({
        rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
      }),
      getTextContent: async () => ({ items: [], styles: {}, lang: "ja" }),
    }),
  } as unknown as PDFDocumentProxy;
  try {
    bindPdfSource(doc, id, 1);
    expect((await extractRegion(doc, 1, [0, 0, 600, 800])).text).toBe(
      "左上\n\n左下\n\n右上\n\n右下",
    );
  } finally {
    globalThis.fetch = original;
  }
});

test("native文字片は単語内で連結し、明示空白と改行を保持する", async () => {
  const original = globalThis.fetch;
  const item = (
    str: string,
    x: number,
    y: number,
    width: number,
    hasEOL: boolean,
    fontName: string,
    height = 12,
  ) => ({
    str,
    dir: "ltr",
    transform: [12, 0, 0, 12, x, y],
    width,
    height,
    fontName,
    hasEOL,
  });
  const inline = [
    item("inter", 10, 100, 30, false, "regular"),
    item("national", 40, 100, 48, false, "bold"),
    item(" ", 88, 100, 4, false, "bold", 0),
    item("relations", 92, 100, 54, true, "bold"),
    item("次の", 10, 50, 24, false, "regular"),
    item("行", 34, 50, 12, true, "bold"),
  ];
  const legacy: DocBlock = {
    ordinal: 1,
    type: "paragraph",
    text: "ページをまたぐ旧本文",
    page: 1,
    prov: [
      { page: 1, bbox: [0, 200, 200, 0], origin: "bottom_left" },
      { page: 2, bbox: [0, 200, 200, 0], origin: "bottom_left" },
    ],
  };
  try {
    for (const blocks of [[], [legacy]]) {
      for (const { items, expected } of [
        { items: inline, expected: "international relations\n次の行" },
        {
          items: [
            item("first", 10, 100, 30, false, "regular"),
            item("", 40, 100, 0, true, "regular", 0),
            item("next", 10, 50, 30, false, "regular"),
          ],
          expected: "first\nnext",
        },
      ]) {
        globalThis.fetch = async () =>
          Response.json({ blocks, next_from: null });
        const doc = {
          numPages: 2,
          getPage: async () => ({
            getViewport: () => ({
              rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
            }),
            getTextContent: async () => ({ items, styles: {}, lang: "ja" }),
          }),
        } as unknown as PDFDocumentProxy;
        bindPdfSource(doc, id, 1);
        expect((await extractRegion(doc, 1, [0, 0, 200, 200])).text).toBe(
          expected,
        );
      }
    }
  } finally {
    globalThis.fetch = original;
  }
});

test("native縦書きの下方向の列を取得し、OCRと二重に載せない", async () => {
  const native = {
    items: [
      {
        str: "日本語",
        dir: "ttb",
        transform: [12, 0, 0, 12, 100, 700],
        width: 12,
        height: 36,
        fontName: "f",
        hasEOL: true,
      },
    ],
    styles: {
      f: { fontFamily: "serif", ascent: 0.8, descent: -0.2, vertical: true },
    },
    lang: "ja",
  };
  const viewport = {
    rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
  };
  const doc = {
    numPages: 1,
    getPage: async () => ({
      getViewport: () => viewport,
      getTextContent: async () => native,
    }),
  } as unknown as PDFDocumentProxy;
  expect((await extractRegion(doc, 1, [80, 660, 120, 695])).text).toBe(
    "日本語",
  );
  const block: DocBlock = {
    ordinal: 0,
    type: "paragraph",
    text: "日本語",
    page: 1,
    prov: [{ page: 1, bbox: [90, 700, 112, 660], origin: "bottom_left" }],
  };
  expect(
    findInText(
      buildPageText(withOcrContent(native, [block], 1, viewport as never)).text,
      "日本語",
    ),
  ).toHaveLength(1);
});

test("OCR後半を同じ段落のnative前半と次段落の間へ統合する", () => {
  const native = {
    items: [
      {
        str: "申出書は",
        dir: "ltr",
        transform: [12, 0, 0, 12, 10, 700],
        width: 48,
        height: 12,
        fontName: "f",
        hasEOL: false,
      },
      {
        str: "次の段落",
        dir: "ltr",
        transform: [12, 0, 0, 12, 10, 600],
        width: 48,
        height: 12,
        fontName: "f",
        hasEOL: true,
      },
    ],
    styles: {},
    lang: "ja",
  };
  const blocks: DocBlock[] = [
    {
      ordinal: 0,
      type: "paragraph",
      text: "申出書は1か月前まで",
      page: 1,
      prov: [
        {
          page: 1,
          bbox: [10, 712, 58, 695],
          origin: "bottom_left",
          charspan: [0, 4],
        },
        {
          page: 1,
          bbox: [58, 712, 130, 695],
          origin: "bottom_left",
          charspan: [4, 10],
        },
      ],
    },
    {
      ordinal: 1,
      type: "paragraph",
      text: "次の段落",
      page: 1,
      prov: [{ page: 1, bbox: [10, 612, 80, 595], origin: "bottom_left" }],
    },
  ];
  const viewport = {
    rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
  };
  const output = withOcrContent(native, blocks, 1, viewport as never);
  expect(
    output.items
      .filter((item) => "str" in item)
      .map((item) => ("str" in item ? item.str : "")),
  ).toEqual(["申出書は", "1か月前まで", "次の段落"]);
  expect(
    findInText(buildPageText(output).text, "申出書は1か月前"),
  ).toHaveLength(1);
});

test("PDF本文も指定した添付先だけが消費できる", () => {
  const context = excerptContext(id, "test.pdf", 1, [
    {
      id: "x",
      page: 1,
      bbox: [0, 0, 100, 100],
      text: "宛先を限定した本文",
      tables: [],
      blockOrdinals: [],
    },
  ]);
  setPendingSelection(context, `pdf:${id}`);
  expect(takePendingSelection("home")).toBeNull();
  expect(takePendingSelection(`pdf:${id}`)?.excerpt).toContain(
    "宛先を限定した本文",
  );
  setPendingSelection(context, "home");
  expect(takePendingSelection(`pdf:${id}`)).toBeNull();
  expect(takePendingSelection("home")?.excerpt).toContain("宛先を限定した本文");
});

test("キーボードだけで部分範囲を移動・縮小して追加できる", async ({ page }) => {
  await setup(page);
  await page.getByTestId("pdf-mode-select").click();
  const layer = page.locator(
    '[data-page="1"] [data-testid="pdf-region-layer"]',
  );
  await layer.focus();
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("ArrowRight");
  await expect(page.getByTestId("pdf-page-input").first()).toHaveValue("1");
  const before = (await page.getByTestId("pdf-region-preview").boundingBox())!;
  await page.keyboard.press("Shift+ArrowLeft");
  const after = (await page.getByTestId("pdf-region-preview").boundingBox())!;
  expect(after.width).toBeLessThan(before.width);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
  const selected = (await page.getByTestId("pdf-region-box").boundingBox())!;
  const whole = (await layer.boundingBox())!;
  expect(selected.width * selected.height).toBeLessThan(
    (whole.width * whole.height) / 2,
  );
  await page.screenshot({ path: "test-results/pdf-keyboard-partial.png" });
});

test("別タブのログアウトで表示済みPDFを破棄する", async ({ page }) => {
  await setup(page);
  await expect(page.getByTestId("pdf-viewer")).toBeVisible();
  // サーバのlogout完了前は旧Cookieが有効でも、PDFを取り直さない。
  let reloads = 0;
  await page.route(`**/api/files/${id}`, async (route) => {
    reloads++;
    await route.fallback();
  });
  const other = await page.context().newPage();
  await other.goto(new URL("/icon.svg", page.url()).toString());
  await other.evaluate(() =>
    localStorage.setItem("shiki:logout-signal", String(Date.now())),
  );
  await expect(page.getByTestId("pdf-viewer")).toBeHidden();
  await page.waitForTimeout(500);
  expect(reloads).toBe(0);
  await other.close();
});

test("finalize待機中のログアウト後に旧ファイルを返さない", async () => {
  const originalFetch = globalThis.fetch;
  const xhrDescriptor = Object.getOwnPropertyDescriptor(
    globalThis,
    "XMLHttpRequest",
  );
  let release!: (response: Response) => void;
  const pending = new Promise<Response>((resolve) => {
    release = resolve;
  });
  let finalizing = false;
  class FakeXHR {
    status = 200;
    upload = { onprogress: null };
    onload: (() => void) | null = null;
    open() {}
    setRequestHeader() {}
    send() {
      queueMicrotask(() => this.onload?.());
    }
  }
  Object.defineProperty(globalThis, "XMLHttpRequest", {
    configurable: true,
    value: FakeXHR,
  });
  globalThis.fetch = async (input) => {
    const url = String(input);
    if (url.startsWith("/api/nodes"))
      return Response.json({ items: [], next_cursor: null });
    if (url.endsWith("/finalize")) {
      finalizing = true;
      return pending;
    }
    return Response.json({
      upload_id: "upload-fixture",
      node_id: id,
      upload_url: "/fixture-upload",
    });
  };
  try {
    const uploading = uploadNewFile({
      file: new File(["old"], "old-user.png", { type: "image/png" }),
    });
    await expect.poll(() => finalizing).toBe(true);
    clearUserStorage();
    release(
      Response.json({ id, name: "old-user.png", kind: "file", version: 1 }),
    );
    await expect(uploading).rejects.toThrow("ログイン状態が変わった");
  } finally {
    globalThis.fetch = originalFetch;
    if (xhrDescriptor)
      Object.defineProperty(globalThis, "XMLHttpRequest", xhrDescriptor);
    else Reflect.deleteProperty(globalThis, "XMLHttpRequest");
  }
});

test("別タブログアウトで取得済み画像の名前とプレビューを消す", async ({
  page,
}) => {
  await setup(page);
  await page
    .getByRole("button", { name: "新しいチャット", exact: true })
    .click();
  await expect(page).toHaveURL(/\/$/);
  await page.route("**/fixture-upload", (route) =>
    route.fulfill({ status: 200, body: "" }),
  );
  await page.route("**/api/files", (route) =>
    route.fulfill({
      json: {
        upload_id: "upload-fixture",
        node_id: id,
        upload_url: new URL("/fixture-upload", page.url()).toString(),
      },
    }),
  );
  await page.route("**/api/files/upload-fixture/finalize", (route) =>
    route.fulfill({
      json: {
        id,
        name: "private-image.png",
        kind: "file",
        version: 1,
        content_type: "image/png",
      },
    }),
  );
  await page.locator('input[type="file"]').setInputFiles({
    name: "private-image.png",
    mimeType: "image/png",
    buffer: Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aHWoAAAAASUVORK5CYII=",
      "base64",
    ),
  });
  await expect(
    page.getByRole("img", { name: "private-image.png", exact: true }),
  ).toBeVisible();
  await page.getByLabel("メッセージを入力").fill("private draft");
  const other = await page.context().newPage();
  await other.goto(new URL("/icon.svg", page.url()).toString());
  await other.evaluate(() =>
    localStorage.setItem("shiki:logout-signal", String(Date.now())),
  );
  await expect(
    page.getByRole("img", { name: "private-image.png", exact: true }),
  ).toHaveCount(0);
  await expect(page.getByText(/private-image\.png/)).toHaveCount(0);
  await expect(page.getByLabel("メッセージを入力")).toBeEmpty();
  await other.close();
});

test("カードの再取得中に削除した範囲を遅延応答で復活させない", async ({
  page,
}) => {
  await setup(page, false, true);
  await page.getByTestId("pdf-mode-select").click();
  await dragRegion(page, 2, [635 / 740, 45 / 550, 695 / 740, 405 / 550]);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
  let release!: () => void;
  let requested = false;
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(`**/api/files/${id}/versions/1/blocks*`, async (route) => {
    requested = true;
    await pending;
    await route.fulfill({ json: { blocks, next_from: null } });
  });
  await page.getByRole("button", { name: "解析結果を再取得" }).click();
  await expect.poll(() => requested).toBe(true);
  await page.getByRole("button", { name: "抜粋1を削除" }).click();
  release();
  await expect(
    page.getByTestId("pdf-excerpt-panel").locator("[aria-busy]"),
  ).toHaveAttribute("aria-busy", "false");
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(0);
});

test("古い画面の失敗復元とcleanupは新しい宛先の本文・画像を壊さない", () => {
  const context = excerptContext(id, "test.pdf", 1, [
    {
      id: "x",
      page: 1,
      bbox: [0, 0, 100, 100],
      text: "old",
      tables: [],
      blockOrdinals: [],
    },
  ]);
  const generation = storageScopeGeneration();
  const target = `pdf:${id}`;
  setPendingSelection({ ...context, excerpt: "new home" }, "home");
  queuePdfAttachments([{ node_id: id, name: "new.png" }], "home");
  restorePendingSelection(context, target, generation);
  clearPendingSelectionForTarget(target);
  clearPdfAttachmentsForTarget(target);
  expect(takePendingSelection("home")?.excerpt).toBe("new home");
  expect(takePdfAttachments("home")[0]?.name).toBe("new.png");
  setPendingSelection(context, target);
  queuePdfAttachments([{ node_id: id, name: "old.png" }], target);
  clearPendingSelectionForTarget(target);
  clearPdfAttachmentsForTarget(target);
  expect(takePendingSelection(target)).toBeNull();
  expect(takePdfAttachments(target)).toEqual([]);
});

test("OCR矩形内の表記が異なるnative文字を検索で二重に数えない", () => {
  const native = {
    items: [
      {
        str: "Total 1000",
        dir: "ltr",
        transform: [12, 0, 0, 12, 50, 700],
        width: 100,
        height: 12,
        fontName: "f",
        hasEOL: true,
      },
      {
        str: "Unparsed extra",
        dir: "ltr",
        transform: [12, 0, 0, 12, 170, 700],
        width: 100,
        height: 12,
        fontName: "f",
        hasEOL: true,
      },
    ],
    styles: {
      f: { fontFamily: "serif", ascent: 0.8, descent: -0.2, vertical: false },
    },
    lang: "en",
  };
  const block: DocBlock = {
    ordinal: 0,
    type: "paragraph",
    text: "Total 1,000",
    page: 1,
    prov: [{ page: 1, bbox: [40, 720, 180, 680], origin: "bottom_left" }],
  };
  const viewport = {
    rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
  };
  const text = buildPageText(
    withOcrContent(native, [block], 1, viewport as never),
  ).text;
  expect(findInText(text, "Total")).toHaveLength(1);
  expect(findInText(text, "Total 1,000")).toHaveLength(1);
  expect(findInText(text, "Unparsed extra")).toHaveLength(1);
  expect(native.items[0].str).toBe("Total 1000");
});

test("画像添付中の削除は旧添付を中止し、残る範囲を添付できる", async ({
  page,
}) => {
  await setup(page);
  await page.getByTestId("pdf-mode-select").click();
  await dragRegion(page, 1, [35 / 550, 45 / 740, 425 / 550, 95 / 740]);
  await dragRegion(page, 1, [35 / 550, 185 / 740, 425 / 550, 325 / 740]);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(2);
  await page.getByRole("checkbox", { name: /画像も添付/ }).check();
  await page.route("**/fixture-upload", (route) => route.fulfill({ body: "" }));
  let uploads = 0;
  await page.route("**/api/files", (route) =>
    route.fulfill({
      json: {
        upload_id: `pdf-image-${++uploads}`,
        node_id: id,
        upload_url: new URL("/fixture-upload", page.url()).toString(),
      },
    }),
  );
  let release!: () => void;
  let requested = false;
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/files/pdf-image-*/finalize", async (route) => {
    const first = route.request().url().includes("pdf-image-1/");
    if (first) {
      requested = true;
      await pending;
    }
    await route.fulfill({
      json: {
        id,
        name: first ? "deleted.png" : "remaining.png",
        kind: "file",
        version: 1,
        content_type: "image/png",
      },
    });
  });
  await page.getByRole("button", { name: "アシスタントに添付" }).click();
  await expect.poll(() => requested).toBe(true);
  await page.getByRole("button", { name: "抜粋1を削除" }).click();
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
  await expect(
    page.getByRole("button", { name: "アシスタントに添付" }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "アシスタントに添付" }).click();
  await expect(page.getByLabel("添付する PDF の抜粋")).toContainText(
    "Product A",
  );
  release();
  await page.waitForTimeout(300);
  await expect(page.getByLabel("添付する PDF の抜粋")).not.toContainText(
    "Quarterly results",
  );
  await expect(page.getByText("deleted.png", { exact: true })).toHaveCount(0);
  expect(uploads).toBe(2);
});

test("重なるOCR枠でも別段落と一致するnative本文を検索から消さない", () => {
  const native = {
    items: [
      {
        str: "Alpha",
        dir: "ltr",
        transform: [12, 0, 0, 12, 10, 700],
        width: 60,
        height: 12,
        fontName: "f",
        hasEOL: true,
      },
      {
        str: "Beta",
        dir: "ltr",
        transform: [12, 0, 0, 12, 10, 650],
        width: 60,
        height: 12,
        fontName: "f",
        hasEOL: true,
      },
    ],
    styles: {
      f: { fontFamily: "serif", ascent: 0.8, descent: -0.2, vertical: false },
    },
    lang: "en",
  };
  const saved: DocBlock[] = [
    {
      ordinal: 0,
      type: "paragraph",
      text: "Alpha normalized",
      page: 1,
      prov: [{ page: 1, bbox: [0, 730, 100, 620], origin: "bottom_left" }],
    },
    {
      ordinal: 1,
      type: "paragraph",
      text: "Beta",
      page: 1,
      prov: [{ page: 1, bbox: [0, 670, 100, 630], origin: "bottom_left" }],
    },
  ];
  const viewport = {
    rawDims: { pageX: 0, pageY: 0, pageWidth: 600, pageHeight: 800 },
  };
  for (const ordered of [saved, [...saved].reverse()]) {
    const text = buildPageText(
      withOcrContent(native, ordered, 1, viewport as never),
    ).text;
    expect(findInText(text, "Alpha normalized")).toHaveLength(1);
    expect(findInText(text, "Beta")).toHaveLength(1);
    expect(text.indexOf("alpha")).toBeLessThan(text.indexOf("beta"));
  }
});

test("確定枠を移動・リサイズしてもIDと件数を保ち、変換途中の操作を中止できる", async ({
  page,
}) => {
  const source = await setup(page);
  await page.getByTestId("pdf-mode-select").click();
  await dragRegion(page, 1, [35 / 550, 45 / 740, 425 / 550, 95 / 740]);
  const box = page.getByTestId("pdf-region-box");
  const id = await box.getAttribute("data-selection-id");
  const before = JSON.parse((await box.getAttribute("data-bbox"))!);
  await box.locator("[data-region-move]").focus();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(box).toHaveAttribute("data-selection-id", id!);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
  await expect
    .poll(async () => JSON.parse((await box.getAttribute("data-bbox"))!)[1])
    .toBeLessThan(before[1]);
  await box.locator("[data-region-move]").focus();
  const handle = page.getByLabel("抜粋1の右下を調整", { exact: true });
  const r = (await handle.boundingBox())!;
  await page.mouse.move(r.x + r.width / 2, r.y + r.height / 2);
  await page.mouse.down();
  await page.mouse.move(r.x + r.width / 2 + 30, r.y + r.height / 2 + 20, {
    steps: 5,
  });
  await page.mouse.up();
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
  await expect
    .poll(async () => JSON.parse((await box.getAttribute("data-bbox"))!)[2])
    .toBeGreaterThan(before[2]);
  const saved = await box.getAttribute("data-bbox");
  await box.locator("[data-region-move]").focus();
  await page.keyboard.press("ArrowLeft");
  await page.keyboard.press("Escape");
  await expect(box).toHaveAttribute("data-bbox", saved!);
  const b = (await box.boundingBox())!;
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width / 2 + 10, b.y + b.height / 2 + 10);
  await page.keyboard.press("Control++");
  await page.mouse.up();
  await expect(box).toHaveAttribute("data-bbox", saved!);
  expect(source.reads()).toBeLessThanOrEqual(1);
  await page.screenshot({ path: "test-results/pdf-edit-handles.png" });
});

test("行と表セルを文字層の実座標から選び、セルだけを添付できる", async ({
  page,
}) => {
  await setup(page);
  await page.getByTestId("pdf-mode-select").click();
  await page.getByRole("button", { name: "行", exact: true }).click();
  await page
    .getByRole("button", {
      name: "行を選択: Quarterly results are ready for review.",
      exact: true,
    })
    .click();
  await expect(page.getByTestId("pdf-excerpt-card")).toContainText(
    "Quarterly results are ready for review.",
  );
  await expect(page.getByTestId("pdf-excerpt-card")).not.toContainText(
    "Financial overview",
  );
  await page.getByRole("button", { name: "すべて解除", exact: true }).click();
  await page.getByRole("button", { name: "表セル", exact: true }).click();
  await page
    .getByRole("button", { name: "セルを選択: 1200", exact: true })
    .click();
  await expect(page.getByTestId("pdf-excerpt-card")).toContainText("1200");
  await expect(page.getByTestId("pdf-excerpt-card")).not.toContainText(
    "Product B",
  );
  const cellBox = page.getByTestId("pdf-region-box");
  const cellBefore = await cellBox.getAttribute("data-bbox");
  await page.getByRole("button", { name: "自由範囲", exact: true }).click();
  await cellBox.locator("[data-region-move]").focus();
  await page.keyboard.press("Shift+ArrowRight");
  await page.keyboard.press("Enter");
  await expect(cellBox).not.toHaveAttribute("data-bbox", cellBefore!);
  await expect(page.getByTestId("pdf-excerpt-card")).toContainText("1200");
  await page.getByRole("button", { name: "アシスタントに添付" }).click();
  await expect(page.getByLabel("添付する PDF の抜粋")).toContainText("1200");
  await expect(page.getByLabel("添付する PDF の抜粋")).not.toContainText(
    "1800",
  );
});

test("ページ整理で並べ替え・削除・取り消しを行い、実PDFへ反映する", async ({
  page,
}) => {
  await setup(page);
  await page.getByTestId("pdf-mode-tools").click();
  await page.getByRole("button", { name: /ページを整理/ }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByTestId("pdf-organize-page")).toHaveCount(2);
  await dialog.getByLabel("ページ2を前へ", { exact: true }).click();
  await expect(dialog.getByTestId("pdf-organize-page").first()).toHaveAttribute(
    "data-source-page",
    "2",
  );
  let event = page.waitForEvent("download");
  await dialog.getByRole("button", { name: "PDF を保存", exact: true }).click();
  let out = await PDFDocument.load(
    await readFile((await (await event).path())!),
  );
  expect(out.getPage(0).getRotation().angle).toBe(90);
  expect(out.getForm().getTextField("stable-field").getText()).toBe("keep-me");
  expect(out.catalog.has(PDFName.of("Outlines"))).toBe(true);
  await dialog.getByLabel("ページ1を削除", { exact: true }).click();
  await expect(dialog.getByTestId("pdf-organize-page")).toHaveCount(1);
  event = page.waitForEvent("download");
  await dialog.getByRole("button", { name: "PDF を保存", exact: true }).click();
  out = await PDFDocument.load(await readFile((await (await event).path())!));
  expect(out.getPageCount()).toBe(1);
  expect(out.getPage(0).getRotation().angle).toBe(90);
  await dialog.getByRole("button", { name: "元に戻す", exact: true }).click();
  await expect(dialog.getByTestId("pdf-organize-page")).toHaveCount(2);
  await dialog
    .getByRole("button", { name: "最初の状態へ", exact: true })
    .click();
  await dialog
    .getByTestId("pdf-organize-page")
    .first()
    .dragTo(dialog.getByTestId("pdf-organize-page").last());
  await expect(dialog.getByTestId("pdf-organize-page").first()).toHaveAttribute(
    "data-source-page",
    "2",
  );
  await dialog
    .getByRole("button", { name: "最初の状態へ", exact: true })
    .click();
  await dialog.getByLabel("ページ2を削除", { exact: true }).click();
  const textDownload = page.waitForEvent("download");
  await dialog.getByRole("button", { name: "PDF を保存", exact: true }).click();
  const { getDocument } = await import("pdfjs-dist/legacy/build/pdf.mjs");
  const textDoc = await getDocument({
    data: new Uint8Array(await readFile((await (await textDownload).path())!)),
    useSystemFonts: true,
  }).promise;
  const textContent = await (await textDoc.getPage(1)).getTextContent();
  expect(
    textContent.items.map((i) => ("str" in i ? i.str : "")).join(" "),
  ).toContain("Quarterly results are ready for review.");
  await textDoc.destroy();
  await dialog.getByRole("button", { name: "元に戻す", exact: true }).click();
  await page.screenshot({ path: "test-results/pdf-organize-light.png" });
  await page.evaluate(() => document.documentElement.classList.add("dark"));
  await page.screenshot({ path: "test-results/pdf-organize-dark.png" });
  await dialog.getByLabel("ページ1を削除", { exact: true }).click();
  await dialog.getByLabel("ページ2を削除", { exact: true }).click();
  await expect(
    dialog.getByRole("button", { name: "PDF を保存", exact: true }),
  ).toBeDisabled();
});

test("端末のPDFを結合し、追加順の変更とファイル除外を反映する", async ({
  page,
}) => {
  await setup(page);
  await page.getByTestId("pdf-mode-tools").click();
  await page.getByRole("button", { name: /PDFを結合/ }).click();
  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByRole("button", { name: "PDF を保存", exact: true }),
  ).toBeDisabled();
  const extra = await PDFDocument.create();
  extra.addPage([321, 456]);
  extra.addPage([322, 457]);
  await page.getByLabel("結合するPDFを追加").setInputFiles({
    name: "追加資料.pdf",
    mimeType: "application/pdf",
    buffer: Buffer.from(await extra.save()),
  });
  await expect(dialog.getByTestId("pdf-merge-source")).toHaveCount(2);
  await dialog.getByLabel("追加資料.pdfを前へ", { exact: true }).click();
  const event = page.waitForEvent("download");
  await dialog.getByRole("button", { name: "PDF を保存", exact: true }).click();
  const result = await PDFDocument.load(
    await readFile((await (await event).path())!),
  );
  expect(result.getPageCount()).toBe(4);
  expect(result.getPage(0).getWidth()).toBe(321);
  expect(result.getPage(1).getWidth()).toBe(322);
  expect(result.getPage(3).getRotation().angle).toBe(90);
  await dialog
    .getByLabel("追加資料.pdfを結合から外す", { exact: true })
    .click();
  await expect(
    dialog.getByRole("button", { name: "PDF を保存", exact: true }),
  ).toBeDisabled();
  await page.getByLabel("結合するPDFを追加").setInputFiles({
    name: "壊れた.pdf",
    mimeType: "application/pdf",
    buffer: Buffer.from("invalid"),
  });
  await expect(dialog.getByRole("alert")).toBeVisible();
  await expect(dialog.getByTestId("pdf-merge-source")).toHaveCount(1);
  await page.screenshot({ path: "test-results/pdf-merge-light.png" });
});

test("全回転・倍率で枠の移動方向とハンドルの画面方向を保ち、ページ外へ出さない", async () => {
  const { getDocument } = await import("pdfjs-dist/legacy/build/pdf.mjs");
  const doc = await getDocument({
    data: new Uint8Array(await fixture()),
    useSystemFonts: true,
  }).promise;
  try {
    for (const number of [1, 2]) {
      const page = await doc.getPage(number);
      for (const rotation of [0, 90, 180, 270])
        for (const scale of [0.33, 1, 2.25]) {
          const vp = page.getViewport({
            scale,
            rotation: (page.rotate + rotation) % 360,
          });
          const original = screenRect([100, 300, 250, 450], vp);
          const moved = editRegion(
            original,
            "move",
            12,
            -8,
            vp.width,
            vp.height,
          );
          const back = screenRect(pdfRect(moved, vp), vp);
          expect(back[0]).toBeCloseTo(original[0] + 12, 5);
          expect(back[1]).toBeCloseTo(original[1] - 8, 5);
          expect(back[2] - back[0]).toBeCloseTo(original[2] - original[0], 5);
          for (const handle of [
            "nw",
            "n",
            "ne",
            "e",
            "se",
            "s",
            "sw",
            "w",
            "move",
          ] as const) {
            const edge = editRegion(
              original,
              handle,
              100000,
              -100000,
              vp.width,
              vp.height,
            );
            expect(edge[0]).toBeGreaterThanOrEqual(0);
            expect(edge[1]).toBeGreaterThanOrEqual(0);
            expect(edge[2]).toBeLessThanOrEqual(vp.width);
            expect(edge[3]).toBeLessThanOrEqual(vp.height);
            expect(edge[2]).toBeGreaterThan(edge[0]);
            expect(edge[3]).toBeGreaterThan(edge[1]);
          }
        }
    }
  } finally {
    await doc.destroy();
  }
});

test("行・セルの細粒度座標は回転とズームでも同じ文字を指す", async ({
  page,
}) => {
  await setup(page);
  await page.getByTestId("pdf-mode-select").click();
  await page.getByRole("button", { name: "表セル", exact: true }).click();
  let saved: number[] | null = null;
  for (let turn = 0; turn < 4; turn++) {
    if (turn) {
      await page.getByTestId("chrome-menu").click();
      await page.getByTestId("pdf-rotate").click();
    }
    await page
      .getByRole("button", { name: "セルを選択: 1200", exact: true })
      .click();
    await expect(page.getByTestId("pdf-excerpt-card")).toContainText("1200");
    const bbox = JSON.parse(
      (await page.getByTestId("pdf-region-box").getAttribute("data-bbox"))!,
    ) as number[];
    if (saved) bbox.forEach((v, i) => expect(v).toBeCloseTo(saved![i], 0));
    else saved = bbox;
    await page.getByRole("button", { name: "すべて解除", exact: true }).click();
    if (turn === 1)
      await page.getByRole("button", { name: "拡大（⌘＋）" }).click();
  }
});

for (const theme of ["light", "dark"] as const)
  test(`狭幅・${theme}でもページ編集の操作と保存ボタンが画面内に収まる`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await setup(page);
    if (theme === "dark")
      await page.evaluate(() => document.documentElement.classList.add("dark"));
    await page.getByTestId("pdf-mode-tools").click();
    await page.getByRole("button", { name: /ページを整理/ }).click();
    const dialog = page.getByRole("dialog", {
      name: "ページを、思いどおりに。",
      exact: true,
    });
    await expect(dialog).toBeVisible();
    await expect
      .poll(async () => {
        const r = (await dialog.boundingBox())!;
        return r.y + r.height;
      })
      .toBeLessThanOrEqual(845);
    const r = (await dialog.boundingBox())!;
    expect(r.width).toBeLessThanOrEqual(390);
    await dialog.getByLabel("ページ2を前へ", { exact: true }).click();
    await expect(
      dialog.getByRole("button", { name: "PDF を保存", exact: true }),
    ).toBeInViewport();
    await page.screenshot({
      path: `test-results/pdf-organize-mobile-${theme}.png`,
    });
  });

test("ドライブのPDFも結合リストへ追加できる", async ({ page }) => {
  await setup(page);
  const other = "a0000000-0000-0000-0000-000000000002";
  await page.route("**/api/nodes?**", (route) =>
    route.fulfill({
      json: {
        items: [
          {
            id: other,
            kind: "file",
            name: "ドライブ資料.pdf",
            content_type: "application/pdf",
            version: 1,
          },
        ],
        next_cursor: null,
      },
    }),
  );
  await page.getByTestId("pdf-mode-tools").click();
  await page.getByRole("button", { name: /PDFを結合/ }).click();
  await page.getByRole("button", { name: "ドライブから", exact: true }).click();
  await page
    .getByRole("button", { name: "ドライブ資料.pdf", exact: true })
    .click();
  await expect(page.getByTestId("pdf-merge-source")).toHaveCount(2);
  const download = page.waitForEvent("download");
  await page
    .getByRole("dialog", { name: "資料を、ひとつのPDFに。", exact: true })
    .getByRole("button", { name: "PDF を保存", exact: true })
    .click();
  const out = await PDFDocument.load(
    await readFile((await (await download).path())!),
  );
  expect(out.getPageCount()).toBe(4);
});

test("ページ順の検証と書き出し中の認証変更を拒否する", async () => {
  expect(() => validatePageOrder([], 2)).toThrow();
  expect(() => validatePageOrder([0, 0], 2)).toThrow();
  expect(() => validatePageOrder([2], 2)).toThrow();
  let release!: (v: Uint8Array) => void;
  const bytes = new Promise<Uint8Array>((resolve) => {
    release = resolve;
  });
  const doc = {
    numPages: 2,
    getData: () => bytes,
  } as unknown as PDFDocumentProxy;
  bindPdfSource(doc, id, 1);
  const pending = organizePdf(doc, [1, 0]);
  clearUserStorage();
  release(new Uint8Array(await fixture()));
  await expect(pending).rejects.toThrow("ログイン状態が変わりました");
});

test("書式で分かれた表セルを集約し、空セルの列をCSVでも保持する", async ({
  page,
}) => {
  await setup(page);
  const pdf = await PDFDocument.create();
  const p = pdf.addPage([400, 400]);
  const regular = await pdf.embedFont(StandardFonts.Helvetica);
  const bold = await pdf.embedFont(StandardFonts.HelveticaBold);
  p.drawText("Product", { x: 30, y: 300, size: 12, font: regular });
  p.drawText(" A", { x: 80, y: 300, size: 12, font: bold });
  p.drawText("B", { x: 230, y: 300, size: 12, font: regular });
  p.drawText("D", { x: 230, y: 260, size: 12, font: regular });
  const bytes = Buffer.from(await pdf.save());
  await page.route("**/fixture.pdf", (route) =>
    route.fulfill({ contentType: "application/pdf", body: bytes }),
  );
  await page.route(`**/api/files/${id}/versions/1/blocks*`, (route) =>
    route.fulfill({
      json: {
        blocks: [
          {
            ordinal: 0,
            type: "table",
            page: 1,
            text: "| Product A | B |\n| --- | --- |\n|  | D |",
            prov: [
              { page: 1, bbox: [20, 320, 330, 240], origin: "bottom_left" },
            ],
          },
        ],
        next_from: null,
      },
    }),
  );
  await page.reload();
  await page.getByTestId("pdf-mode-select").click();
  await page.getByRole("button", { name: "表セル", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "セルを選択: Product A", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "セルを選択: Product", exact: true }),
  ).toHaveCount(0);
  await dragRegion(page, 1, [20 / 400, 70 / 400, 340 / 400, 180 / 400]);
  await expect(page.getByTestId("pdf-excerpt-card").locator("pre")).toHaveText(
    "Product A\tB\n\tD",
  );
  const event = page.waitForEvent("download");
  await page.getByRole("button", { name: "CSV", exact: true }).click();
  const csv = await readFile((await (await event).path())!, "utf8");
  expect(csv).toContain('"Product A","B"');
  expect(csv).toContain('"","D"');
});

test("セル選択の矩形化とPDF読み込みは上限を展開前に確認する", async () => {
  expect(() => checkPdfBudget(60 * 1024 * 1024, 50 * 1024 * 1024)).toThrow(
    "合計100MB",
  );
  expect(() => checkPdfBudget(0, 51 * 1024 * 1024)).toThrow("50MB");
  let cancelled = false;
  const response = new Response(
    new ReadableStream({
      pull(c) {
        c.enqueue(new Uint8Array(10));
      },
      cancel() {
        cancelled = true;
      },
    }),
  );
  await expect(readPdfResponse(response, 12)).rejects.toThrow("上限");
  expect(cancelled).toBe(true);
  const units = Array.from({ length: 10000 }, (_, i) => ({
    kind: "cell" as const,
    row: i < 5000 ? 0 : i - 4999,
    column: i < 5000 ? i : 0,
    table: 0,
    text: "x",
    bbox: [0, 0, 10, 10] as [number, number, number, number],
  }));
  expect(() => fineContent(units, [0, 0, 10, 10], "cell")).toThrow(
    "大きすぎます",
  );
  const sparse = [0, 600].map((row) => ({
    kind: "cell" as const,
    row,
    column: 0,
    table: 0,
    text: "x",
    bbox: [0, 0, 10, 10] as [number, number, number, number],
  }));
  expect(() => fineContent(sparse, [0, 0, 10, 10], "cell")).toThrow(
    "大きすぎます",
  );
});

test("削除ページへの内部リンクから本文を出力へコピーしない", async () => {
  const { decodePDFRawStream, PDFRawStream } = await import("pdf-lib");
  const pdf = await PDFDocument.create();
  const kept = pdf.addPage([200, 200]),
    deleted = pdf.addPage([300, 300]);
  const secret = pdf.context.flateStream("DELETED_SECRET_CONTENT");
  deleted.node.set(PDFName.of("Contents"), pdf.context.register(secret));
  const link = pdf.context.obj({
    Type: PDFName.of("Annot"),
    Subtype: PDFName.of("Link"),
    Rect: [0, 0, 100, 100],
    Dest: [deleted.ref, PDFName.of("Fit")],
  });
  kept.node.set(
    PDFName.of("Annots"),
    pdf.context.obj([pdf.context.register(link)]),
  );
  const bytes = await pdf.save();
  const doc = {
    numPages: 2,
    getData: async () => bytes,
  } as unknown as PDFDocumentProxy;
  const result = await PDFDocument.load(await organizePdf(doc, [0]));
  expect(result.getPageCount()).toBe(1);
  for (const [, object] of result.context.enumerateIndirectObjects())
    if (object instanceof PDFRawStream)
      expect(
        Buffer.from(decodePDFRawStream(object).decode()).toString(),
      ).not.toContain("DELETED_SECRET_CONTENT");
});

test("自由範囲の枠はセルモードへ切り替えた後も調整できる", async ({ page }) => {
  await setup(page);
  await page.getByTestId("pdf-mode-select").click();
  await dragRegion(page, 1, [35 / 550, 45 / 740, 425 / 550, 95 / 740]);
  const box = page.getByTestId("pdf-region-box");
  const before = await box.getAttribute("data-bbox");
  await page.getByRole("button", { name: "表セル", exact: true }).click();
  await box.locator("[data-region-move]").focus();
  await page.keyboard.press("ArrowRight");
  await page.keyboard.press("Enter");
  await expect(box).not.toHaveAttribute("data-bbox", before!);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
});

test("削除ページだけの画像を共有・入れ子のリソースから引き継がない", async () => {
  for (const nested of [false, true]) {
    const pdf = await PDFDocument.create();
    const kept = pdf.addPage([200, 200]),
      deleted = pdf.addPage([200, 200]);
    const image = (bytes: number[]) =>
      pdf.context.register(
        pdf.context.stream(new Uint8Array(bytes), {
          Type: "XObject",
          Subtype: "Image",
          Width: 1,
          Height: 1,
          ColorSpace: "DeviceRGB",
          BitsPerComponent: 8,
        }),
      );
    const objects = pdf.context.obj({
      Keep: image([255, 0, 0]),
      Secret: image([17, 23, 42]),
    });
    const resources = pdf.context.obj({ XObject: objects });
    if (nested) {
      const form = pdf.context.flateStream("/Keep Do", {
        Type: "XObject",
        Subtype: "Form",
        BBox: [0, 0, 100, 100],
        Resources: resources,
      });
      objects.set(PDFName.of("VisibleForm"), pdf.context.register(form));
    }
    kept.node.set(PDFName.of("Resources"), resources);
    deleted.node.set(PDFName.of("Resources"), resources);
    kept.node.set(
      PDFName.of("Contents"),
      pdf.context.register(
        pdf.context.flateStream(nested ? "/VisibleForm Do" : "/Keep Do"),
      ),
    );
    deleted.node.set(
      PDFName.of("Contents"),
      pdf.context.register(pdf.context.flateStream("/Secret Do")),
    );
    const bytes = await pdf.save();
    const result = await PDFDocument.load(
      await organizePdf(
        {
          numPages: 2,
          getData: async () => bytes,
        } as unknown as PDFDocumentProxy,
        [0],
      ),
    );
    const { PDFDict } = await import("pdf-lib");
    const out = result
      .getPage(0)
      .node.Resources()!
      .lookup(PDFName.of("XObject"), PDFDict);
    expect(out.has(PDFName.of("Secret"))).toBe(false);
    expect(out.has(PDFName.of(nested ? "VisibleForm" : "Keep"))).toBe(true);
  }
});

test("残すページへの内部リンクを保存後の実ページ参照へ向け直す", async () => {
  const pdf = await PDFDocument.create();
  const first = pdf.addPage([200, 200]),
    second = pdf.addPage([200, 200]);
  pdf.addPage([200, 200]);
  const annotation = pdf.context.obj({
    Type: PDFName.of("Annot"),
    Subtype: PDFName.of("Link"),
    Rect: [0, 0, 100, 100],
    Dest: [second.ref, PDFName.of("Fit")],
  });
  first.node.set(
    PDFName.of("Annots"),
    pdf.context.obj([pdf.context.register(annotation)]),
  );
  const bytes = await pdf.save();
  const result = await PDFDocument.load(
    await organizePdf(
      {
        numPages: 3,
        getData: async () => bytes,
      } as unknown as PDFDocumentProxy,
      [0, 1],
    ),
  );
  const { PDFArray, PDFDict } = await import("pdf-lib");
  const annots = result.getPage(0).node.lookup(PDFName.of("Annots"), PDFArray);
  const link = annots.lookup(0, PDFDict);
  const dest = link.lookup(PDFName.of("Dest"), PDFArray);
  expect(dest.get(0).toString()).toBe(result.getPage(1).ref.toString());
});

test("結合する各PDFの内部リンクを実際の出力ページへ向け直す", async () => {
  const docs: PDFDocumentProxy[] = [];
  for (let i = 0; i < 2; i++) {
    const pdf = await PDFDocument.create();
    const first = pdf.addPage([200, 200]);
    const second = pdf.addPage([200, 200]);
    const annotation = pdf.context.obj({
      Type: PDFName.of("Annot"),
      Subtype: PDFName.of("Link"),
      Rect: [0, 0, 100, 100],
      ...(i === 0
        ? { Dest: [second.ref, PDFName.of("Fit")] }
        : { A: { S: PDFName.of("GoTo"), D: [second.ref, PDFName.of("Fit")] } }),
    });
    first.node.set(
      PDFName.of("Annots"),
      pdf.context.obj([pdf.context.register(annotation)]),
    );
    const bytes = await pdf.save();
    docs.push({
      numPages: 2,
      getData: async () => bytes,
    } as unknown as PDFDocumentProxy);
  }
  const result = await PDFDocument.load(await mergePdfs(docs));
  const { PDFArray, PDFDict } = await import("pdf-lib");
  expect(result.getPageCount()).toBe(4);
  for (let i = 0; i < 2; i++) {
    const annots = result
      .getPage(i * 2)
      .node.lookup(PDFName.of("Annots"), PDFArray);
    const link = annots.lookup(0, PDFDict);
    const dest =
      i === 0
        ? link.lookup(PDFName.of("Dest"), PDFArray)
        : link
            .lookup(PDFName.of("A"), PDFDict)
            .lookup(PDFName.of("D"), PDFArray);
    expect(dest.get(0).toString()).toBe(
      result.getPage(i * 2 + 1).ref.toString(),
    );
  }
});

test("大量の同一Contents参照でも引数展開による保存失敗を起こさない", async () => {
  const pdf = await PDFDocument.create();
  const kept = pdf.addPage([200, 200]);
  pdf.addPage([200, 200]);
  const empty = pdf.context.register(pdf.context.flateStream(""));
  kept.node.set(
    PDFName.of("Contents"),
    pdf.context.obj(Array(200000).fill(empty)),
  );
  const bytes = await pdf.save();
  const result = await PDFDocument.load(
    await organizePdf(
      {
        numPages: 2,
        getData: async () => bytes,
      } as unknown as PDFDocumentProxy,
      [0],
    ),
  );
  expect(result.getPageCount()).toBe(1);
});

test("離れた別セルのAとBをOCRセルABへまとめない", async ({ page }) => {
  await setup(page);
  const pdf = await PDFDocument.create();
  const p = pdf.addPage([400, 400]);
  p.drawText("A", { x: 30, y: 300, size: 12 });
  p.drawText("B", { x: 230, y: 300, size: 12 });
  const bytes = Buffer.from(await pdf.save());
  await page.route("**/fixture.pdf", (route) =>
    route.fulfill({ contentType: "application/pdf", body: bytes }),
  );
  await page.route(`**/api/files/${id}/versions/1/blocks*`, (route) =>
    route.fulfill({
      json: {
        blocks: [
          {
            ordinal: 0,
            type: "table",
            page: 1,
            text: "| A | B | AB |\n| --- | --- | --- |",
            prov: [
              { page: 1, bbox: [20, 320, 390, 280], origin: "bottom_left" },
            ],
          },
        ],
        next_from: null,
      },
    }),
  );
  await page.reload();
  await page.getByTestId("pdf-mode-select").click();
  await page.getByRole("button", { name: "表セル", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "セルを選択: A", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "セルを選択: B", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "セルを選択: AB", exact: true }),
  ).toHaveCount(0);
});

test("分割Contentsと共有Font辞書でも使用中の書体を残す", async () => {
  const { PDFDict } = await import("pdf-lib");
  const pdf = await PDFDocument.create();
  const a = pdf.addPage([200, 200]),
    b = pdf.addPage([200, 200]);
  pdf.addPage([200, 200]);
  const font1 = pdf.context.register(
    pdf.context.obj({
      Type: "Font",
      Subtype: "Type1",
      BaseFont: "Courier-Bold",
    }),
  );
  const font2 = pdf.context.register(
    pdf.context.obj({ Type: "Font", Subtype: "Type1", BaseFont: "Symbol" }),
  );
  const fonts = pdf.context.register(pdf.context.obj({ F1: font1, F2: font2 }));
  a.node.set(PDFName.of("Resources"), pdf.context.obj({ Font: fonts }));
  b.node.set(PDFName.of("Resources"), pdf.context.obj({ Font: fonts }));
  a.node.set(
    PDFName.of("Contents"),
    pdf.context.obj([
      pdf.context.register(pdf.context.flateStream("BT /F1 12 ")),
      pdf.context.register(
        pdf.context.flateStream("Tf 20 50 Td (HELLO) Tj ET"),
      ),
    ]),
  );
  b.node.set(
    PDFName.of("Contents"),
    pdf.context.register(
      pdf.context.flateStream("BT /F2 12 Tf 20 50 Td (A) Tj ET"),
    ),
  );
  const bytes = await pdf.save();
  const result = await PDFDocument.load(
    await organizePdf(
      {
        numPages: 3,
        getData: async () => bytes,
      } as unknown as PDFDocumentProxy,
      [0, 1],
    ),
  );
  for (const page of result.getPages()) {
    const font = page.node.Resources()!.lookup(PDFName.of("Font"), PDFDict);
    expect(font.has(PDFName.of("F1"))).toBe(true);
    expect(font.has(PDFName.of("F2"))).toBe(true);
  }
});

test("ページ本体にResourcesがなくても注釈の未使用画像を取り除く", async () => {
  const pdf = await PDFDocument.create();
  const a = pdf.addPage([200, 200]),
    b = pdf.addPage([200, 200]);
  a.node.delete(PDFName.of("Resources"));
  const secret = pdf.context.register(
    pdf.context.stream(new Uint8Array([17, 23, 42]), {
      Type: "XObject",
      Subtype: "Image",
      Width: 1,
      Height: 1,
      ColorSpace: "DeviceRGB",
      BitsPerComponent: 8,
    }),
  );
  const resources = pdf.context.obj({
    XObject: pdf.context.obj({ Secret: secret }),
  });
  const ap = pdf.context.register(
    pdf.context.flateStream("0 0 10 10 re S", {
      Type: "XObject",
      Subtype: "Form",
      BBox: [0, 0, 100, 100],
      Resources: resources,
    }),
  );
  const stamp = pdf.context.register(
    pdf.context.obj({
      Type: "Annot",
      Subtype: "Stamp",
      Rect: [0, 0, 100, 100],
      AP: { N: ap },
    }),
  );
  a.node.set(PDFName.of("Annots"), pdf.context.obj([stamp]));
  b.node.set(PDFName.of("Resources"), resources);
  b.node.set(
    PDFName.of("Contents"),
    pdf.context.register(pdf.context.flateStream("/Secret Do")),
  );
  const bytes = await pdf.save();
  const result = await PDFDocument.load(
    await organizePdf(
      {
        numPages: 2,
        getData: async () => bytes,
      } as unknown as PDFDocumentProxy,
      [0],
    ),
  );
  const { PDFRawStream } = await import("pdf-lib");
  expect(
    result.context
      .enumerateIndirectObjects()
      .filter(
        ([, o]) =>
          o instanceof PDFRawStream &&
          o.dict.get(PDFName.of("Subtype")) === PDFName.of("Image"),
      ),
  ).toHaveLength(0);
});

test("既定の色空間を保持し、残したページの色を変えない", async () => {
  const pdf = await PDFDocument.create();
  const p = pdf.addPage([200, 200]);
  pdf.addPage([200, 200]);
  p.node.set(
    PDFName.of("Resources"),
    pdf.context.obj({
      ColorSpace: {
        DefaultRGB: [
          "CalRGB",
          { WhitePoint: [0.9505, 1, 1.089], Gamma: [3, 3, 3] },
        ],
        DefaultGray: ["CalGray", { WhitePoint: [0.9505, 1, 1.089], Gamma: 3 }],
      },
    }),
  );
  p.node.set(
    PDFName.of("Contents"),
    pdf.context.register(
      pdf.context.flateStream("0.5 0.5 0.5 rg 10 10 100 100 re f"),
    ),
  );
  const bytes = await pdf.save();
  const result = await PDFDocument.load(
    await organizePdf(
      {
        numPages: 2,
        getData: async () => bytes,
      } as unknown as PDFDocumentProxy,
      [0],
    ),
  );
  const { PDFDict } = await import("pdf-lib");
  const spaces = result
    .getPage(0)
    .node.Resources()!
    .lookup(PDFName.of("ColorSpace"), PDFDict);
  expect(spaces.has(PDFName.of("DefaultRGB"))).toBe(true);
  expect(spaces.has(PDFName.of("DefaultGray"))).toBe(true);
});

test("書式が異なるABセルを、OCR由来の別列AとBへ割り当てない", async ({
  page,
}) => {
  await setup(page);
  const pdf = await PDFDocument.create();
  const p = pdf.addPage([400, 400]);
  const regular = await pdf.embedFont(StandardFonts.Helvetica);
  const bold = await pdf.embedFont(StandardFonts.HelveticaBold);
  p.drawText("A", { x: 30, y: 300, size: 12, font: regular });
  p.drawText("B", {
    x: 30 + regular.widthOfTextAtSize("A", 12),
    y: 300,
    size: 12,
    font: bold,
  });
  const bytes = Buffer.from(await pdf.save());
  await page.route("**/fixture.pdf", (route) =>
    route.fulfill({ contentType: "application/pdf", body: bytes }),
  );
  await page.route(`**/api/files/${id}/versions/1/blocks*`, (route) =>
    route.fulfill({
      json: {
        blocks: [
          {
            ordinal: 0,
            type: "table",
            page: 1,
            text: "| AB | A | B |\n| --- | --- | --- |",
            prov: [
              { page: 1, bbox: [20, 320, 390, 280], origin: "bottom_left" },
            ],
          },
        ],
        next_from: null,
      },
    }),
  );
  await page.reload();
  await page.getByTestId("pdf-mode-select").click();
  await page.getByRole("button", { name: "表セル", exact: true }).click();
  await expect(page.getByTestId("pdf-fine-unit")).toHaveCount(0);
  await page.getByRole("button", { name: "自由範囲", exact: true }).click();
  await dragRegion(page, 1, [20 / 400, 70 / 400, 390 / 400, 130 / 400]);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(1);
});

test("ページ並べ替えで中間Pagesから継承した寸法・回転・文字リソースを保つ", async () => {
  const pdf = await PDFDocument.create();
  const p = pdf.addPage([240, 360]);
  p.drawText("Inherited text");
  const second = pdf.addPage([100, 150]);
  const root = pdf.catalog.Pages();
  const child = pdf.context.obj({
    Type: PDFName.of("Pages"),
    Parent: pdf.catalog.get(PDFName.of("Pages")),
    Kids: [p.ref],
    Count: 1,
    Resources: p.node.Resources(),
    MediaBox: [0, 0, 240, 360],
    CropBox: [10, 20, 220, 340],
    Rotate: 90,
  });
  const childRef = pdf.context.register(child);
  p.node.set(PDFName.of("Parent"), childRef);
  for (const name of ["Resources", "MediaBox", "CropBox", "Rotate"])
    p.node.delete(PDFName.of(name));
  root.set(PDFName.of("Kids"), pdf.context.obj([childRef, second.ref]));
  const bytes = await pdf.save();
  const out = await PDFDocument.load(
    await organizePdf(
      {
        numPages: 2,
        getData: async () => bytes,
      } as unknown as PDFDocumentProxy,
      [1, 0],
    ),
  );
  const kept = out.getPage(1);
  expect(kept.getSize()).toEqual({ width: 240, height: 360 });
  expect(kept.getCropBox()).toEqual({ x: 10, y: 20, width: 210, height: 320 });
  expect(kept.getRotation().angle).toBe(90);
  expect(kept.node.Resources()!.has(PDFName.of("Font"))).toBe(true);
});

test("メッシュShadingストリームの参照する色空間をページ削除後も保つ", async () => {
  const pdf = await PDFDocument.create();
  const p = pdf.addPage([200, 200]);
  pdf.addPage([200, 200]);
  const shading = pdf.context.register(
    pdf.context.flateStream(new Uint8Array(), {
      ShadingType: 4,
      ColorSpace: PDFName.of("CS1"),
      BitsPerCoordinate: 8,
      BitsPerComponent: 8,
      BitsPerFlag: 8,
      Decode: [0, 200, 0, 200, 0, 1, 0, 1, 0, 1],
    }),
  );
  p.node.set(
    PDFName.of("Resources"),
    pdf.context.obj({
      ColorSpace: { CS1: PDFName.of("DeviceRGB") },
      Pattern: {
        P1: { Type: PDFName.of("Pattern"), PatternType: 2, Shading: shading },
      },
    }),
  );
  p.node.set(
    PDFName.of("Contents"),
    pdf.context.register(
      pdf.context.flateStream("/Pattern cs /P1 scn 0 0 100 100 re f"),
    ),
  );
  const bytes = await pdf.save();
  const out = await PDFDocument.load(
    await organizePdf(
      {
        numPages: 2,
        getData: async () => bytes,
      } as unknown as PDFDocumentProxy,
      [0],
    ),
  );
  const { PDFDict } = await import("pdf-lib");
  expect(
    out
      .getPage(0)
      .node.Resources()!
      .lookup(PDFName.of("ColorSpace"), PDFDict)
      .has(PDFName.of("CS1")),
  ).toBe(true);
});

test("セル座標を丸め、出典の上限を選択時に確認する", () => {
  const units = Array.from({ length: 50 }, (_, i) => ({
    kind: "cell" as const,
    row: Math.floor(i / 5),
    column: i % 5,
    table: 0,
    text: "x",
    bbox: [1.123456789, 2.987654321, 3.123456789, 4.987654321] as [
      number,
      number,
      number,
      number,
    ],
  }));
  const result = fineContent(units, [0, 0, 10, 10], "cell")!;
  expect(result.cells![0].bbox).toEqual([1.12, 2.99, 3.12, 4.99]);
  const context = excerptContext(id, "cells.pdf", 1, [
    { ...result, id: "c", page: 1, bbox: [0, 0, 10, 10], blockOrdinals: [0] },
  ]);
  expect(JSON.stringify(context.locator).length).toBeLessThanOrEqual(4096);
  const many = Array.from({ length: 100 }, (_, i) => ({
    ...units[0],
    row: Math.floor(i / 10),
    column: i % 10,
  }));
  expect(() => fineContent(many, [0, 0, 10, 10], "cell")).toThrow(
    "大きすぎます",
  );
});

test("結合したPDFにフォームの保存済み表示値を残す", async ({ page }) => {
  const pdf = await PDFDocument.create();
  const p = pdf.addPage([200, 200]);
  const field = pdf.getForm().createTextField("invoice.amount");
  field.setText("123");
  field.addToPage(p, { x: 20, y: 100, width: 100, height: 30 });
  const other = await PDFDocument.create();
  other.addPage([200, 200]);
  const inputs = await Promise.all(
    [pdf, other].map(async (d) => {
      const bytes = await d.save();
      return {
        numPages: 1,
        getData: async () => bytes,
      } as unknown as PDFDocumentProxy;
    }),
  );
  const merged = await mergePdfs(inputs);
  const output = await PDFDocument.load(merged);
  expect(output.getForm().getFields()).toHaveLength(0);
  await setup(page, false, true);
  await page.route("**/fixture.pdf", (r) =>
    r.fulfill({ contentType: "application/pdf", body: Buffer.from(merged) }),
  );
  await page.reload();
  await expect(page.locator('[data-page="1"] .pdf-text-layer')).toContainText(
    "123",
  );
});

test("非表示または特殊な外観の入力欄を結合で露出・変形させない", async () => {
  for (const mode of [
    "hidden",
    "noView",
    "optional",
    "bbox",
    "matrix",
  ] as const) {
    const pdf = await PDFDocument.create();
    const p = pdf.addPage([200, 200]);
    const field = pdf.getForm().createTextField("private.amount");
    field.setText("secret");
    field.addToPage(p, { x: 20, y: 100, width: 100, height: 30 });
    const widget = field.acroField.getWidgets()[0];
    if (mode === "hidden" || mode === "noView")
      widget.setFlags(mode === "hidden" ? 2 : 32);
    else if (mode === "optional") {
      const layer = pdf.context.register(
        pdf.context.obj({
          Type: PDFName.of("OCG"),
          Name: PDFString.of("Hidden"),
        }),
      );
      widget.dict.set(PDFName.of("OC"), layer);
      pdf.catalog.set(
        PDFName.of("OCProperties"),
        pdf.context.obj({ OCGs: [layer], D: { OFF: [layer] } }),
      );
    } else {
      const normal = widget.getAppearances()!.normal;
      const { PDFStream } = await import("pdf-lib");
      expect(normal).toBeInstanceOf(PDFStream);
      if (normal instanceof PDFStream)
        normal.dict.set(
          PDFName.of(mode === "bbox" ? "BBox" : "Matrix"),
          pdf.context.obj(
            mode === "bbox" ? [10, 20, 60, 40] : [2, 0, 0, 2, 0, 0],
          ),
        );
    }
    const bytes = await pdf.save({ updateFieldAppearances: false });
    const doc = {
      numPages: 1,
      getData: async () => bytes,
    } as unknown as PDFDocumentProxy;
    await expect(mergePdfs([doc, doc]), mode).rejects.toThrow(
      "入力欄の表示には結合が未対応",
    );
  }
});

test("セル抜粋の拡大が出典上限を超えても旧枠・本文を保持する", async ({
  page,
}) => {
  await setup(page);
  const pdf = await PDFDocument.create();
  const p = pdf.addPage([600, 800]);
  const rows = Array.from({ length: 8 }, (_, r) =>
    Array.from({ length: 7 }, (_, c) => `C${r}${c}`),
  );
  rows.forEach((row, r) =>
    row.forEach((text, c) =>
      p.drawText(text, { x: 50 + c * 60, y: 700 - r * 40, size: 8 }),
    ),
  );
  const bytes = Buffer.from(await pdf.save());
  const text = rows.map((row) => `| ${row.join(" | ")} |`).join("\n");
  await page.route("**/fixture.pdf", (route) =>
    route.fulfill({ contentType: "application/pdf", body: bytes }),
  );
  await page.route(`**/api/files/${id}/versions/1/blocks*`, (route) =>
    route.fulfill({
      json: {
        blocks: [
          {
            ordinal: 0,
            type: "table",
            page: 1,
            text,
            prov: [
              { page: 1, bbox: [40, 720, 460, 400], origin: "bottom_left" },
            ],
          },
        ],
        next_from: null,
      },
    }),
  );
  await page.reload();
  await page.getByTestId("pdf-mode-select").click();
  await page.getByRole("button", { name: "表セル", exact: true }).click();
  await expect(page.getByTestId("pdf-fine-unit")).toHaveCount(56);
  await dragRegion(page, 1, [40 / 600, 80 / 800, 440 / 600, 240 / 800]);
  await dragRegion(page, 1, [40 / 600, 240 / 800, 440 / 600, 400 / 800]);
  const cards = page.getByTestId("pdf-excerpt-card");
  await expect(cards).toHaveCount(2);
  const box = page.getByTestId("pdf-region-box").nth(1);
  const before = await box.getAttribute("data-bbox");
  const oldText = await cards.nth(1).locator("pre").textContent();
  const handle = page.getByLabel("抜粋2の上を調整", { exact: true });
  await box.locator("[data-region-move]").focus();
  const bounds = (await handle.boundingBox())!;
  const layer = (await page.getByTestId("pdf-region-layer").boundingBox())!;
  await page.mouse.move(
    bounds.x + bounds.width / 2,
    bounds.y + bounds.height / 2,
  );
  await page.mouse.down();
  await page.mouse.move(
    bounds.x + bounds.width / 2,
    layer.y + (layer.height * 80) / 800,
    { steps: 5 },
  );
  await page.mouse.up();
  await expect(
    page.getByRole("alert").filter({
      hasText: "選択範囲が大きすぎます。範囲を小さくしてください。",
    }),
  ).toBeVisible();
  await expect(box).toHaveAttribute("data-bbox", before!);
  await expect(cards.nth(1).locator("pre")).toHaveText(oldText!);
});

test("名前付きDestとGoToを入力ごとのページ参照へ解決し、削除先を除く", async () => {
  const { PDFArray, PDFDict, PDFHexString } = await import("pdf-lib");
  const inputs: PDFDocumentProxy[] = [];
  for (const tree of [false, true]) {
    const pdf = await PDFDocument.create();
    const first = pdf.addPage([200, 200]);
    const second = pdf.addPage([200, 200]);
    const removed = pdf.addPage([200, 200]);
    const kept = pdf.context.obj({ D: [second.ref, PDFName.of("Fit")] });
    const gone = pdf.context.obj([removed.ref, PDFName.of("Fit")]);
    if (tree)
      pdf.catalog.set(
        PDFName.of("Names"),
        pdf.context.obj({
          Dests: {
            Kids: [
              pdf.context.register(
                pdf.context.obj({
                  Names: [
                    PDFHexString.fromText("chapter"),
                    kept,
                    PDFString.of("removed"),
                    gone,
                  ],
                }),
              ),
            ],
          },
        }),
      );
    else
      pdf.catalog.set(
        PDFName.of("Dests"),
        pdf.context.obj({ chapter: kept, removed: gone }),
      );
    const name = tree
      ? PDFHexString.fromText("chapter")
      : PDFName.of("chapter");
    const annots = [
      { Dest: name },
      { A: { S: PDFName.of("GoTo"), D: PDFString.of("chapter") } },
      { Dest: PDFString.of("removed") },
    ].map((dest) =>
      pdf.context.register(
        pdf.context.obj({
          Type: PDFName.of("Annot"),
          Subtype: PDFName.of("Link"),
          Rect: [0, 0, 50, 50],
          ...dest,
        }),
      ),
    );
    first.node.set(PDFName.of("Annots"), pdf.context.obj(annots));
    const bytes = await pdf.save();
    const doc = {
      numPages: 3,
      getData: async () => bytes,
    } as unknown as PDFDocumentProxy;
    inputs.push(doc);
    const subset = await PDFDocument.load(await organizePdf(doc, [1, 0]));
    const links = subset.getPage(1).node.lookup(PDFName.of("Annots"), PDFArray);
    expect(
      links
        .lookup(0, PDFDict)
        .lookup(PDFName.of("Dest"), PDFArray)
        .get(0)
        .toString(),
    ).toBe(subset.getPage(0).ref.toString());
    expect(links.lookup(2, PDFDict).has(PDFName.of("Dest"))).toBe(false);
  }
  const merged = await PDFDocument.load(await mergePdfs(inputs));
  for (const offset of [0, 3]) {
    const links = merged
      .getPage(offset)
      .node.lookup(PDFName.of("Annots"), PDFArray);
    const direct = links
      .lookup(0, PDFDict)
      .lookup(PDFName.of("Dest"), PDFArray);
    const action = links
      .lookup(1, PDFDict)
      .lookup(PDFName.of("A"), PDFDict)
      .lookup(PDFName.of("D"), PDFArray);
    expect(direct.get(0).toString()).toBe(
      merged.getPage(offset + 1).ref.toString(),
    );
    expect(action.get(0).toString()).toBe(
      merged.getPage(offset + 1).ref.toString(),
    );
  }
});

test("フォームがなくても非表示レイヤーを含むPDFの結合・抽出を拒否する", async () => {
  const pdf = await PDFDocument.create();
  pdf.addPage([200, 200]);
  pdf.addPage([200, 200]);
  const layer = pdf.context.register(
    pdf.context.obj({ Type: PDFName.of("OCG"), Name: PDFString.of("Hidden") }),
  );
  pdf.catalog.set(
    PDFName.of("OCProperties"),
    pdf.context.obj({ OCGs: [layer], D: { OFF: [layer] } }),
  );
  const bytes = await pdf.save();
  const doc = {
    numPages: 2,
    getData: async () => bytes,
  } as unknown as PDFDocumentProxy;
  await expect(mergePdfs([doc, doc])).rejects.toThrow("レイヤーの表示設定");
  await expect(organizePdf(doc, [0])).rejects.toThrow("レイヤーの表示設定");
});
