import { test, expect } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// コンポーザのスラッシュコマンド（issue #387）の E2E。
///
/// 候補は `GET /skills/catalog`（モデルが `skill` ツールで見ているカタログと同一の源）由来で、
/// コマンド宣言（`SkillBody.command`）つきは `/<コマンド>`、無いもの（画面で作ったスキル）は
/// スキル名で並ぶ。ここでは宣言つきスキルを
/// 1 つ作り、補完 → 確定（ピル）→ 解除 → 「+」メニューからの打ち込みまでを通す。
/// `SHOTS_DIR` を渡すと各状態の PNG を保存する。
const SHOTS = process.env.SHOTS_DIR;

test.use({ deviceScaleFactor: 2, locale: "ja-JP", viewport: { width: 1280, height: 900 } });

test("スラッシュコマンドの補完と確定", async ({ page }) => {
  await loginViaKeycloak(page);

  // コマンド宣言つきの skill を作る（宣言の編集欄は UI に無い＝バンドル/API が正の経路）。
  const name = uniqueName("research");
  // **コマンド名も一意にする**。別スキルが同じコマンド名を宣言でき（名前空間は強制しない）、
  // 過去実行が残す skill と混ざると候補数が増えて検証が不安定になる。
  const cmd = name.toLowerCase().replace(/[^a-z0-9-]/g, "-");
  await page.goto("/");
  // BFF は同一オリジンからの fetch を要求するため、ページ内で叩く。
  const created = await page.evaluate(async ({ skillName, cmdName }) => {
    // double-submit CSRF（`lib/api.ts` の apiFetch と同じ作法）。
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/);
    const res = await fetch("/api/skills", {
      method: "POST",
      credentials: "include",
      headers: {
        "Content-Type": "application/json",
        ...(csrf ? { "X-CSRF-Token": decodeURIComponent(csrf[1]) } : {}),
      },
      body: JSON.stringify({
        name: skillName,
        body: {
          description: "web と社内文書を横断して深掘り調査し、出典つきレポートを書く。",
          instructions: "# 深掘り調査\n手順に従って調査する。",
          command: {
            name: cmdName,
            hint: "調べたいことを入力",
            variants: [
              { args: "", summary: "質問→計画→実行（推奨）" },
              { args: "auto", summary: "質問と計画確認を省略して即実行" },
            ],
          },
        },
      }),
    });
    return { status: res.status, text: await res.text() };
  }, { skillName: name, cmdName: cmd });
  expect(created.status, `skill 作成: ${created.status} ${created.text}`).toBeLessThan(300);
  const skillId = (JSON.parse(created.text) as { id: string }).id;

  await page.reload();
  const input = page.getByLabel("メッセージを入力");

  // `/` を打つと補完が出る。
  await input.fill(`/${cmd}`);
  const menu = page.getByTestId("slash-command-menu");
  await expect(menu).toBeVisible({ timeout: 10_000 });
  await expect(page.getByTestId("slash-command-option")).toHaveCount(2);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/slash-command-menu.png` });

  // Enter で確定するとピルになる。
  await input.press("Enter");
  const pill = page.getByTestId("slash-command-pill");
  await expect(pill).toBeVisible();
  await expect(pill).toContainText(`/${cmd}`);
  await expect(input).toHaveText("");
  // hint がプレースホルダになる。
  await expect(input).toHaveAttribute("placeholder", "調べたいことを入力");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/slash-command-pill.png` });

  // キャレット先頭の Backspace で外れる。
  await input.press("Backspace");
  await expect(pill).toHaveCount(0);

  // 「+」メニューからも打ち込める。**どのスキルが上位に来るかは順序依存**（メニューは
  // 件数を絞る・#390）なので、特定のコマンドではなく「先頭の項目を選ぶとピルになる」ことを見る。
  await page.getByRole("button", { name: "追加メニューを開く" }).click();
  const menuItem = page.getByTestId("composer-skill-command").first();
  await expect(menuItem).toBeVisible();
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/slash-command-plus-menu.png` });
  // 項目の 1 行目（`/<token>`、宣言の無いスキルはスキル名）がそのままピルになる。
  const token = ((await menuItem.locator("span.block").first().textContent()) ?? "").trim();
  expect(token.length).toBeGreaterThan(0);
  await menuItem.click();
  const pillFromMenu = page.getByTestId("slash-command-pill");
  await expect(pillFromMenu).toBeVisible();
  await expect(pillFromMenu).toContainText(token);
  // 「最近使ったスキル」はユーザー＋テナント単位のキーに記録される（素のキーには書かない・#817）。
  const recentKeys = await page.evaluate(() =>
    Object.keys(window.localStorage).filter((k) => k.startsWith("shiki.composer.recent-skills")),
  );
  expect(recentKeys).toHaveLength(1);
  expect(recentKeys[0]).toMatch(/^shiki\.composer\.recent-skills@[^|]+\|.+$/);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/slash-command-pill-auto.png` });

  // 補完で確定しなくても、直接打ち切ったコマンドは**送信時に解決される**（#387・Codex P2）。
  // 候補が閉じることだけでなく、POST に対象 skill が載ることまで見る（skills を落とす回帰を防ぐ）。
  // 語そのものを押すと外れる（#768。本文の先頭に入る語になった）。
  await pillFromMenu.click();
  const posted = page.waitForRequest(
    (r) => r.method() === "POST" && /\/api\/threads\/[^/]+\/messages$/.test(r.url()),
  );
  await input.fill(`/${cmd} auto 市場規模を調べて`);
  await expect(page.getByTestId("slash-command-menu")).toHaveCount(0);
  await page.getByRole("button", { name: "送信" }).click();
  const body = JSON.parse((await posted).postData() ?? "{}") as {
    text?: string;
    skills?: { artifact_id: string }[];
    autonomous?: boolean;
  };
  expect(body.skills?.length, `skills が載っていない: ${JSON.stringify(body)}`).toBe(1);
  expect(body.skills?.[0].artifact_id).toBe(skillId);
  // コマンドはリテラルとして本文の先頭に残る（意味づけは skill の instructions が持つ）。
  expect(body.text).toBe(`/${cmd} auto 市場規模を調べて`);
  // コマンド起動は長ホライズン（通常チャットの max_steps=6 では多段の作法が切れる）。
  expect(body.autonomous).toBe(true);
});

