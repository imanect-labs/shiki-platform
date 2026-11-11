import {
  queuePdfAttachments,
  takePdfAttachments,
} from "../src/lib/pdf-attachment-handoff";
import { uploadNewFile } from "../src/lib/storage";
import { clearUserStorage } from "../src/lib/user-storage";
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
} from "../src/lib/selection-context";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { test, expect, type Page } from "@playwright/test";
import { PDFDocument, StandardFonts, degrees, rgb } from "pdf-lib";
import { searchDocument, buildPageText, findInText } from "../src/lib/pdf-text";
import { rectFromViewport, blockRect, tableCsv } from "../src/lib/pdf-excerpts";
import { boxToViewport, viewportDimensions } from "../src/lib/pdf-geometry";
import { parsePageRange } from "../src/components/pdf/pdf-utilities";
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
  return Buffer.from(await doc.save());
}
async function setup(page: Page, failedBlocks = false) {
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
          blocks: blocks.filter(
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
  // 回転後、同じ PDF 座標を再び選択（90度では x=y, y=x）。
  await page.screenshot({ path: "test-results/pdf-selection-light.png" });
  await page.getByTestId("chrome-menu").click();
  await page.getByTestId("pdf-rotate").click();
  await dragRegion(page, 1, [645 / 740, 35 / 550, 695 / 740, 425 / 550]);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(2);
  await expect(page.getByTestId("pdf-excerpt-card").nth(1)).toContainText(
    blocks[0].text,
  );
  await page.getByRole("button", { name: "拡大（⌘＋）" }).click();
  await dragRegion(page, 1, [645 / 740, 35 / 550, 695 / 740, 425 / 550]);
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(3);
  expect(source.reads()).toBe(before);
  await page.getByRole("button", { name: "抜粋2を削除" }).click();
  await expect(page.getByTestId("pdf-excerpt-card")).toHaveCount(2);
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
