import { test, expect, type Page } from "@playwright/test";

import { loginAs, loginViaKeycloak, uniqueName } from "./helpers";

/// 職員間メッセージ（Phase 14・design §4.14）。実サーバ（compose の shiki-server）に対して、
/// チャンネルの作成・投稿・スレッド・リアクション・編集・削除・検索と、非公開チャンネルが他人に見えないこと、
/// DM を開けることを確かめる。権限の網羅は Rust の結合テスト（crates/messaging・crates/api）が担う。

test.use({ viewport: { width: 1440, height: 900 } });

async function openMessages(page: Page) {
  await page.goto("/messages");
  await expect(page.getByRole("complementary", { name: "チャンネル一覧" })).toBeVisible();
}

async function createChannel(page: Page, name: string, kind: "公開" | "非公開", invite?: string) {
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByRole("menuitem", { name: "チャンネルを作成" }).click();
  const dialog = page.getByRole("dialog", { name: "チャンネルを作成" });
  await dialog.getByRole("radio", { name: new RegExp(`^${kind} `) }).click();
  await dialog.getByLabel("名前").fill(name);
  if (invite) {
    await dialog.getByRole("textbox", { name: "招待する人" }).fill(invite);
    await dialog.getByRole("option", { name: new RegExp(invite, "i") }).first().click();
  }
  await dialog.getByRole("button", { name: "作成する" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("heading", { level: 1, name })).toBeVisible();
}

async function send(page: Page, text: string, where = "#") {
  const box = page.getByRole("combobox", { name: new RegExp(`^${where === "#" ? "#" : ""}.*へメッセージを送る$`) });
  await box.fill(text);
  await box.press("Enter");
}

function row(page: Page, text: string) {
  return page.getByTestId("channel-view").getByTestId("message-row").filter({ hasText: text });
}

test("チャンネルを作って投稿し、スレッド・リアクション・編集・削除・検索ができる", async ({ page }) => {
  await loginViaKeycloak(page);
  await openMessages(page);
  const name = uniqueName("e2e-ch");
  await createChannel(page, name, "公開");

  // 投稿。
  const first = `最初の発言 ${name}`;
  await send(page, first);
  await expect(row(page, first)).toBeVisible();

  // リアクション（ホバーで出る操作）。
  await row(page, first).hover();
  await row(page, first).getByRole("button", { name: "リアクションを付ける" }).click();
  await page.getByRole("button", { name: "👍" }).click();
  await expect(row(page, first).getByRole("button", { name: /👍 1 件/ })).toHaveAttribute("aria-pressed", "true");

  // スレッドで返信。
  await row(page, first).hover();
  await row(page, first).getByRole("button", { name: "スレッドで返信" }).click();
  const thread = page.getByTestId("thread-pane");
  await expect(thread).toBeVisible();
  const reply = `スレッドの返信 ${name}`;
  const replyBox = thread.getByRole("combobox", { name: "スレッドに返信…" });
  await replyBox.fill(reply);
  await replyBox.press("Enter");
  await expect(thread.getByTestId("message-row").filter({ hasText: reply })).toBeVisible();
  await expect(row(page, first).getByRole("button", { name: /返信 1 件/ })).toBeVisible();
  await thread.getByRole("button", { name: "スレッドを閉じる" }).click();
  await expect(thread).toBeHidden();

  // 編集（本人だけ）。
  const second = `二つ目 ${name}`;
  await send(page, second);
  await row(page, second).hover();
  await row(page, second).getByRole("button", { name: "編集" }).click();
  const editor = row(page, second).getByRole("combobox", { name: "発言を編集" });
  await editor.fill(`${second} 直しました`);
  await editor.press("Enter");
  await expect(row(page, `${second} 直しました`)).toContainText("（編集済み）");

  // 削除（確認してから）。
  const third = `消す発言 ${name}`;
  await send(page, third);
  await row(page, third).hover();
  await row(page, third).getByRole("button", { name: "削除" }).click();
  await page.getByRole("dialog", { name: "発言を削除しますか？" }).getByRole("button", { name: "削除する" }).click();
  await expect(page.getByText("この発言は削除されました")).toBeVisible();
  await expect(row(page, third)).toHaveCount(0);

  // 検索（参加しているチャンネルの発言が当たる）。
  await page.getByRole("button", { name: "発言を検索" }).click();
  await page.getByRole("textbox", { name: "発言を検索" }).fill(first);
  const hit = page.getByTestId("search-hit").filter({ hasText: name });
  await expect(hit.first()).toBeVisible();
  await hit.first().click();
  await expect(page.getByRole("heading", { level: 1, name })).toBeVisible();
});

test("送信の応答を待つ間に書き足した下書きは消えず、そのまま送ったときだけ空になる", async ({ page }) => {
  await loginViaKeycloak(page);
  await openMessages(page);
  const name = uniqueName("e2e-draft");
  await createChannel(page, name, "公開");
  const box = page.getByRole("combobox", { name: /へメッセージを送る$/ });

  // 投稿のリクエストを止めておき、その間に書き足してから流す。
  let release: () => void = () => undefined;
  const held = new Promise<void>((r) => (release = r));
  let seen: () => void = () => undefined;
  const posted = new Promise<void>((r) => (seen = r));
  await page.route("**/api/messaging/channels/*/messages", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    seen();
    await held;
    await route.continue();
  });
  const sent = `送った本文 ${name}`;
  await box.fill(sent);
  await box.press("Enter");
  await posted;
  await box.press("End");
  await box.pressSequentially(" 追記");
  release();
  await expect(row(page, sent)).toBeVisible();
  await expect(box).toHaveValue(`${sent} 追記`);
  await page.unroute("**/api/messaging/channels/*/messages");

  // 書き足さずに送れば空になる。
  await box.fill(`次の発言 ${name}`);
  await box.press("Enter");
  await expect(row(page, `次の発言 ${name}`)).toBeVisible();
  await expect(box).toHaveValue("");
});

