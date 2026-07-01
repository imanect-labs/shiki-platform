import { test, expect } from "@playwright/test";

import { loginViaKeycloak, submitKeycloakLogin } from "./helpers";

test("未ログイン保護ページ→ログイン→戻り先復帰→ダミー送信→ログアウト", async ({
  page,
}) => {
  // 1. 未ログインで保護ページへ → /login?next=/drive へ誘導される。
  await page.goto("/drive");
  await expect(page).toHaveURL(/\/login\?next=%2Fdrive/);
  await expect(page.getByRole("heading", { name: "ログイン" })).toBeVisible();

  // 2. Keycloak でログイン。
  await page.getByRole("button", { name: "Keycloak でログイン" }).click();
  await submitKeycloakLogin(page);

  // 3. ログイン後は元の遷移先（/drive）へ復帰する（next 復帰）。
  await page.waitForURL(/\/drive$/, { timeout: 20_000 });

  // 4. /me がシェル内に反映される（アカウントバナーにユーザー名）。
  await expect(page.getByRole("button", { name: "アカウントメニューを開く" })).toBeVisible();

  // 5. ホームでダミーチャット送信 → 会話画面 → モック応答が出る。
  await page.goto("/");
  const composer = page.getByPlaceholder("何でも尋ねて、何でも作成");
  await composer.fill("E2E テストのメッセージ");
  await composer.press("Enter");
  await page.waitForURL(/\/c\//, { timeout: 20_000 });
  // サイドバー履歴にも同名リンクが出るため、会話本文（main 内）に限定して検証する。
  const main = page.getByRole("main");
  await expect(main.getByText("E2E テストのメッセージ")).toBeVisible();
  await expect(main.getByText(/権限考慮 RAG と自律エージェント/)).toBeVisible({
    timeout: 15_000,
  });

  // 5.5 検索パレットが ⌘K で開く（シェルで単一管理。複数マウントなら strict 違反で落ちる）。
  await page.keyboard.press("Control+k");
  await expect(page.getByPlaceholder("チャットを検索...")).toBeVisible();
  await page.keyboard.press("Escape");

  // 6. ログアウト → セッション破棄。BFF は id_token_hint を出さないため Keycloak が
  //    ログアウト確認画面を挟むことがある。出たら確定する。
  await page.getByRole("button", { name: "アカウントメニューを開く" }).click();
  await page.getByRole("menuitem", { name: "ログアウト" }).click();
  await page.waitForURL(/(:8081\/.*logout|localhost:3000)/, { timeout: 20_000 });
  if (page.url().includes(":8081")) {
    await page.locator("#kc-logout, button[type=submit], input[type=submit]").first().click();
  }

  // セッションが破棄され、保護ページへ行くとログインへ戻る（負例の確認）。
  await page.goto("/drive");
  await expect(page).toHaveURL(/\/login/, { timeout: 20_000 });
});

test("ログイン済みで /login に来たらアプリへリダイレクトする", async ({ page }) => {
  await loginViaKeycloak(page);

  // 再度 /login へ来ると、セッション確認後にアプリへ戻される。
  await page.goto("/login");
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: 20_000 });
});
