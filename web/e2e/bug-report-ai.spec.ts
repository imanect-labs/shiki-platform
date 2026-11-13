import { test, expect } from "@playwright/test";
import { content, report, policy, shot, loginReport } from "./bug-report-fixtures";

// UI は決定的なローカル応答に固定。サーバの実検索証拠・本人/分岐/権限は Rust IT で検証する。
test.use({ deviceScaleFactor: 2, colorScheme: process.env.BUG_REPORT_DARK ? "dark" : "light", viewport: { width: 1440, height: 1000 } });
const thread = "22345678-1234-4234-8234-123456789abc";
const caseId = "32345678-1234-4234-8234-123456789abc";
const guidance = "42345678-1234-4234-8234-123456789abc";
for (const mode of ["hits", "no_hits", "not_searched", "search_failed"] as const) {
  test(`AI報告: ${mode}`, async ({ page }) => {
    await loginReport(page);
    let resolution = "unknown", intent = false, drafted = false, submitted = false, attempts = 0;
    const receipt = mode === "hits" || mode === "no_hits";
    const caseValue = () => ({ id: caseId, guidance_message_id: guidance, hit_count: mode === "hits" ? 1 : 0, resolution, report_intent: intent, report_id: drafted ? report.id : null });
    await page.route(new RegExp(`/api/threads/${thread}(?:/.*)?$`), async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path.includes("/help-cases/")) {
        if (!receipt) return route.fulfill({ status: 404, json: {} });
        if (path.endsWith("/resolution")) resolution = route.request().postDataJSON();
        if (path.endsWith("/report-intent")) {
          if (mode === "hits" && resolution !== "unresolved") return route.fulfill({ status: 409, json: {} });
          intent = true;
        }
        return route.fulfill({ json: caseValue() });
      }
      if (path.endsWith("/messages")) {
        if (route.request().method() === "POST") {
          expect(intent).toBe(true);
          attempts++;
          if (attempts === 1) return route.fulfill({ status: 503, json: { error: "一時的な送信失敗" } });
          drafted = true;
          return route.fulfill({ json: { run_id: "r", user_message_id: "u2", assistant_message_id: "a2" } });
        }
        return route.fulfill({ json: { messages: [{ id: "u", role: "user", content: [{ type: "text", text: "保存できません。使い方を教えてください。" }], created_at: "2026-11-14T00:00:00Z" }, { id: guidance, role: "assistant", content: [{ type: "text", text: mode === "search_failed" ? "ヘルプを検索できませんでした。" : mode === "not_searched" ? "まだヘルプを検索していません。" : "ヘルプを調べました。保存した後で画面を再読み込みしてください。" }, ...(receipt ? [{ type: "help_case_ref", case_id: caseId }] : [])], created_at: "2026-11-14T00:00:01Z" }, ...(drafted ? [{ id: "a2", role: "assistant", content: [{ type: "text", text: "報告の下書きを作成しました。" }, { type: "bug_report_draft", report_id: report.id, rev: 1 }], created_at: "2026-11-14T00:00:02Z" }] : [])], active_run_id: null } });
      }
      if (path.endsWith("/stream")) return route.fulfill({ contentType: "text/event-stream", body: `data: ${JSON.stringify({ type: "bug_report_draft", report_id: report.id, rev: 1 })}\n\ndata: ${JSON.stringify({ type: "done", message_id: "a2" })}\n\n` });
      if (path === `/api/threads/${thread}`) return route.fulfill({ json: { id: thread, title: "不具合の相談", agent_mode: false, skill_pins: [], created_at: "2026-11-14T00:00:00Z", updated_at: "2026-11-14T00:00:00Z" } });
      return route.fulfill({ json: {} });
    });
    await page.route("**/api/bug-reports/**", async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path.endsWith("/policy")) return route.fulfill({ json: { policy, destination_available: false, destination_version: null } });
      if (path.endsWith("/submit")) { submitted = true; return route.fulfill({ json: report }); }
      return route.fulfill({ json: { report: { ...report, status: submitted ? "held" : "draft", rev: submitted ? 2 : 1, submission_rev: submitted ? 1 : 0 }, submission: submitted ? { content, content_consent: true, sink_profile_id: "disabled", sink_config_version: 1, notice_version: "bug-report-v1" } : null } });
    });
    await page.goto(`/c/${thread}`);
    if (!receipt) {
      await expect(page.getByText(mode === "search_failed" ? "ヘルプを検索できませんでした。" : "まだヘルプを検索していません。", { exact: true })).toBeVisible();
      await expect(page.getByRole("button", { name: "下書きを作る" })).toHaveCount(0);
      await shot(page, `ai-${mode}-denied`);
      return;
    }
    await expect(page.getByRole("region", { name: "ヘルプの解決確認" })).toBeVisible();
    await shot(page, `ai-${mode}-guidance`);
    if (mode === "hits") {
      await expect(page.getByRole("button", { name: "下書きを作る" })).toHaveCount(0);
      await shot(page, "ai-unresolved-confirmation");
      await page.getByRole("button", { name: "解決しなかった" }).focus();
      await page.keyboard.press("Enter");
    }
    await expect(page.getByText("不具合として報告しますか？", { exact: true })).toBeVisible();
    await shot(page, `ai-${mode}-proposal`);
    await page.getByRole("button", { name: "下書きを作る" }).click();
    await expect(page.getByRole("button", { name: "下書きの作成を再試行" })).toBeEnabled();
    await shot(page, `ai-${mode}-retry`);
    // report_intent は保存済み。再読み込み後も要求を再送できる。
    await page.reload();
    await page.getByRole("button", { name: "下書きの作成を再試行" }).focus();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("link", { name: "要約を確認・編集する", exact: true })).toBeVisible();
    expect(attempts).toBe(2);
    await shot(page, `ai-${mode}-draft`);
    await page.getByRole("link", { name: "要約を確認・編集する", exact: true }).click();
    await expect(page.getByLabel("どのようなことが起きましたか")).toHaveValue(content.symptom);
    await page.getByRole("checkbox").check();
    await page.getByRole("button", { name: "送信内容を確認" }).click();
    await expect(page.getByRole("dialog")).toBeVisible();
    await shot(page, `ai-${mode}-summary-confirmation`);
    await page.getByRole("button", { name: "この内容で報告する" }).click();
    await expect(page.getByText("保留（外部には未送信）", { exact: true })).toBeVisible();
    await shot(page, `ai-${mode}-submitted`);
    expect(submitted).toBe(true);
  });
}