test("非公開チャンネルは参加していない人には見えず、DM を開ける", async ({ page, browser }) => {
  await loginViaKeycloak(page);
  await openMessages(page);
  const name = uniqueName("e2e-private");
  await createChannel(page, name, "非公開");
  const secret = `内緒の発言 ${name}`;
  await send(page, secret);
  await expect(row(page, secret)).toBeVisible();
  const channelId = new URL(page.url()).searchParams.get("c");
  expect(channelId).toBeTruthy();

  // Bob（同じテナント・招待していない）には一覧にも検索にも出ず、直接開いても読めない。
  const bobCtx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const bob = await bobCtx.newPage();
  await loginAs(bob, "bob");
  await openMessages(bob);
  await expect(bob.getByTestId("channel-row").filter({ hasText: name })).toHaveCount(0);
  const direct = await bob.request.get(`/api/messaging/channels/${channelId}`);
  expect(direct.status()).toBe(404);
  await bob.goto(`/messages?c=${channelId}`);
  await expect(bob.getByText("このチャンネルを開けません")).toBeVisible();
  await bob.getByRole("button", { name: "発言を検索" }).click();
  await bob.getByRole("textbox", { name: "発言を検索" }).fill(secret);
  await expect(bob.getByText("一致する発言がありません")).toBeVisible();

  // DM を開く（同じ 2 人の DM は 1 つ）。Alice が送ると Bob の一覧に出る。
  await page.getByRole("button", { name: "新規作成" }).click();
  await page.getByRole("menuitem", { name: "ダイレクトメッセージ" }).click();
  const dm = page.getByRole("dialog", { name: "ダイレクトメッセージ" });
  await dm.getByRole("textbox", { name: "相手を探す" }).fill("bob");
  await dm.getByRole("button", { name: /bob/i }).first().click();
  await expect(dm).toBeHidden();
  const hello = `こんにちは ${name}`;
  const dmBox = page.getByRole("combobox", { name: /へメッセージを送る$/ });
  await dmBox.fill(hello);
  await dmBox.press("Enter");
  await expect(row(page, hello)).toBeVisible();

  await bob.goto("/messages");
  await bob.getByTestId("channel-row").filter({ hasText: /alice/i }).first().click();
  await expect(bob.getByTestId("channel-view").getByTestId("message-row").filter({ hasText: hello })).toBeVisible();

  // Bob の返信は、開いたままの Alice の画面に配信（SSE）で届く（読み直さない）。
  const answer = `お返事です ${name}`;
  const bobBox = bob.getByRole("combobox", { name: /へメッセージを送る$/ });
  await bobBox.fill(answer);
  await bobBox.press("Enter");
  await expect(row(page, answer)).toBeVisible({ timeout: 15_000 });
  await bobCtx.close();
});
