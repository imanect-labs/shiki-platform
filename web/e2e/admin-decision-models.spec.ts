import { expect, test, type Page } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// 判定用のモデル（FEAT-1・modality=decision）の管理画面とモデル一覧の E2E。
///
/// - 管理画面: 判定用のモデルに「判定用」と元のモデルを出し、既定・Auto の振り先には選べない。chat モデルを全部
///   非公開にすると、判定用のモデルが公開でも保存できない。
/// - `/llm/models`: 既定（chat）には判定用のモデルが出ず、`?modality=decision` は判定用のモデルだけを返す。
/// 前提: 判定用のモデルを持つカタログ（`~/.config/shiki-dev/llm-stub.toml` の decide-stub など）・LLM は stub。
/// 判定用のモデルが無い構成（env だけの compose など）では流さない。実 LLM は呼ばない（判定は実行しない）。

type AdminModel = {
  id: string;
  label: string;
  enabled: boolean;
  modality: "chat" | "decision";
  decision_via?: string | null;
};

async function csrfHeader(page: Page): Promise<Record<string, string>> {
  const csrf = (await page.context().cookies()).find((c) => c.name === "shiki_csrf")?.value ?? "";
  return { "x-csrf-token": csrf, "content-type": "application/json" };
}

async function adminModels(page: Page): Promise<AdminModel[]> {
  const res = await page.request.get("/api/org-admin/ai/models");
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { models: AdminModel[] }).models;
}

async function setEnabled(page: Page, models: { id: string; enabled: boolean }[]) {
  const res = await page.request.put("/api/org-admin/ai/settings", {
    headers: await csrfHeader(page),
    data: { models },
  });
  expect(res.ok(), await res.text()).toBeTruthy();
}

async function listIds(page: Page, query: string): Promise<string[]> {
  const res = await page.request.get(`/api/llm/models${query}`);
  expect(res.ok(), await res.text()).toBeTruthy();
  return ((await res.json()) as { models: { id: string }[] }).models.map((m) => m.id);
}

test.describe("判定用のモデル", () => {
  test("管理画面で判定用と分かり、既定・Auto に選べず、チャットの一覧に出ない", async ({ page }) => {
    await loginViaKeycloak(page);
    const before = await adminModels(page);
    const decision = before.find((m) => m.modality === "decision" && !m.decision_via);
    test.skip(!decision, "判定用のモデル（stub の決定器）がカタログに無い構成");
    if (!decision) return;
    const chatIds = before.filter((m) => m.modality === "chat").map((m) => m.id);
    try {
      // 判定用のモデルを公開して保存する（元に戻すのは finally）。
      if (!decision.enabled) await setEnabled(page, [{ id: decision.id, enabled: true }]);

      await page.goto("/admin/ai/models");
      await expect(page.getByRole("heading", { name: "モデル", exact: true })).toBeVisible();
      const table = page.locator("table");
      const row = table.getByRole("row").filter({ hasText: decision.id });
      await expect(row.getByText("判定用", { exact: true })).toBeVisible();
      // 既定にはできない（ラジオが無い）。chat モデルにはある。
      await expect(page.getByRole("radio", { name: `${decision.label} を既定にする` })).toHaveCount(0);
      // Auto の振り先の候補に出ない。
      await page.getByRole("combobox", { name: "Auto のモデル" }).click();
      await expect(page.getByRole("option", { name: "デプロイ設定に従う" })).toBeVisible();
      await expect(page.getByRole("option", { name: decision.label, exact: true })).toHaveCount(0);
      await page.keyboard.press("Escape");

      // chat モデルを全部非公開にすると、判定用のモデルが公開でも保存できない（理由が出る）。
      for (const id of chatIds) {
        const label = before.find((m) => m.id === id)?.label ?? id;
        const sw = table.getByRole("switch", { name: `${label} を公開` });
        if ((await sw.getAttribute("aria-checked")) === "true") await sw.click();
      }
      await expect(
        page.getByText("チャットで使えるモデル（判定用以外）を少なくとも 1 つ公開してください"),
      ).toBeVisible();
      await expect(page.getByRole("button", { name: "変更を保存" })).toBeDisabled();
      await page.getByRole("button", { name: "取り消す" }).click();
      await expect(page.getByRole("button", { name: "変更を保存" })).toHaveCount(0);

      // チャット・ワークフローのモデル欄（chat）には出ず、判定ノードのモデル欄には出る。
      expect(await listIds(page, "")).not.toContain(decision.id);
      expect(await listIds(page, "?purpose=workflow")).not.toContain(decision.id);
      const decisions = await listIds(page, "?purpose=workflow&modality=decision");
      expect(decisions).toContain(decision.id);
      for (const id of decisions) expect(chatIds).not.toContain(id);
    } finally {
      if (!decision.enabled) await setEnabled(page, [{ id: decision.id, enabled: false }]);
    }
    const after = await adminModels(page);
    expect(after.find((m) => m.id === decision.id)?.enabled).toBe(decision.enabled);
  });

  test("スマホ幅でも判定用と分かり、既定にするの欄が出ない", async ({ browser }) => {
    const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true });
    const page = await ctx.newPage();
    try {
      await loginViaKeycloak(page);
      const models = await adminModels(page);
      const decision = models.find((m) => m.modality === "decision");
      test.skip(!decision, "判定用のモデルがカタログに無い構成");
      if (!decision) return;
      await page.goto("/admin/ai/models");
      const card = page.getByRole("list", { name: "モデル" }).getByRole("listitem").filter({ hasText: decision.label });
      await expect(card.getByText("判定用", { exact: true })).toBeVisible();
      await expect(card.getByRole("radio")).toHaveCount(0);
      // ページが横にはみ出さない。
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      expect(overflow).toBeLessThanOrEqual(0);
    } finally {
      await ctx.close();
    }
  });
});
