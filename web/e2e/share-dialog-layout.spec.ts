import { expect, test, type Locator, type Page } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 共有ダイアログの作り直し（「ファイル共有設定の UI が見にくい」への対応）の受け入れ条件を検証する。
/// - ダイアログの高さは、検索中・タブの切り替え・作成フォームの展開・共有相手の多さで変わらない。
/// - 共有相手が多いとき、ダイアログは伸びず、中の一覧だけがスクロールする。
/// - 1 つの検索欄でメンバーと部署・ロールを探し、検索欄の横で 1 回選んだ権限で共有する。
/// - 共有中の相手は候補に「○で共有中」と出て、別の権限を選ぶと「○に変更」で変えられる。
/// - 発行済みリンクは作成フォームより上に出て、発行済みがあれば作成フォームは畳まれる。
/// - 日本語の変換確定の Enter では検索欄のフォーカスを外さない。

async function openShareDialog(page: Page, folder: string): Promise<Locator> {
  await page.getByRole("button", { name: `「${folder}」の操作` }).click();
  await page.getByRole("menuitem", { name: "共有" }).click();
  const share = page.getByRole("dialog");
  await expect(share.getByText(`「${folder}」を共有`)).toBeVisible();
  // 共有中の相手の読み込みが終わるまで待つ（読み込み中の表示が消える）。
  await expect(share.getByText("読み込み中…")).toHaveCount(0, { timeout: 10_000 });
  return share;
}

async function createFolder(page: Page, prefix: string): Promise<string> {
  await page.goto("/drive");
  const folder = uniqueName(prefix);
  await page.getByRole("button", { name: "新規フォルダ" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox").fill(folder);
  await dialog.getByRole("button", { name: "作成" }).click();
  await expect(page.getByText(folder, { exact: true })).toBeVisible();
  return folder;
}

async function height(locator: Locator): Promise<number> {
  // 開閉のアニメーション（拡大・縮小）の途中で測らない。
  // 回り続ける読み込みの印（無限アニメーション）は待たない。
  await locator.evaluate((el) =>
    Promise.all(
      el
        .getAnimations({ subtree: true })
        .filter((a) => a.effect?.getTiming().iterations !== Infinity)
        .map((a) => a.finished),
    ),
  );
  const box = await locator.boundingBox();
  if (!box) throw new Error("ダイアログが表示されていない");
  return Math.round(box.height);
}

/// 共有相手の一覧の取得だけを差し替える（n 件の架空の相手。付与・解除などの他の経路は実バックエンドのまま）。
async function stubShares(page: Page, n: number) {
  await page.route("**/api/nodes/*/shares", async (route) => {
    if (route.request().method() !== "GET") return route.continue();
    const entries = Array.from({ length: n }, (_, i) => ({
      target: { type: "user", id: `00000000-0000-0000-0000-${String(1000 + i).padStart(12, "0")}` },
      role: i % 4 === 0 ? "editor" : "viewer",
      display_name: `共有相手 ${i + 1}`,
      email: `member${i + 1}@a-corp.example.com`,
    }));
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(entries) });
  });
}

const DESKTOP = { width: 1280, height: 900 };
const VIEWPORTS = [
  { label: "PC 幅 1280px", size: DESKTOP },
  { label: "スマホ幅 390px", size: { width: 390, height: 844 } },
];

