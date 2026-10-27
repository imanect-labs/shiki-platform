import { test, expect } from "@playwright/test";

import type { components } from "@/generated/api";
import { loginViaKeycloak, uniqueName } from "./helpers";

/// メッセージ一覧の応答型は **Rust の `chat::Message`（utoipa）から生成**したものを使う
/// （手書きミラーを増やさない・codegen が正）。`invoked_actions` の必須性・名前・要素型が
/// 変われば型検査で落ちる。型注釈は消えるので Playwright の実行には影響しない。
type MessagesResponse = { messages: components["schemas"]["Message"][] };

/// deep research（issue #387）の E2E: `/deep-research` の**質問 → 計画 → 実行**を通す。
///
/// 実 LLM は使わず、スタブプロバイダの `/deep-research` 駆動
/// （`crates/llm-gateway/src/providers/stub_deep_research.rs`）でフェーズを決定的に再現する。
/// 質問・計画は回答の本文の ```ui（Yui の ask・plan・#604）で出る。回答・承認は会話への発話として
/// **別 run** になるため、run を跨いで状態が復元されること
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
              // 本番のバンドルと同じく確認フェーズの門（#400）を掛ける。明確化・計画の段階では
              // ツールが渡らず、本文の ask・plan で段階が進む（#604）ことまで通しで見る。
              { args: "", summary: "質問→計画→実行（推奨）", phase: "plan_first" },
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

/// 本文の ```ui ask・plan の図（会話にそれぞれ 1 つ）。
const askFigure = (page: import("@playwright/test").Page) =>
  page.locator('[data-testid="ui-figure"][data-ui-kind="ask"]');
const planFigure = (page: import("@playwright/test").Page) =>
  page.locator('[data-testid="ui-figure"][data-ui-kind="plan"]');

/// 質問（スタブの 2 問・どちらも任意）に答えて送る。
async function answerQuestions(page: import("@playwright/test").Page) {
  const ask = askFigure(page);
  const submit = ask.getByRole("button", { name: "この条件で進める" });
  await expect(submit).toBeEnabled({ timeout: 60_000 });
  await ask.getByRole("radio", { name: /^国内のみ/ }).check();
  await ask.getByRole("radio", { name: /^意思決定用/ }).check();
  await submit.click();
  await expect(ask.getByRole("status")).toContainText("送信しました");
}

