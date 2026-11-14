import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// ホームの七十二候（#1188）。境目は太陽黄経から計算するので、国立天文台の暦（2026 年）と
/// 同じ日に切り替わることを、時計を固定して確かめる。端末のタイムゾーンは日本時間に揃える。
test.use({ timezoneId: "Asia/Tokyo", locale: "ja-JP" });

const CASES: { at: string; name: string }[] = [
  { at: "2026-02-03T12:00:00+09:00", name: "鶏始乳" }, // 大寒の末候（節分）
  { at: "2026-02-04T12:00:00+09:00", name: "東風解凍" }, // 立春
  { at: "2026-11-11T12:00:00+09:00", name: "山茶始開" },
  { at: "2026-11-12T12:00:00+09:00", name: "地始凍" }, // 立冬の次候
  // 境目の瞬間（11/12 の昼過ぎ）より前でも、その日のうちに越えるなら新しい候の初日になる。
  { at: "2026-11-12T00:30:00+09:00", name: "地始凍" },
  { at: "2026-12-22T12:00:00+09:00", name: "乃東生" }, // 冬至
];

test.describe("ホームの七十二候", () => {
  test.beforeEach(async ({ page }) => {
    await loginViaKeycloak(page);
  });

  for (const c of CASES) {
    test(`${c.at} は「${c.name}」`, async ({ page }) => {
      await page.clock.setFixedTime(new Date(c.at));
      await page.goto("/");
      await expect(page.getByTestId("home-microseason")).toHaveText(c.name);
    });
  }

  test("候名を押すと読み・意味・節気と説明リンクが出て、Esc で閉じる", async ({ page }) => {
    await page.clock.setFixedTime(new Date("2026-11-12T12:00:00+09:00"));
    await page.goto("/");
    await page.getByTestId("home-microseason").click();
    const detail = page.getByTestId("home-microseason-detail");
    await expect(detail).toContainText("ちはじめてこおる");
    await expect(detail).toContainText("大地が凍り始める");
    await expect(detail).toContainText("二十四節気「立冬」の次候");
    const link = detail.getByRole("link", { name: "七十二候について" });
    await expect(link).toHaveAttribute("href", /wikipedia\.org/);
    await expect(link).toHaveAttribute("target", "_blank");
    await page.keyboard.press("Escape");
    await expect(detail).toBeHidden();
  });
});
