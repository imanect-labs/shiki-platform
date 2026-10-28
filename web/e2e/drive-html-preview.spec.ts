import { expect, test } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// ドライブに置いた HTML は、開くと描画された姿で見える。スクリプトは動かず（sandbox ＋ CSP）、
/// 外部リンクは新しいタブで開くように、相対リンクは外される。外部の画像は閲覧者が選ぶまで読み込まない。
/// 「ソース」で中身のコードに切り替えられる。
test("ドライブの HTML を描画して表示し、スクリプトは動かさない", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/drive");
  // 外部の画像は実際には取りに行かず、取りに来たかだけを数える。
  const external: string[] = [];
  await page.route("https://img.example/**", (route) => {
    external.push(route.request().url());
    return route.fulfill({
      contentType: "image/svg+xml",
      body: '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40"/></svg>',
    });
  });

  const fileName = `${uniqueName("レポート")}.html`;
  const html = `<!doctype html>
<html>
<head><style>h1 { color: rgb(200, 0, 0); }</style></head>
<body>
  <h1 id="title">描画された見出し</h1>
  <img id="remote" src="https://img.example/logo.svg" alt="外部画像">
  <p><a id="jump" href="#bottom">一番下へ</a> <a id="ext" href="https://example.com/">外部リンク</a> <a id="rel" href="other.html">相対リンク</a></p>
  <svg width="200" height="40"><a id="svglink" xlink:href="https://example.com/"><text x="4" y="28">SVG リンク</text></a></svg>
  <div><template shadowrootmode="open"><a id="dsd" href="https://example.com/">影のリンク</a></template></div>
  <iframe id="inner" srcdoc="<a href='https://example.com/'>入れ子</a>"></iframe>
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
  // 一覧の行の操作メニューから開く（アップロード直後はファイル名が進捗の表示にも出るため、名前の文字では掴まない）。
  const rowMenu = page.getByRole("button", { name: `「${fileName}」の操作` });
  await expect(rowMenu).toBeVisible({ timeout: 20_000 });
  await rowMenu.click();
  await page.getByRole("menuitem", { name: "開く" }).click();
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
  // 宣言的 Shadow DOM の中のリンクも書き換える（DOMParser は展開しないが iframe 側は展開する）。
  // 入れ子の srcdoc は中を掃除できないので、埋め込みごと外す。
  await expect(frame.locator("#dsd")).toHaveAttribute("target", "_blank");
  await expect(frame.locator("#dsd")).toHaveAttribute("rel", "noopener noreferrer");
  await expect(frame.locator("#inner")).toHaveCount(0);
  // SVG のリンクは rel を持てず、押すと iframe の中が外部ページへ遷移するので外す。
  await frame.getByText("SVG リンク").click({ force: true });
  await expect(frame.locator("#title")).toHaveText("描画された見出し");
  // ページ内リンクは文書の中でスクロールする（iframe がアプリのページへ遷移しない）。
  await frame.locator("#jump").click();
  await expect(frame.locator("#bottom")).toBeInViewport();
  await expect(frame.locator("#title")).toHaveText("描画された見出し");

  // ファイルの内容であることを常に示す。外部の画像は「表示する」を押すまで取りに行かない。
  await expect(page.getByTestId("html-preview-notice")).toBeVisible();
  expect(external).toHaveLength(0);
  await page.getByTestId("html-allow-external").click();
  await expect(frame.locator("#remote")).toHaveJSProperty("naturalWidth", 40);
  expect(external).toEqual(["https://img.example/logo.svg"]);
  await expect(page.getByTestId("html-allow-external")).toHaveCount(0);

  // ソースに切り替えるとコードが見え、戻すと描画に戻る。
  await page.getByTestId("html-view-source").click();
  await expect(page.getByTestId("text-preview")).toHaveAttribute("data-mode", "view");
  await expect(page.getByTestId("text-preview")).toContainText('<h1 id="title">');
  await page.getByTestId("html-view-rendered").click();
  await expect(frame.locator("#title")).toHaveText("描画された見出し");
});
