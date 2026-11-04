import { test, expect } from "@playwright/test";

/// 回答本文の Markdown ギャラリー（/reference/markdown）の描画検証（issue #1015）。
/// 認証・バックエンド・LLM 不要（middleware は /reference を除外）＝ web 単体で回せる。
/// `SHOTS_DIR` を渡すとギャラリー全体の PNG を保存する。
const SHOTS = process.env.SHOTS_DIR;

test.describe("回答本文の Markdown ギャラリー", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/reference/markdown");
  });

  test("約物に隣接する太字が閉じる（** が文字として残らない）", async ({ page }) => {
    const cell = page.getByTestId("markdown-cjk-bold");
    await expect(cell.locator("strong").first()).toHaveText("「具体的にどうやって検索しているのか（メカニズム）」");
    await expect(cell.locator("strong")).toHaveCount(4);
    await expect(cell).not.toContainText("**");
  });

  test("\\( \\)・\\[ \\]・$$ を数式にし、単独の $ とコードは残す", async ({ page }) => {
    const cell = page.getByTestId("markdown-math");
    // KaTeX は数式があるときだけ遅れて読み込む。
    await expect(cell.locator(".katex-display")).toHaveCount(2);
    // 行内: → ×2・c_i・E=mc^2（別行立て 2 つの内側の .katex も数える）。
    await expect(cell.locator(".katex")).toHaveCount(6);
    await expect(cell).toContainText("金額の $5 と $10 は数式にしない");
    await expect(cell.locator("code")).toHaveText("\\(x\\)");
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/markdown-gallery.png`, fullPage: true });
  });

  test("引用番号の直後の「。」を番号と同じ行に置く", async ({ page }) => {
    // 幅を少しずつ変え、番号がちょうど行末に来る幅でも、直後の約物だけが次の行頭に落ちないこと
    // （番号と約物を折り返さない塊にする）。落ちた幅の一覧を返す。
    const li = page.getByTestId("markdown-business").locator("li").nth(1);
    const broken = await li.evaluate((el) => {
      const chip = [...el.querySelectorAll("span")].find((s) => s.textContent === "2");
      const textNode = (() => {
        const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
        let n: Node | null;
        while ((n = walker.nextNode())) {
          if (chip && chip.compareDocumentPosition(n) & Node.DOCUMENT_POSITION_FOLLOWING && n.textContent?.includes("。"))
            return n;
        }
        return null;
      })();
      if (!chip || !textNode) return ["no-chip"];
      const out: number[] = [];
      for (let w = 160; w <= 700; w += 2) {
        el.style.width = `${w}px`;
        const range = document.createRange();
        const i = textNode.textContent!.indexOf("。");
        range.setStart(textNode, i);
        range.setEnd(textNode, i + 1);
        const p = range.getBoundingClientRect();
        const c = chip.getBoundingClientRect();
        // 同じ行なら「。」は番号の右にある。次の行頭に落ちると左（行頭）へ戻る。
        if (p.left < c.left) out.push(w);
      }
      el.style.width = "";
      return out;
    });
    expect(broken).toEqual([]);
  });
});