test("deep research: 質問（ask）→ 計画（plan）→ 調査 → レポート → 出典 → 下書き", async ({
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

  // ── フェーズ 0: 質問（本文の ```ui ask・最大 3 問・1 ターン） ──
  await answerQuestions(page);
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/deep-research-question.png`, fullPage: true });

  // ── フェーズ 1: 計画（本文の ```ui plan・開始ボタンで承認を取る） ──
  const plan = planFigure(page);
  const planStart = plan.getByRole("button", { name: "この計画で開始" });
  await expect(planStart).toBeEnabled({ timeout: 60_000 });
  // 計画は**この依頼固有の問い**が並ぶ（手順を並べない）。件数と、方法論の語が出ていないことを見る。
  await expect(plan.locator("ol > li")).toHaveCount(6);
  await expect(plan).toContainText("2026 年の実数はいくらか");
  for (const method of ["証拠台帳", "節ごとに執筆", "視点を分けて"]) {
    await expect(plan.getByText(method, { exact: false })).toHaveCount(0);
  }
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/deep-research-plan.png`, fullPage: true });
  await planStart.click();

  // ── フェーズ 2〜4: 調査 → レポート → 出典 → 下書き ──
  // レポート本文（節見出し・両論併記・見つからなかったことの明示）。
  await expect(page.getByText("結論と確度").first()).toBeVisible({ timeout: 90_000 });
  await expect(page.getByText("公表資料では確認できなかった").first()).toBeVisible();

  // ツール実行表示（#386）に取得した URL が具体的に出る。展開して全件を見る。
  // **最後の run** のものを見る（先行 run にも tool-activity が出ることがある）。
  // 完了後は 1 行要約に畳まれているので、ヘッダを押して全件のタイムラインを出す。
  const activity = page.getByTestId("tool-activity").last();
  await expect(activity).toBeVisible();
  await activity.getByRole("button").first().click();
  const expanded = page.getByTestId("tool-activity-expanded").last();
  await expect(expanded).toContainText("example.com/stub-1");
  await expect(expanded).toContainText("notes.md");
  // 裏取りは独立した検証者へ委譲し、**その指摘を反映してから**提出する（#407）。
  // 委譲した事実だけを見ると、指摘を無視する回帰を見逃す。
  await expect(expanded).toContainText("未確認・誤引用・過剰な一般化");
  await expect(expanded).toContainText("report.md");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/deep-research-activity.png`, fullPage: true });

  // 出典は会話の最後にリンクの一覧で並ぶ（旧 emit_ui の出典カードの置き換え・#604）と、
  // 保存ボタン（下書きノート）。
  await expect(page.getByRole("heading", { name: "出典", exact: true }).first()).toBeVisible();
  await expect(
    page.getByRole("link", { name: "国内 SaaS 市場調査（2026 年版）" }).first(),
  ).toHaveAttribute("href", "https://example.com/stub-1");
  const draft = page.getByTestId("note-draft-card").first();
  await expect(draft).toBeVisible();
  await expect(draft).toContainText("2026年 国内SaaS市場の調査");
  // 提出されるのは**検証の指摘を反映した後**の本文。証拠は E1 の 1 系統しか無いのに
  // 「独立 2 系統が一致」と断定していた箇所が、検証者の指摘で直っている。
  // ユーザーが受け取る面（会話・下書き）のどこにも断定が残っていないことを見る
  // ——委譲した事実だけを見ると、指摘を無視する回帰を通してしまう。
  await draft.click();
  await expect(page.getByText("出典 1 系統・別集計とは不一致").first()).toBeVisible();
  await expect(page.getByText("独立 2 系統が一致")).toHaveCount(0);
  // レポートの ```ui はノートでも図として描かれる（文字のままにしない）。
  const figure = page.getByTestId("note-yui-figure").first();
  await expect(figure.locator('[data-testid="ui-figure"][data-ui-kind="bar"]')).toBeVisible();
  await expect(figure).toContainText("公表値の比較");
  if (SHOTS) await page.screenshot({ path: `${SHOTS}/deep-research-report.png`, fullPage: true });

  // 承認カードは出ない（作業メモはシステム領域＝事前許可・#392）。
  await expect(page.getByText("承認が必要です")).toHaveCount(0);
});

/// 回答済み・開始済みの質問・計画が未操作へ巻き戻り、二度送信できてしまう回帰（#410）。
///
/// 見た目だけの話ではない。計画の「開始」を二度押せると**調査がまるごと二重に走る**
/// （実測で 1 本あたり 253 ツール操作・十数分・実費）。表示の根拠（サーバ記録）と
/// サーバ側の拒否は**対で**確かめる — 片方だけでは押せてしまう / 押せないのに未操作に見える。
/// 押下がサーバの実行台帳へ届いた件数（`invoked_actions` を持つメッセージ数）。
///
/// UI の表示は待ちの根拠にならない。生成中に押した操作はクライアント側の順番待ちに積まれる
/// だけで、確定メッセージへの差し替えで「順番待ち」の札は**送信前に**消え得る。その状態で
/// リロードすると積んだ操作ごと消えるので、サーバの記録そのものを見る。
async function invokedCount(page: import("@playwright/test").Page): Promise<number> {
  return page.evaluate(async () => {
    const threadId = location.pathname.split("/").filter(Boolean).pop();
    const res = await fetch(`/api/threads/${threadId}/messages`, { credentials: "include" });
    const data = (await res.json()) as MessagesResponse;
    return data.messages.filter((m) => (m.invoked_actions ?? []).length > 0).length;
  });
}

