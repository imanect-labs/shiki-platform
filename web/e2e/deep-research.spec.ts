import { test, expect } from "@playwright/test";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// deep research（issue #387）の E2E: `/deep-research` の**質問 → 計画 → 実行**を通す。
///
/// 実 LLM は使わず、スタブプロバイダの `/deep-research` 駆動
/// （`crates/llm-gateway/src/providers/stub_deep_research.rs`）でフェーズを決定的に再現する。
/// カードの押下は `chat.submit` で**別 run** になるため、run を跨いで状態が復元されること
/// （＝スキルの適用と autonomous の継承が続くこと）もここで初めて通し見できる。
const SHOTS = process.env.SHOTS_DIR;

/// スタブが反応するコマンド名（本番のバンドルと同一。ここを変えると駆動しない）。
const COMMAND = "deep-research";

test.use({ deviceScaleFactor: 2, locale: "ja-JP", viewport: { width: 1280, height: 1000 } });

/// コマンド宣言つきの skill を本人 owner として作る（レジストリ/署名鍵に依存させない）。
async function createDeepResearchSkill(page: import("@playwright/test").Page) {
  const name = uniqueName("deep-research");
  const created = await page.evaluate(async (skillName) => {
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
          instructions: "# 深掘り調査\n質問 → 計画 → 実行の順に進める。",
          command: {
            name: "deep-research",
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
  }, name);
  expect(created.status, `skill 作成: ${created.status} ${created.text}`).toBeLessThan(300);
}

test("deep research: 質問カード → 計画カード → 調査 → レポート → 出典 → 下書き", async ({
  page,
}) => {
  await loginViaKeycloak(page);
  await page.goto("/");
  await createDeepResearchSkill(page);
  await page.reload();

  // ── 起動: `/deep-research` を確定してから依頼を書く ──
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`/${COMMAND}`);
  await expect(page.getByTestId("slash-command-menu")).toBeVisible({ timeout: 10_000 });
  await input.press("Enter");
  await expect(page.getByTestId("slash-command-pill")).toContainText(`/${COMMAND}`);
  await input.fill("2026 年の国内 SaaS 市場規模を調べて");
  await page.getByRole("button", { name: "送信" }).click();

  // ── フェーズ 0: 質問カード（最大 3 問・1 ターン・複数問はステップ送り） ──
  const options = page.getByTestId("genui-question-option");
  await expect(options.first()).toBeVisible({ timeout: 60_000 });
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/deep-research-question.png`, fullPage: true });
  await options.first().click();
  // 2 問目へ送って回答する（最後の問いで submit_label のボタンになる）。
  await page.getByRole("button", { name: "次へ" }).click();
  await expect(options.first()).toBeVisible();
  await options.first().click();
  await page.getByRole("button", { name: "この条件で進める" }).click();
  await expect(page.getByText("回答を送信しました")).toBeVisible();

  // ── フェーズ 1: 計画カード（開始ボタンで承認を取る） ──
  const planStart = page.getByTestId("genui-plan-start");
  await expect(planStart).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId("genui-plan-steps").locator("li")).toHaveCount(4);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/deep-research-plan.png`, fullPage: true });
  await planStart.click();

  // ── フェーズ 2〜4: 調査 → レポート → 出典 → 下書き ──
  // レポート本文（節見出し・両論併記・見つからなかったことの明示）。
  await expect(page.getByText("結論と確度").first()).toBeVisible({ timeout: 90_000 });
  await expect(page.getByText("公表資料では確認できなかった").first()).toBeVisible();

  // ツール実行表示（#386）に取得した URL が具体的に出る。展開して全件を見る。
  // **最後の run** のものを見る（先行 run＝質問/計画カードにも tool-activity が出る）。
  const activity = page.getByTestId("tool-activity").last();
  await expect(activity).toBeVisible();
  await activity.click();
  const expanded = page.getByTestId("tool-activity-expanded").last();
  await expect(expanded).toContainText("example.com/stub-1");
  await expect(expanded).toContainText("notes.md");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/deep-research-activity.png`, fullPage: true });

  // 出典カード（web 出典の唯一の構造化表示）と保存ボタン（下書きノート）。
  await expect(page.getByText("出典", { exact: true }).first()).toBeVisible();
  await expect(page.getByRole("link", { name: /example\.com/ }).first()).toBeVisible();
  const draft = page.getByTestId("note-draft-card").first();
  await expect(draft).toBeVisible();
  await expect(draft).toContainText("2026年 国内SaaS市場の調査");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/deep-research-report.png`, fullPage: true });

  // 承認カードは出ない（作業メモはシステム領域＝事前許可・#392）。
  await expect(page.getByText("承認が必要です")).toHaveCount(0);
});

test("deep research auto: 確認を省略して 1 ターンで完走する", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/");
  await createDeepResearchSkill(page);
  await page.reload();

  // `auto` variant を補完から選ぶ（variants の 2 番目）。
  const input = page.getByLabel("メッセージを入力");
  await input.fill(`/${COMMAND}`);
  await expect(page.getByTestId("slash-command-menu")).toBeVisible({ timeout: 10_000 });
  await input.press("ArrowDown");
  await input.press("Enter");
  await expect(page.getByTestId("slash-command-pill")).toContainText(`/${COMMAND} auto`);
  await input.fill("2026 年の国内 SaaS 市場規模を調べて");
  await page.getByRole("button", { name: "送信" }).click();

  // 質問カードも計画カードも出さず、そのままレポートまで進む。
  await expect(page.getByText("結論と確度").first()).toBeVisible({ timeout: 90_000 });
  await expect(page.getByTestId("genui-question-option")).toHaveCount(0);
  await expect(page.getByTestId("genui-plan-start")).toHaveCount(0);
  await expect(page.getByTestId("note-draft-card").first()).toBeVisible();
  if (SHOTS) {
    await page.screenshot({ path: `${SHOTS}/deep-research-auto.png`, fullPage: true });
  }
});
