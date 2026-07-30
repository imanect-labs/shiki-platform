import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";

import { loginViaKeycloak, uniqueName } from "./helpers";

/// 実 LLM ＋ 実 web（SearXNG）での deep research 完走検証（#387 / #391 の DoD）。
///
/// **手動実行専用**（実 LLM の課金と非決定性のため CI では走らせない）。`REAL_LLM=1` が無ければ
/// skip する。実行手順:
///
/// ```bash
/// # SearXNG（compose の websearch プロファイル）と実 LLM を配線したサーバを起動しておく
/// REAL_LLM=1 E2E_BASE_URL=http://localhost:10386 \
///   pnpm exec playwright test e2e/real-deep-research.manual.spec.ts
/// ```
///
/// 見るのは「完走するか」だけ。品質（引用が実在するか・矛盾が両論併記か・未確認主張が残って
/// いないか）は出力を人が読んで判断する（自動判定できる性質ではない）。
///
/// `RECORD=1` で**動画**（webm）も残す。`FLOW=default` にすると質問カード → 計画カード →
/// 実行の全経路を通す（`auto` は確認を省略した 1 ターン）。
const SHOTS = process.env.SHOTS_DIR ?? "/tmp";
const TOPIC =
  process.env.TOPIC ?? "リモートワークは従業員の生産性を上げるのか下げるのか、根拠つきで";
/// `default` = 質問 → 計画 → 実行（計画カードの中身まで見える）／`auto` = 即実行。
const FLOW = process.env.FLOW ?? "auto";

test.use({
  deviceScaleFactor: 2,
  locale: "ja-JP",
  viewport: { width: 1280, height: 1200 },
  // 動画は 1 テストにつき 1 本（webm）。等倍サイズで撮る（deviceScaleFactor は効かない）。
  video: process.env.RECORD === "1" ? { mode: "on", size: { width: 1280, height: 1200 } } : "off",
});
test.setTimeout(30 * 60 * 1000);
test.skip(process.env.REAL_LLM !== "1", "実 LLM 検証は手動（REAL_LLM=1 で実行）");

test("実 LLM: /deep-research が出典つきレポートまで完走する", async ({ page }, testInfo) => {
  await loginViaKeycloak(page);
  await page.goto("/");

  // 配布バンドルの instructions をそのまま使う（本番と同一の手順書で検証する）。
  const bundle = JSON.parse(
    readFileSync("../sdk/first-party-skills/deep-research/skill.json", "utf8"),
  ) as Record<string, unknown>;
  const created = await page.evaluate(
    async ({ skillName, body }) => {
      const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/);
      const res = await fetch("/api/skills", {
        method: "POST",
        credentials: "include",
        headers: {
          "Content-Type": "application/json",
          ...(csrf ? { "X-CSRF-Token": decodeURIComponent(csrf[1]) } : {}),
        },
        body: JSON.stringify({ name: skillName, body }),
      });
      return { status: res.status, text: await res.text() };
    },
    { skillName: uniqueName("deep-research-real"), body: bundle },
  );
  expect(created.status, created.text).toBeLessThan(300);

  await page.reload();
  const input = page.getByLabel("メッセージを入力");
  await input.fill("/deep-research");
  await expect(page.getByTestId("slash-command-menu")).toBeVisible({ timeout: 15_000 });
  if (FLOW === "auto") {
    await input.press("ArrowDown"); // auto variant
  }
  await input.press("Enter");
  await input.fill(TOPIC);
  await page.getByRole("button", { name: "送信" }).click();

  if (FLOW !== "auto") {
    // ── フェーズ 0: 質問カード（モデルが立てた問いに答える） ──
    const options = page.getByTestId("genui-question-option");
    await expect(options.first()).toBeVisible({ timeout: 5 * 60 * 1000 });
    await page.screenshot({ path: `${SHOTS}/real-dr-question.png`, fullPage: true });
    // 各問の先頭選択肢を選び、最後の問いで送信する。問い数も submit のラベルも AI が決めるので
    // 「次へ」が出ている限り送り、消えたら testid で送信する（文言に依存しない）。
    const next = page.getByRole("button", { name: "次へ" });
    for (let step = 0; step < 6; step++) {
      await options.first().click();
      if (!(await next.isVisible().catch(() => false))) break;
      await next.click();
    }
    await page.getByTestId("genui-question-submit").click();

    // ── フェーズ 1: 計画カード（この依頼固有の問いが並ぶこと） ──
    const planStart = page.getByTestId("genui-plan-start");
    await expect(planStart).toBeVisible({ timeout: 5 * 60 * 1000 });
    const steps = page.getByTestId("genui-plan-steps").locator("li");
    const count = await steps.count();
    console.log(`=== PLAN (${count} steps) ===`);
    console.log(await page.getByTestId("genui-plan-steps").innerText());
    await page.screenshot({ path: `${SHOTS}/real-dr-plan.png`, fullPage: true });
    // 手順（方法論）が並んでいたら計画として失敗（ユーザーの判断材料にならない）。
    for (const method of ["証拠台帳", "節ごとに執筆", "視点を分けて"]) {
      expect(
        await page.getByTestId("genui-plan-steps").getByText(method, { exact: false }).count(),
        `計画に手順「${method}」が出ている（依頼固有の問いを並べること）`,
      ).toBe(0);
    }
    await planStart.click();
  }

  // 完走の合図: 「生成を停止」が出てから消える＝run 終了。
  await expect(page.getByRole("button", { name: "生成を停止" })).toBeVisible({ timeout: 120_000 });
  await expect(page.getByRole("button", { name: "生成を停止" })).toHaveCount(0, {
    timeout: 25 * 60 * 1000,
  });

  await page.screenshot({ path: `${SHOTS}/real-deep-research.png`, fullPage: true });
  const body = (await page.locator("main").innerText()).slice(0, 20_000);
  console.log("=== PAGE TEXT ===\n" + body);
  if (process.env.RECORD === "1") {
    console.log(`=== VIDEO === ${await page.video()?.path()}`);
  }
  expect(testInfo.status).not.toBe("timedOut");
});