test("コマンド宣言の無い自作スキルも / と「スキルを検索」から呼べる", async ({ page }) => {
  await loginViaKeycloak(page);

  // 画面で作るスキルと同じく command を持たない（`/` 名は作られず、スキル名で探す）。
  const uniq = uniqueName("mine");
  const name = `週報 ${uniq}`;
  await page.goto("/");
  const created = await page.evaluate(async (skillName) => {
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/);
    const res = await fetch("/api/skills", {
      method: "POST",
      credentials: "include",
      headers: {
        "Content-Type": "application/json",
        ...(csrf ? { "X-CSRF-Token": decodeURIComponent(csrf[1]) } : {}),
      },
      body: JSON.stringify({
        name: skillName,
        body: { description: "週報を定型で書く自作スキル。", instructions: "# 週報\n定型で書く。" },
      }),
    });
    return { status: res.status, text: await res.text() };
  }, name);
  expect(created.status, `skill 作成: ${created.status} ${created.text}`).toBeLessThan(300);
  const skillId = (JSON.parse(created.text) as { id: string }).id;

  await page.reload();
  const input = page.getByLabel("メッセージを入力");

  // `/` の後に打った語をスキル名で探して出す（`/` 付きのコマンド名にはしない）。
  await input.fill(`/${uniq}`);
  const option = page.getByTestId("slash-command-option");
  await expect(option).toHaveCount(1, { timeout: 10_000 });
  await expect(option).toContainText(name);
  await expect(option).not.toContainText(`/${uniq}`);
  await input.fill("");

  // 「+」の「スキルを検索」は入力欄の先頭に `/` を入れて補完を開く。書きかけの本文は残る。
  await input.fill("今週分をまとめて");
  await page.getByRole("button", { name: "追加メニューを開く" }).click();
  await page.getByTestId("composer-search-skills").click();
  await expect(page.getByTestId("slash-command-menu")).toBeVisible();
  await expect(input).toBeFocused();
  await expect(input).toContainText("今週分をまとめて");
  await page.keyboard.type(uniq);
  await expect(option).toHaveCount(1);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/skill-search-slash.png` });
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("slash-command-pill")).toContainText(name);
  await expect(input).toHaveText("今週分をまとめて");

  // 送ると、この発話にだけその skill がピンされる。本文にコマンドのリテラルは付けない。
  const posted = page.waitForRequest(
    (r) => r.method() === "POST" && /\/api\/threads\/[^/]+\/messages$/.test(r.url()),
  );
  await page.getByRole("button", { name: "送信" }).click();
  const body = JSON.parse((await posted).postData() ?? "{}") as {
    text?: string;
    skills?: { artifact_id: string }[];
  };
  expect(body.skills?.[0]?.artifact_id).toBe(skillId);
  expect(body.text).toBe("今週分をまとめて");
});
