import { test, expect } from "@playwright/test";

/// 回答のリンクの出所チェック（/reference/answer-links・#1143）。
/// 認証・バックエンド・LLM 不要（middleware は /reference を除外）＝ web 単体で回せる。
///
/// 守ること:
/// - 出典（Web の引用）と同じホストへのリンクは、そのまま新しいタブで開く。
/// - 出典に無い外部リンク（社内文書に仕込まれた誘導・表示を偽ったリンク・メール）は、行き先と注意の印を添え、
///   押しても中クリックしても、確認ダイアログで「開く」を押すまで開かない。
/// - 回答以外の本文（Markdown を直接使う場所）は確認しない。
/// `SHOTS_DIR` を渡すとライト/ダークの PNG を保存する。
const SHOTS = process.env.SHOTS_DIR;

test.use({ deviceScaleFactor: 2 });

for (const theme of ["light", "dark"] as const) {
  test(`出典に無いリンクは確認してから開く（${theme}）`, async ({ page, context }) => {
    await page.emulateMedia({ colorScheme: theme });
    // 外部へは実際には出さない（開いたかどうかは新しいタブの発生で見る）。
    await context.route(/\.(example|example\.net|go\.jp)\//, (route) => route.fulfill({ status: 200, body: "ok" }));
    await page.goto("/reference/answer-links");
    const timeline = page.getByTestId("answer-links-timeline");

    // 出典と同じホスト（www.nta.go.jp）のリンクは注意の印が付かない。
    const trusted = timeline.getByRole("link", { name: "国税庁のタックスアンサー" });
    await expect(trusted).toHaveAttribute("target", "_blank");
    await expect(trusted).not.toHaveAttribute("data-testid", "answer-link-unverified");

    // 出典に無いリンクは 5 つ。行き先が見える。
    // - 仕込まれた URL・表示を偽ったリンク・メール
    // - web_fetch させただけのページ（引用 3）と同じホスト（Web 検索の結果に出ていないので信頼しない）
    // - 不正なパーセントエンコードのメール（描画ごと落とさない）
    const unverified = timeline.getByTestId("answer-link-unverified");
    await expect(unverified).toHaveCount(5);
    await expect(unverified.nth(0)).toContainText("evil.example");
    await expect(unverified.nth(1)).toContainText("nta-go-jp.example.net");
    await expect(unverified.nth(2)).toContainText("mailto:keiri@corp.example");
    await expect(unverified.nth(3)).toContainText("evil.example");
    await expect(unverified.nth(4)).toContainText("mailto:help%ZZ@corp.example");
    // 本物の href を持たない（右クリック・長押し・ドラッグ・PDF で素通りさせない）。
    for (let i = 0; i < 5; i++) await expect(unverified.nth(i)).not.toHaveAttribute("href", /.*/);
    // プロトコル相対の URL（`//evil.example/apply`）はアプリ内リンクとして扱わず、リンクにもしない。
    await expect(timeline.getByText("手続きは 申請ページ から。")).toBeVisible();
    await expect(timeline.getByRole("link", { name: "申請ページ", exact: true })).toHaveCount(0);
    await expect(timeline.locator('a[href^="//"]')).toHaveCount(0);

    if (SHOTS) await page.screenshot({ path: `${SHOTS}/answer-links-${theme}.png`, fullPage: true });

    // 押すと確認ダイアログ。キャンセルでは新しいタブを開かない。
    let opened = 0;
    context.on("page", () => opened++);
    await unverified.nth(0).click();
    const dialog = page.getByTestId("unverified-link-dialog");
    await expect(dialog).toBeVisible();
    await expect(dialog.getByTestId("unverified-link-url")).toHaveText("https://evil.example/login");
    // 既定のフォーカスはキャンセル（Enter で開いてしまわない）。
    await expect(dialog.getByRole("button", { name: "キャンセル" })).toBeFocused();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/answer-links-dialog-${theme}.png` });
    await page.keyboard.press("Enter");
    await expect(dialog).toBeHidden();

    // 中クリックでは何も開かない（href が無い）。
    await unverified.nth(1).click({ button: "middle" });
    // キーボードでもリンクとして操作でき、確認ダイアログが出る。
    await unverified.nth(3).focus();
    await page.keyboard.press(" ");
    await expect(dialog).toBeVisible();
    await expect(dialog.getByTestId("unverified-link-url")).toHaveText("https://evil.example/notice");
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await page.waitForTimeout(300);
    expect(opened).toBe(0);

    // 「開く」を押したときだけ新しいタブで開く。
    await unverified.nth(0).click();
    const popup = context.waitForEvent("page");
    await dialog.getByTestId("unverified-link-open").click();
    // noopener で開いたタブは、開いた直後は URL が空。遷移を待ってから見る。
    const tab = await popup;
    await tab.waitForURL("https://evil.example/login");
  });
}

test("回答以外の本文のリンクは確認しない", async ({ page }) => {
  await page.goto("/reference/answer-links");
  const plain = page.getByTestId("answer-links-plain");
  await expect(plain.getByTestId("answer-link-unverified")).toHaveCount(0);
  await expect(plain.getByRole("link", { name: "example" })).toHaveAttribute("target", "_blank");
});
