import { test, expect, type Page } from "@playwright/test";

/// ```ui の図のギャラリー（/reference/yui）をスマホ幅で開き、図が画面からはみ出さないことを確かめる。
/// 認証・バックエンド・LLM 不要（middleware は /reference を除外）＝ web 単体で回せる。
/// `SHOTS_DIR` を渡すと図ごとの PNG を保存する。
const SHOTS = process.env.SHOTS_DIR;

/// はみ出している要素の一覧（空なら収まっている）。
function overflows(page: Page) {
  return page.evaluate(() => {
    const vw = document.documentElement.clientWidth;
    const out: string[] = [];
    if (document.documentElement.scrollWidth > vw) out.push(`ページが横に ${document.documentElement.scrollWidth - vw}px はみ出す`);
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
    return out;
  });
}

for (const width of [390, 320]) {
  test.describe(`図のギャラリー（幅 ${width}px）`, () => {
    test.use({ viewport: { width, height: 844 }, isMobile: true, hasTouch: true });

    test("画面も図も横にはみ出さない", async ({ page }) => {
      await page.goto("/reference/yui");
      const sections = page.locator("section[data-testid^=yui-]");
      await expect(sections.first()).toBeVisible();
      const total = await sections.count();
      expect(total).toBeGreaterThan(30);
      // 図の描画部は遅れて読み込まれる。どの図も描き終わる（図か、描けなかったときの記述）まで待ち、
      // 描けなかった図が無いことを確かめる。
      await expect(page.locator('[data-testid="ui-figure"], [data-testid="ui-figure-fallback"]')).toHaveCount(total, {
        timeout: 30_000,
      });
      await expect(page.getByTestId("ui-figure-fallback")).toHaveCount(0);

      // SVG の図は最初の幅で描いてから、親の幅を測って描き直す。描き直しが終わるまで測り直す。
      await expect.poll(() => overflows(page), { timeout: 10_000 }).toEqual([]);

      if (SHOTS) {
        for (const sec of await sections.all()) {
          await sec.screenshot({ path: `${SHOTS}/${width}-${await sec.getAttribute("data-testid")}.png` });
        }
      }
    });
  });
}
