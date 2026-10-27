import { test, expect } from "@playwright/test";

/// ```ui の図のギャラリー（/reference/yui）をスマホ幅で開き、図が画面からはみ出さないことを確かめる。
/// 認証・バックエンド・LLM 不要（middleware は /reference を除外）＝ web 単体で回せる。
/// `SHOTS_DIR` を渡すと図ごとの PNG を保存する。
const SHOTS = process.env.SHOTS_DIR;

for (const width of [390, 320]) {
  test.describe(`図のギャラリー（幅 ${width}px）`, () => {
    test.use({ viewport: { width, height: 844 }, isMobile: true, hasTouch: true });

    test("画面も図も横にはみ出さない", async ({ page }) => {
      await page.goto("/reference/yui");
      const sections = page.locator("section[data-testid^=yui-]");
      // 図の描画部は遅れて読み込まれる。全部の図が描かれてから測る。
      await expect(page.getByTestId("ui-figure")).toHaveCount(await sections.count(), { timeout: 60_000 });

      const result = await page.evaluate(() => {
        const vw = document.documentElement.clientWidth;
        const out: string[] = [];
        for (const sec of document.querySelectorAll<HTMLElement>("section[data-testid^=yui-]")) {
          const col = sec.getBoundingClientRect();
          for (const el of sec.querySelectorAll<HTMLElement | SVGElement>("*")) {
            const r = el.getBoundingClientRect();
            if (r.width === 0 || r.height === 0) continue;
            const name = `${sec.dataset.testid} ${el.tagName.toLowerCase()} 「${(el.textContent ?? "").slice(0, 20)}」`;
            // どの図も画面の外へは出ない。
            if (r.left < -0.5 || r.right > vw + 0.5) out.push(`${name} が画面の外に出る`);
            // 入力・ボタンを持つ図（HTML で描く）は本文の列からも出ない（SVG の図は内側の余白の分だけ広げてある）。
            if (el.closest("[data-yui=ask],[data-yui=plan],[data-yui=app],[data-yui=calc]")) {
              if (r.left < col.left - 0.5 || r.right > col.right + 0.5) out.push(`${name} が本文の列から出る`);
            }
          }
          // 図の下の操作ボタンは文字の途中で折り返さない（1 行の高さに収まる）。
          for (const b of sec.querySelectorAll<HTMLElement>("figcaption button")) {
            if (b.getBoundingClientRect().height > 30) out.push(`${sec.dataset.testid} の「${b.textContent}」が折り返す`);
          }
        }
        return { page: document.documentElement.scrollWidth - vw, out };
      });
      expect(result.page).toBeLessThanOrEqual(0);
      expect(result.out).toEqual([]);

      if (SHOTS) {
        for (const sec of await sections.all()) {
          await sec.screenshot({ path: `${SHOTS}/yui-${width}-${await sec.getAttribute("data-testid")}.png` });
        }
      }
    });
  });
}
