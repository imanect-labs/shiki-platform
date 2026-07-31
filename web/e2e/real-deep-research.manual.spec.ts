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
/// 既定のテーマは**このリポジトリで実際に判断が割れている論点**にする（#391 の受入条件・
/// #404）。賛否が公開情報で真っ向から割れており（Anthropic は multi-agent research system で
/// 有効性を主張・Cognition と LangChain は「マルチエージェントを作るな」と撤回）、一次情報が
/// web にあり、結論がそのまま `SubagentLimits` の判断材料になる。
///
/// 他に使える論点（`TOPIC=` で差し替え）:
///   - エージェント隔離の gVisor / Firecracker / WASM をどう使い分けるべきか（#346）
///   - 日本語 RAG でハイブリッド検索とリランカーはどれだけ効くか
///   - Zanzibar 系 ReBAC の実運用レイテンシと、RBAC/ABAC との使い分け
const TOPIC =
  process.env.TOPIC ??
  "LLM エージェントの調査タスクで、サブエージェントへの並列委譲は" +
    "単一エージェントに対してトークン増分に見合う品質向上をもたらすのか。" +
    "どの条件で有効でどの条件で有害か、実測と失敗事例の根拠つきで";
/// `default` = 質問 → 計画 → 実行（計画カードの中身まで見える）／`auto` = 即実行。
const FLOW = process.env.FLOW ?? "auto";

test.use({
  // 録画時は等倍・小さめ（webm が数十 MB になるため）。スクショだけなら 2x で撮る。
  deviceScaleFactor: process.env.RECORD === "1" ? 1 : 2,
  locale: "ja-JP",
  viewport: { width: 1200, height: 900 },
  // 動画は 1 テストにつき 1 本（webm）。size はビューポートに合わせる。
  video: process.env.RECORD === "1" ? { mode: "on", size: { width: 1200, height: 900 } } : "off",
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
  const skillName = uniqueName("deep-research-real");
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
    { skillName, body: bundle },
  );
  expect(created.status, created.text).toBeLessThan(300);

  await page.reload();
  const input = page.getByLabel("メッセージを入力");
  await input.fill("/deep-research");
  await expect(page.getByTestId("slash-command-menu")).toBeVisible({ timeout: 15_000 });
  // **いま作った skill の候補を選ぶ**。同じ dev DB を使い回すと `/deep-research` を名乗る
  // 過去の skill が何十件も候補に並び、先頭を取ると古い body（＝古い宣言）でピンされる。
  const mine = page.getByTestId("slash-command-option").filter({ hasText: skillName });
  // 候補は variant の宣言順（`""` → `"auto"`）で並ぶ。バンドルはこのテストが投入した
  // ものなので順序は既知。
  await expect(mine, `候補に ${skillName} の 2 variant が出ること`).toHaveCount(2, {
    timeout: 15_000,
  });
  await mine.nth(FLOW === "auto" ? 1 : 0).click();
  await input.fill(TOPIC);
  await page.getByRole("button", { name: "送信" }).click();

  if (FLOW !== "auto") {
    // ── フェーズ 0: 質問カード ──
    // `plan_first` の variant では**必ず**出る（明確化の run には `question_card` を出す
    // `emit_ui` しか提示されない・#400）。出ないなら門が効いていないので落とす。
    const options = page.getByTestId("genui-question-option");
    await expect(options.first()).toBeVisible({ timeout: 4 * 60 * 1000 });
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
    const titles = await page
      .getByTestId("genui-plan-steps")
      .getByTestId("plan-step-title")
      .allInnerTexts();
    console.log(`=== PLAN (${titles.length} steps) ===`);
    console.log(await page.getByTestId("genui-plan-steps").innerText());
    await page.screenshot({ path: `${SHOTS}/real-dr-plan.png`, fullPage: true });
    // 計画は**問いの一覧**であること（禁止語の列挙ではなく形式で判定する。工程を並べる語彙は
    // 無限にあり、列挙は必ず漏れる。「問いの形か」なら 1 つの規則で全部を捕まえられる）。
    // 許すのは「〜か」「〜か？」「〜？」「〜のはなぜか」等の疑問形。
    const asQuestion = /(か|？|\?)$/;
    const notQuestions = titles.map((t) => t.trim()).filter((t) => !asQuestion.test(t));
    expect(
      notQuestions,
      "計画の各項目は問いの形であること（工程を並べるとユーザーの判断材料にならない）",
    ).toEqual([]);
    expect(titles.length, "問いは 5〜7 個").toBeGreaterThanOrEqual(4);
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
