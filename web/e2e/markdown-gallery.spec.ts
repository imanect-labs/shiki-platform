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

  test("区切りがリスト・引用・表・コードの中にあっても構造を壊さない", async ({ page }) => {
    const cell = page.getByTestId("markdown-math-edge");
    await expect(cell.locator(".katex").first()).toBeVisible();
    // リストは途中で切れない（箇条書き 2 項目・番号付き 3 項目のまま）。項目の中の \\[ \\] も式にする。
    await expect(cell.locator("ul > li")).toHaveCount(2);
    await expect(cell.locator("ul > li").first().locator(".katex-display")).toHaveCount(1);
    await expect(cell.locator("ol > li")).toHaveCount(3);
    await expect(cell.locator("ol")).toHaveCount(1);
    // 引用の中の式（1 行・複数行）は引用の中に描く。
    await expect(cell.locator("blockquote").nth(0).locator(".katex")).toHaveCount(1);
    await expect(cell.locator("blockquote").nth(1).locator(".katex")).toHaveCount(1);
    // 表の行はセルを失わない。
    await expect(cell.locator("tr")).toHaveCount(3);
    await expect(cell.locator("table .katex")).toHaveCount(1);
    // 隣り合う式は 2 つの式になる。文中の角括弧のエスケープは式にしない。
    await expect(cell.locator("p", { hasText: "隣り合う式" }).locator(".katex")).toHaveCount(2);
    await expect(cell).toContainText("[注1]・[config_file]");
    // コードの中の \\( は式の区切りにならず、後ろの式とも組にならない。本文の $$ も式を壊さない。
    const bre = cell.locator("p", { hasText: "BRE のグループ" });
    await expect(bre.locator("code")).toHaveText("\\(");
    await expect(bre.locator(".katex")).toHaveCount(2);
    await expect(bre).toContainText("PID は $$ で取れる");
    // コード（バッククォート 2 つのインラインコード・フェンス）は原文のまま（幅 0 の空白なども足さない）。
    await expect(cell.locator("code", { hasText: "`\\(x\\)`" })).toHaveCount(1);
    const fence = await cell.locator("pre code", { hasText: "find ." }).innerText();
    expect(fence).toContain("  \\( -name '*.log' -o -name '*.tmp' \\) -delete");
    expect(fence).toContain("echo \\(z\\) $$");
    expect(fence).not.toMatch(/[\u200B\uE000-\uF8FF]/);
    // 暗黙に閉じたフェンスの後ろの式も描く。書き損じ（赤字）は出ない。
    await expect(cell.locator("p", { hasText: "引用の後の" }).locator(".katex")).toHaveCount(1);
    await expect(page.locator(".katex-error")).toHaveCount(0);
  });

  test("引用の中で暗黙に閉じたフェンスの後ろにだけある数式も描く", async ({ page }) => {
    // この本文だけで数式の有無を判定する（ほかの本文の数式で読み込まれた KaTeX に頼らない）。
    const cell = page.getByTestId("markdown-math-after-quoted-fence");
    await expect(cell.locator("blockquote pre code")).toHaveText("code");
    await expect(cell.locator("p", { hasText: "引用の後の" }).locator(".katex")).toHaveCount(1);
  });

  test("式の大きさに上限があり、行内の式は自分の箱の外に描かない", async ({ page }) => {
    const cell = page.getByTestId("markdown-math-edge");
    const p = cell.locator("p", { hasText: "大きさの上限" });
    await expect(p.locator(".katex")).toHaveCount(2);
    const [cellBox, ruleBox] = await Promise.all([cell.boundingBox(), p.locator(".katex").first().boundingBox()]);
    // \rule{500em} は maxSize（10em）で切り詰められ、本文の幅を押し広げない。
    expect(ruleBox!.width).toBeLessThan(cellBox!.width);
    // \\rlap の中身は式の箱（幅 0）の外に出るので、clip-path か横スクロールの箱で切り取られる。
    const clipped = await p.locator(".katex").nth(1).evaluate((el) => {
      const s = getComputedStyle(el.parentElement!);
      return s.clipPath.includes("inset") || s.overflowX !== "visible";
    });
    expect(clipped).toBe(true);
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