test("回答済み・開始済みの質問・計画は巻き戻らず、二度送信もできない（#410）", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/");
  await createDeepResearchSkill(page);
  await page.reload();

  const input = page.getByLabel("メッセージを入力");
  await input.fill(`/${COMMAND}`);
  await expect(page.getByTestId("slash-command-menu")).toBeVisible({ timeout: 10_000 });
  await input.press("Enter");
  await input.fill("2026 年の国内 SaaS 市場規模を調べて");
  await page.getByRole("button", { name: "送信" }).click();
  await page.waitForURL(/\/c\/[0-9a-f-]+/i, { timeout: 20_000 });

  // 質問に回答する。
  await answerQuestions(page);
  await expect
    .poll(() => invokedCount(page), {
      timeout: 60_000,
      message: "質問への回答がサーバへ記録されること",
    })
    .toBeGreaterThanOrEqual(1);

  // リロードしても未回答へ戻らない（送信済みはローカル state ではなくサーバ記録が正）。
  await page.reload();
  const ask = askFigure(page);
  await expect(ask.getByText("回答済みです")).toBeVisible({ timeout: 30_000 });
  await expect(ask.getByRole("button", { name: "この条件で進める" })).toBeDisabled();

  // サーバも二度目を拒否する（409）。拒否された送信は発話も生成も作らない。
  const retry = await page.evaluate(async () => {
    const threadId = location.pathname.split("/").pop();
    const csrf = document.cookie.match(/(?:^|;\s*)shiki_csrf=([^;]+)/);
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (csrf) headers["X-CSRF-Token"] = decodeURIComponent(csrf[1]);
    const list = async () =>
      (await (
        await fetch(`/api/threads/${threadId}/messages`, { credentials: "include" })
      ).json()) as MessagesResponse;
    const before = await list();
    const card = before.messages.find((m) => (m.invoked_actions ?? []).length > 0);
    if (!card) return { status: 0, actionId: null, before: 0, after: 0 };
    const res = await fetch(`/api/threads/${threadId}/messages/${card.id}/ui-actions`, {
      method: "POST",
      credentials: "include",
      headers,
      // 値は正しい（質問はすべて任意）。値の検証ではなく「1 回だけ」で拒否されることを見る。
      body: JSON.stringify({ action_id: card.invoked_actions?.[0], params: {} }),
    });
    const after = await list();
    return {
      status: res.status,
      actionId: card.invoked_actions?.[0] ?? null,
      before: before.messages.length,
      after: after.messages.length,
    };
  });
  expect(retry.actionId, "実行済み action がメッセージと一緒に返ること").not.toBeNull();
  expect(retry.status, "二度目は 409").toBe(409);
  expect(retry.after, "拒否された送信は発話を作らない").toBe(retry.before);

  // 計画も同じ（こちらの二度押しが調査の二重実行になる）。
  const plan = planFigure(page);
  const planStart = plan.getByRole("button", { name: "この計画で開始" });
  await expect(planStart).toBeEnabled({ timeout: 60_000 });
  await planStart.click();
  await expect(plan.getByRole("status")).toContainText("送信しました");
  await expect
    .poll(() => invokedCount(page), {
      timeout: 60_000,
      message: "計画の押下がサーバへ記録されること",
    })
    .toBeGreaterThanOrEqual(2);
  await page.reload();
  await expect(planFigure(page).getByText("回答済みです")).toBeVisible({ timeout: 30_000 });
  await expect(
    planFigure(page).getByRole("button", { name: "この計画で開始" }),
  ).toBeDisabled();
  if (SHOTS) {
    await page.screenshot({ path: `${SHOTS}/deep-research-invoked.png`, fullPage: true });
  }
});

test("deep research auto: 確認を省略して 1 ターンで完走する", async ({ page }) => {
  // 進捗カード（PlanPanel）は run の間だけ出て、終わると消える。表示の変化を画面の中で記録し、
  // 0 のまま止まらず最後まで進んだことを後から確かめる（出たきり動かない回帰を防ぐ）。
  await page.addInitScript(() => {
    const seen: string[] = [];
    (window as unknown as { __planProgress: string[] }).__planProgress = seen;
    const record = () => {
      const el = document.querySelector('[data-testid="plan-progress"]');
      const text = el?.textContent?.trim();
      if (text && seen[seen.length - 1] !== text) seen.push(text);
    };
    new MutationObserver(record).observe(document, {
      subtree: true,
      childList: true,
      characterData: true,
    });
  });
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

  // 質問も計画も出さず、そのままレポートまで進む。
  await expect(page.getByText("結論と確度").first()).toBeVisible({ timeout: 90_000 });
  await expect(page.locator('[data-ui-kind="ask"], [data-ui-kind="plan"]')).toHaveCount(0);
  await expect(page.getByTestId("note-draft-card").first()).toBeVisible();
  // 進捗カードは節目ごとに進み、提出で全部完了になる（スタブは手順書どおり 0→1→2→4 / 4）。
  const progress = await page.evaluate(
    () => (window as unknown as { __planProgress: string[] }).__planProgress,
  );
  expect(progress, "進捗カードの表示の変化").toEqual(["0/4", "1/4", "2/4", "4/4"]);
  if (SHOTS) {
    await page.screenshot({ path: `${SHOTS}/deep-research-auto.png`, fullPage: true });
  }
});