for (const vp of VIEWPORTS) {
  test(`共有ダイアログ（${vp.label}）: 高さは共有 0 件・3 件・25 件、検索中、リンクタブ、作成フォーム展開で変わらず、一覧だけがスクロールする`, async ({
    page,
  }) => {
    await page.setViewportSize(DESKTOP);
    await loginViaKeycloak(page); // alice
    const folder = await createFolder(page, "高さ固定");

    // ドライブの行メニューは PC 幅で開き、開いたダイアログを測る幅に合わせてから測る。
    const open = async () => {
      await page.setViewportSize(DESKTOP);
      const share = await openShareDialog(page, folder);
      await page.setViewportSize(vp.size);
      return share;
    };
    const close = async (share: Locator) => {
      await share.getByRole("button", { name: "閉じる", exact: true }).click();
      await expect(share).toBeHidden();
    };
    const heights: Record<string, number> = {};

    // 共有 0 件（実バックエンドの新しいフォルダ）。
    let share = await open();
    await expect(share.getByText("まだ誰にも共有していません。")).toBeVisible();
    heights["共有 0 件"] = await height(share);

    // 検索中（候補が一覧の位置に出る）。
    const search = share.getByPlaceholder("名前や部署で検索");
    await search.fill("営業");
    await expect(share.locator("li", { hasText: "営業部" })).toBeVisible({ timeout: 10_000 });
    heights["検索中"] = await height(share);
    await search.fill("");

    // リンクタブ（発行済み 0 件・作成フォームが開いている）。
    await share.getByTestId("share-tab-links").click();
    await expect(share.getByTestId("link-create")).toBeVisible();
    heights["リンクタブ（0 件）"] = await height(share);
    await share.getByTestId("link-audience-organization").click();
    await share.getByTestId("link-create").click();
    await expect(share.getByTestId("link-item")).toHaveCount(1, { timeout: 10_000 });
    heights["リンクタブ（発行直後）"] = await height(share);
    await close(share);

    // 開き直すと作成フォームは畳まれている → 開いた状態（作成フォーム展開）。
    share = await open();
    await share.getByTestId("share-tab-links").click();
    await expect(share.getByTestId("link-form-open")).toBeVisible({ timeout: 10_000 });
    heights["リンクタブ（作成フォームを畳んだ状態）"] = await height(share);
    await share.getByTestId("link-form-open").click();
    await expect(share.getByTestId("link-create")).toBeVisible();
    heights["作成フォーム展開"] = await height(share);
    await close(share);

    // 共有 3 件・25 件（一覧の取得だけを差し替える）。
    for (const n of [3, 25]) {
      await page.unrouteAll({ behavior: "wait" });
      await stubShares(page, n);
      share = await open();
      await expect(share.getByTestId("share-entry")).toHaveCount(n);
      heights[`共有 ${n} 件`] = await height(share);
      if (n === 25) {
        // ダイアログは伸びず、中の一覧の領域だけがスクロールする（最後の行までスクロールして見える）。
        const scroll = share.getByTestId("share-people-scroll");
        const overflow = await scroll.evaluate((el) => ({ sh: el.scrollHeight, ch: el.clientHeight }));
        expect(overflow.sh).toBeGreaterThan(overflow.ch);
        const last = share.getByTestId("share-entry").last();
        await last.scrollIntoViewIfNeeded();
        await expect(last).toBeInViewport();
        heights["共有 25 件（最後までスクロール後）"] = await height(share);
      }
      await close(share);
    }

    // すべての状態で高さが同じ（記録として出力する）。
    console.log(`[share-dialog-layout] ${vp.label} heights=${JSON.stringify(heights)}`);
    const values = Object.values(heights);
    expect(values.length).toBe(9);
    for (const [state, h] of Object.entries(heights)) {
      expect(h, `${vp.label}・${state}`).toBe(values[0]);
    }
  });
}

