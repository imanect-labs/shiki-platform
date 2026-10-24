import { expect, test } from "@playwright/test";

import { loginViaKeycloak } from "./helpers";

/// issue #880: チャットの「ローカルからアップロード」で、ドライブに同じ名前のファイルが既にあると
/// finalize が 409 になり「アップロードに失敗しました」で止まっていた（iPhone の写真は
/// `IMG_xxxx.jpeg` で重なりやすく、同じ写真をもう一度添付するだけで起きる）。
/// 同じ名前があれば連番名（`名前 (1).拡張子`）で置き、添付として付くこと。

test("同じ名前のファイルをもう一度添付すると、連番名で保存されて添付される", async ({ page }) => {
  await loginViaKeycloak(page);
  await page.goto("/");
  const name = `attach-${Date.now()}.txt`;
  const numbered = name.replace(/\.txt$/, " (1).txt");
  const input = page.locator('input[type="file"]:not([multiple])').first();

  for (const expected of [name, numbered]) {
    await input.setInputFiles({
      name,
      mimeType: "text/plain",
      buffer: Buffer.from(`same name ${expected}\n`),
    });
    // 添付チップに、実際に保存された名前が出る（失敗のトーストは出ない）。
    await expect(page.getByText(expected, { exact: true }).first()).toBeVisible({ timeout: 20_000 });
  }
  await expect(page.getByText(`${name} のアップロードに失敗しました`)).toHaveCount(0);

  // ドライブにも両方が残っている（上書きしていない）。
  const names = await page.evaluate(async (q) => {
    const res = await fetch(`/api/nodes?q=${encodeURIComponent(q)}`, { credentials: "include" });
    if (!res.ok) throw new Error(`nodes: ${res.status}`);
    const body = (await res.json()) as { items: { name: string }[] };
    return body.items.map((n) => n.name).sort();
  }, name.replace(/\.txt$/, ""));
  expect(names).toEqual([name, numbered].sort());
});
