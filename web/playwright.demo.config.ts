import { defineConfig, devices } from "@playwright/test";

/// #338 共有リンク／一般アクセスのデモ録画用 config（本番 e2e とは別・video を有効化）。
/// 前提: shiki-server(:8080) と keycloak(:8081) が起動済み。web は :3000 で自前起動する。
const BASE_URL = process.env.E2E_BASE_URL ?? "http://localhost:3000";

export default defineConfig({
  testDir: "./e2e-demo",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: "list",
  timeout: 180_000,
  use: {
    baseURL: BASE_URL,
    ignoreHTTPSErrors: true,
    viewport: { width: 1280, height: 800 },
    // 見やすいように録画とゆっくり操作を有効化。
    video: { mode: "on", size: { width: 1280, height: 800 } },
    launchOptions: { slowMo: 350 },
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: process.env.E2E_BASE_URL
    ? undefined
    : {
        command: "pnpm start",
        url: BASE_URL,
        timeout: 120_000,
        reuseExistingServer: true,
        env: { BACKEND_ORIGIN: process.env.BACKEND_ORIGIN ?? "http://localhost:8080" },
      },
});
