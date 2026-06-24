import { defineConfig, devices } from "@playwright/test";

/// E2E は BFF（オパークセッション Cookie）＋ Keycloak の実フローを検証する。
/// 前提（CI / ローカル共通）:
///   - compose で keycloak(:8081) と shiki-server(:8080) が起動している。
///   - shiki-server の SHIKI__AUTH__REDIRECT_URI = http://localhost:3000/auth/callback。
///   - web は :3000 で起動し、BACKEND_ORIGIN=http://localhost:8080 でプロキシする。
/// realm の shiki-web クライアントは http://localhost:3000/* を許可済み（deploy/keycloak）。
const PORT = 3000;
const BASE_URL = `http://localhost:${PORT}`;

export default defineConfig({
  testDir: "./e2e",
  // OIDC ラウンドトリップを跨ぐためテスト間の状態混線を避け直列実行する。
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["github"], ["list"]] : "list",
  timeout: 60_000,
  use: {
    baseURL: BASE_URL,
    trace: "on-first-retry",
    // 自己署名や混在オリジン（:3000 / :8080 / :8081）でも進めるよう緩める。
    ignoreHTTPSErrors: true,
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  // 本番ビルド済みの web を起動する。ローカルで既に :3000 が動いていれば再利用。
  webServer: {
    command: "pnpm start",
    url: BASE_URL,
    timeout: 120_000,
    reuseExistingServer: !process.env.CI,
    env: {
      BACKEND_ORIGIN: process.env.BACKEND_ORIGIN ?? "http://localhost:8080",
    },
  },
});
