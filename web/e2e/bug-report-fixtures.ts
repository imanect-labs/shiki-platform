import { loginViaKeycloak } from "./helpers";
export const content = {
  category: "bug",
  title: "ボタンを押しても画面が変わらない",
  symptom: "保存ボタンを押しても動きません",
  steps_tried: "再読み込みしました",
  expected_behavior: "保存した内容を表示してほしいです",
};
export const report = {
  id: "12345678-1234-4234-8234-123456789abc",
  reporter_id: "alice",
  status: "held",
  rev: 2,
  submission_rev: 1,
  content,
};
export const policy = {
  enabled: true,
  require_approval: true,
  allow_content_sharing: true,
  rev: 1,
};

// 明示実行時だけレビュー用の画面画像を残す。
export async function shot(
  page: import("@playwright/test").Page,
  name: string,
) {
  const dir = process.env.BUG_REPORT_SHOTS;
  if (!dir) return;
  await page.screenshot({
    path: `${dir}/${process.env.BUG_REPORT_MOBILE ? "mobile" : "desktop"}${process.env.BUG_REPORT_DARK ? "-dark" : ""}-${name}.png`,
    fullPage: true,
    animations: "disabled",
  });
}

export async function loginReport(page: import("@playwright/test").Page) {
  if (process.env.BUG_REPORT_MOBILE)
    await page.setViewportSize({ width: 1440, height: 1000 });
  await loginViaKeycloak(page);
  if (process.env.BUG_REPORT_MOBILE)
    await page.setViewportSize({ width: 390, height: 844 });
}