test("共有ダイアログ: 1 つの検索欄でメンバーと部署を探し、共有中の相手は候補から権限を変えられる", async ({
  page,
}) => {
  await loginViaKeycloak(page); // alice
  const folder = await createFolder(page, "検索と権限");
  const share = await openShareDialog(page, folder);
  const search = share.getByPlaceholder("名前や部署で検索");

  // 種別の切り替えは無い（以前の「メンバー / 部署・ロール」の切り替えが出ない）。
  await expect(share.getByTestId("share-kind-role")).toHaveCount(0);

  // 1 回の入力で、メンバーと部署・ロールの両方を検索する（2 つの検索が同じ語で飛ぶ）。
  const userReq = page.waitForRequest((r) => /\/api\/directory\/users\?.*q=%E5%96%B6%E6%A5%AD/.test(r.url()));
  const roleReq = page.waitForRequest((r) => /\/api\/directory\/roles\?.*q=%E5%96%B6%E6%A5%AD/.test(r.url()));
  await search.fill("営業");
  await Promise.all([userReq, roleReq]);

  // 検索中は、共有中の一覧の位置（同じスクロール領域）に候補が入れ替わって出る。
  const scroll = share.getByTestId("share-people-scroll");
  await expect(scroll.getByRole("list", { name: "部署・ロールの候補" })).toContainText("営業部", {
    timeout: 10_000,
  });
  await expect(scroll.getByText("共有中の相手")).toHaveCount(0);

  // 同じ検索欄で、メンバーも探せる。
  await search.fill("bob");
  await expect(share.getByRole("list", { name: "メンバーの候補" })).toContainText("bob@a-corp.example.com", {
    timeout: 10_000,
  });

  // 権限は検索欄の横で 1 回選ぶ（既定は閲覧）。bob を閲覧で共有 → 検索が閉じて一覧に戻る。
  await expect(share.getByLabel("追加する相手の権限")).toHaveText(/閲覧/);
  await search.fill("bob");
  const bobCandidate = share.locator("li", { hasText: "bob@a-corp.example.com" });
  await bobCandidate.getByRole("button", { name: "共有", exact: true }).click();
  const bobEntry = share.getByTestId("share-entry").filter({ hasText: "bob@a-corp.example.com" });
  await expect(bobEntry).toBeVisible({ timeout: 10_000 });
  await expect(search).toHaveValue("");
  await expect(bobEntry.getByRole("combobox")).toHaveText(/閲覧/);
  // 一覧は設定画面と同じ部品（rule-soft の面・行の間の荒い破線）で描かれる。
  await expect(bobEntry).toHaveClass(/shiki-dash-top/);
  await expect(bobEntry.locator("xpath=..")).toHaveClass(/rule-soft/);

  // 共有中の相手は候補に「閲覧で共有中」と「共有済み」が出る。
  await search.fill("bob");
  await expect(bobCandidate).toContainText("閲覧で共有中", { timeout: 10_000 });
  await expect(bobCandidate).toContainText("共有済み");

  // 付ける権限を編集に変えると「編集に変更」になり、押すと bob の権限が編集になる（共有は 1 件のまま）。
  await share.getByLabel("追加する相手の権限").click();
  await page.getByRole("option", { name: "編集" }).click();
  await bobCandidate.getByRole("button", { name: "編集に変更" }).click();
  await expect(bobEntry.getByRole("combobox")).toHaveText(/編集/, { timeout: 10_000 });
  await expect(share.getByTestId("share-entry").filter({ hasText: "bob@a-corp.example.com" })).toHaveCount(1);
});

test("共有ダイアログ: 発行済みリンクが作成フォームより上に出て、発行済みがあれば作成フォームは畳まれる", async ({
  page,
}) => {
  await loginViaKeycloak(page); // alice
  const folder = await createFolder(page, "リンク並び");
  let share = await openShareDialog(page, folder);
  await share.getByTestId("share-tab-links").click();

  // 未対応の選択肢は「準備中」の印付きで、内部の Issue 番号は出さない。
  await expect(share.getByTestId("link-audience-anonymous")).toContainText("準備中");
  await expect(share.getByText("#341")).toHaveCount(0);

  await share.getByTestId("link-audience-organization").click();
  await share.getByTestId("link-create").click();
  await expect(share.getByTestId("link-item")).toHaveCount(1, { timeout: 10_000 });
  // 発行と同時にトーストが出て、Esc は手前のトーストを閉じるので、ダイアログは閉じるボタンで閉じる。
  await share.getByRole("button", { name: "閉じる", exact: true }).click();
  await expect(share).toBeHidden();

  // 開き直すと、発行済みリンクが上にあり、作成フォームは畳まれている。
  share = await openShareDialog(page, folder);
  await share.getByTestId("share-tab-links").click();
  const list = share.getByTestId("link-list");
  const opener = share.getByTestId("link-form-open");
  await expect(list).toBeVisible({ timeout: 10_000 });
  await expect(opener).toBeVisible();
  await expect(share.getByTestId("link-create")).toHaveCount(0);
  const listBox = await list.boundingBox();
  const openerBox = await opener.boundingBox();
  expect(listBox!.y).toBeLessThan(openerBox!.y);

  // 「新しいリンクを作成」で作成フォームが開く。
  await opener.click();
  await expect(share.getByTestId("link-create")).toBeVisible();
});

test("共有ダイアログ: 日本語の変換確定の Enter では検索欄のフォーカスが外れない", async ({ page }) => {
  await loginViaKeycloak(page); // alice
  const folder = await createFolder(page, "IME");
  const share = await openShareDialog(page, folder);
  const search = share.getByPlaceholder("名前や部署で検索");
  await search.fill("やまだ");
  await search.focus();

  const pressEnter = (isComposing: boolean) =>
    search.evaluate(
      (el, composing) =>
        el.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true, isComposing: composing }),
        ),
      isComposing,
    );

  // 変換確定の Enter（isComposing）ではフォーカスが残る。
  await pressEnter(true);
  await expect(search).toBeFocused();
  // 通常の Enter はキーボードを閉じる（フォーカスを外す）。
  await pressEnter(false);
  await expect(search).not.toBeFocused();
});
