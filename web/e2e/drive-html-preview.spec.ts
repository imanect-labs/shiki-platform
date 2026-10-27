import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ドライブに置いた HTML は、開くと描画された姿で見える。スクリプトは動かず（sandbox ＋ CSP）、
/// 外部リンクは新しいタブで開くように、相対リンクは外される。「ソース」で中身のコードに切り替えられる。
test("ドライブの HTML を描画して表示し、スクリプトは動かさない", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");

  const fileName = `${uniqueName("レポート")}.html`;
  const html = `<!doctype html>
<html>
<head><style>h1 { color: rgb(200, 0, 0); }</style></head>
<body>
  <h1 id="title">描画された見出し</h1>
  <p><a id="jump" href="#bottom">一番下へ</a> <a id="ext" href="https://example.com/">外部リンク</a> <a id="rel" href="other.html">相対リンク</a></p>
  <svg width="200" height="40"><a id="svglink" xlink:href="https://example.com/"><text x="4" y="28">SVG リンク</text></a></svg>
  <div style="height: 2000px"></div>
  <p id="bottom">一番下</p>
  <script>document.getElementById("title").textContent = "スクリプトが動いた";</script>
</body>
</html>`;
  await page.locator('input[type="file"][multiple]').setInputFiles({
    name: fileName,
    mimeType: "text/html",
    buffer: Buffer.from(html),
  });
  await expect(page.getByText(fileName, { exact: true })).toBeVisible({ timeout: 20_000 });

  await page.getByText(fileName, { exact: true }).dblclick();
  await expect(page).toHaveURL(/\/file\//);
  await expect(page.getByTestId("text-preview")).toHaveAttribute("data-mode", "rendered");

  // 能力を絞った sandbox（スクリプト・同一オリジンを許さない）。
  const iframe = page.getByTestId("html-preview-frame");
  const sandbox = (await iframe.getAttribute("sandbox")) ?? "";
  expect(sandbox).not.toContain("allow-scripts");
  expect(sandbox).not.toContain("allow-same-origin");

  const frame = page.frameLocator('[data-testid="html-preview-frame"]');
  // CSS は効き、スクリプトは動かない。
  await expect(frame.locator("#title")).toHaveText("描画された見出し");
  await expect(frame.locator("#title")).toHaveCSS("color", "rgb(200, 0, 0)");
  await expect(frame.locator("script")).toHaveCount(0);
  await expect(frame.locator("#ext")).toHaveAttribute("target", "_blank");
  await expect(frame.locator("#rel")).not.toHaveAttribute("href", /.*/);
  // SVG のリンクは rel を持てず、押すと iframe の中が外部ページへ遷移するので外す。
  await frame.getByText("SVG リンク").click();
  await expect(frame.locator("#title")).toHaveText("描画された見出し");
  // ページ内リンクは文書の中でスクロールする（iframe がアプリのページへ遷移しない）。
  await frame.locator("#jump").click();
  await expect(frame.locator("#bottom")).toBeInViewport();
  await expect(frame.locator("#title")).toHaveText("描画された見出し");

  // ソースに切り替えるとコードが見え、戻すと描画に戻る。
  await page.getByTestId("html-view-source").click();
  await expect(page.getByTestId("text-preview")).toHaveAttribute("data-mode", "view");
  await expect(page.getByTestId("text-preview")).toContainText('<h1 id="title">');
  await page.getByTestId("html-view-rendered").click();
  await expect(frame.locator("#title")).toHaveText("描画された見出し");
});
