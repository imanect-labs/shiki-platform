import { test, expect } from "@playwright/test";

import { loginViaKeycloak, submitKeycloakLogin } from "./helpers";

test("未ログイン保護ページ→ログイン→戻り先復帰→ダミー送信→ログアウト", async ({
  page,
}) => {
  // 1. 未ログインで保護ページへ → /login?next=/drive を経て、そのまま Keycloak の
  //    ログイン画面（Shiki テーマ）へ送られる。
  await page.goto("/drive");
  await page.waitForURL(/:8081\/realms\/shiki\/protocol\/openid-connect\/auth/);
  await expect(page.getByRole("heading", { name: "ログイン" })).toBeVisible();
  await expect(page.getByRole("button", { name: "パスキーでログイン" })).toBeVisible();

  // 2. ユーザー名とパスワードでログイン。
  await submitKeycloakLogin(page);

  // 3. ログイン後は元の遷移先（/drive）へ復帰する（next 復帰）。
  await page.waitForURL(/\/drive$/, { timeout: 20_000 });

  // 4. /me がシェル内に反映される（アカウントバナーにユーザー名）。
  await expect(page.getByRole("button", { name: "アカウントメニューを開く" })).toBeVisible();

  // 5. ホームでダミーチャット送信 → 会話画面 → backend（stub LLM）応答が出る。
  await page.goto("/");
  const composer = page.getByPlaceholder("何でも尋ねて、社内文書も検索");
  await composer.fill("E2E テストのメッセージ");
  await composer.press("Enter");
  await page.waitForURL(/\/c\//, { timeout: 20_000 });
  // サイドバー履歴にも同名リンクが出るため、会話本文（main 内）に限定して検証する。
  // stub 応答「回答: E2E テストのメッセージ」にも部分一致するため first() で strict 違反を避ける
  // （応答が 5 秒以内に描画されると 2 要素になるタイミング依存 flake だった）。
  const main = page.getByRole("main");
  await expect(main.getByText("E2E テストのメッセージ").first()).toBeVisible();
  // 接続非依存ジョブ＋SSE で stub 応答（「回答: …」）がストリーミングで届く。
  await expect(main.getByText(/回答/).first()).toBeVisible({
    timeout: 20_000,
  });

  // 5.5 検索パレットが ⌘K で開く（シェルで単一管理。複数マウントなら strict 違反で落ちる）。
  await page.keyboard.press("Control+k");
  await expect(page.getByPlaceholder("チャットを検索...")).toBeVisible();
  await page.keyboard.press("Escape");

  // 5.6 localStorage のユーザーのデータはユーザー＋テナント単位のキーに置かれ、
  //     ログアウトで消える（共用 PC で次の人に残さない・#817）。画面の好みは残す。
  await page.evaluate(() => {
    window.localStorage.setItem("shiki.note-drafts.v1", "{}"); // #817 以前の素のキー
    window.localStorage.setItem("docchat:active:legacy-file", "t-legacy");
    window.localStorage.setItem("shiki.slide-drafts.v1@other-corp|someone", "{}");
    window.localStorage.setItem("shiki:sidebar:width", "280");
    // 引用の手がかり（文書本文の断片）とホームからの最初のメッセージも本文を含む。
    window.localStorage.setItem("shiki:cite:e2e-chunk", JSON.stringify({ find: "本文の断片", at: Date.now() }));
    window.sessionStorage.setItem("shiki:pending-message:e2e-thread", JSON.stringify({ text: "下書き", attachments: [] }));
  });
  // 別タブ（同じブラウザ）にもユーザーのデータが残っている。ログアウトはこのタブにも知らされ、
  // タブごとの sessionStorage まで消える。
  const otherTab = await page.context().newPage();
  await otherTab.goto("/");
  await expect(otherTab.getByLabel("メッセージを入力")).toBeVisible({ timeout: 20_000 });
  await otherTab.evaluate(() =>
    window.sessionStorage.setItem(
      "shiki:pending-message:e2e-other-tab",
      JSON.stringify({ text: "別タブの下書き", attachments: [] }),
    ),
  );
  // 6. ログアウト → セッション破棄。BFF は id_token_hint を出さないため Keycloak が
  //    ログアウト確認画面を挟むことがある。出たら確定する。
  //    ログアウトの遷移（Keycloak の end-session）が始まる前に次の goto を打つと遷移が中断され、
  //    Keycloak のセッションが残って /login から黙って再ログインされる。今の画面を離れるまで待つ。
  const beforeLogout = page.url();
  await page.getByRole("button", { name: "アカウントメニューを開く" }).click();
  await page.getByRole("menuitem", { name: "ログアウト" }).click();
  await page.waitForURL((url) => url.href !== beforeLogout, { timeout: 20_000, waitUntil: "commit" });
  const onLogoutConfirm = (url: URL) => url.host.endsWith(":8081") && url.pathname.includes("/logout");
  if (onLogoutConfirm(new URL(page.url()))) {
    await page.locator("#kc-logout, button[type=submit], input[type=submit]").first().click();
    await page.waitForURL((url) => !onLogoutConfirm(url), { timeout: 20_000, waitUntil: "commit" });
  }

  // ユーザーのデータのキーは（他ユーザー分・旧来の素のキーも含め）消え、画面の好みは残る。
  // /reference は middleware の保護外なので、未ログインでも同一オリジンで localStorage を覗ける。
  // ログアウト後のリダイレクト（Keycloak → アプリ → /login）が続いている間の goto は中断され得る
  // （上の waitForURL は遷移前の URL にも一致する）ので、遷移が落ち着くまでやり直す。
  await expect(async () => {
    await page.goto("/reference");
  }).toPass({ timeout: 20_000 });
  const remaining = await page.evaluate(() => Object.keys(window.localStorage));
  expect(
    remaining.filter(
      (k) =>
        k.startsWith("shiki.composer.recent-skills") ||
        k.startsWith("docchat:active:") ||
        k.startsWith("shiki:cite:") ||
        /^shiki\.[a-z]+-drafts\.v1/.test(k),
    ),
  ).toEqual([]);
  expect(remaining).toContain("shiki:sidebar:width");
  // sessionStorage は同じタブに残るため、こちらも消えていること。
  const remainingSession = await page.evaluate(() => Object.keys(window.sessionStorage));
  expect(remainingSession.filter((k) => k.startsWith("shiki:pending-message:"))).toEqual([]);
  await expect
    .poll(() => otherTab.evaluate(() => Object.keys(window.sessionStorage)))
    .not.toContain("shiki:pending-message:e2e-other-tab");
  await otherTab.close();

  // セッションが破棄され、保護ページへ行くと Keycloak のログイン画面へ戻る（負例の確認）。
  // /login は未ログインなら即 Keycloak へ送るため、goto の load 待ちはリダイレクトで中断され得る。
  await page.goto("/drive", { waitUntil: "commit" });
  await page.waitForURL(/:8081\/realms\/shiki\/protocol\/openid-connect\/auth/, { timeout: 20_000 });
  await expect(page.getByRole("heading", { name: "ログイン" })).toBeVisible();
});

test("ログイン済みで /login に来たらアプリへリダイレクトする", async ({ page }) => {
  await loginViaKeycloak(page);

  // 再度 /login へ来ると、セッション確認後にアプリへ戻される。
  await page.goto("/login");
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: 20_000 });
});
