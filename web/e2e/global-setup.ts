import { chromium, type FullConfig } from "@playwright/test";

import { loginViaKeycloak, orgDefaultProblem } from "./helpers";

/// E2E を流す前の前提の確かめ（BUG-115）: 組織の既定（モデルを省略した呼び出し・おまかせで使うモデル）が stub で
/// 動くこと。チャットを送る E2E の多くはこれを前提にしていて、外れていると黙って実 LLM を呼ぶ。外れていれば、何を
/// どう直すかを言って、テストを始める前に止める（実 LLM は呼ばない。確かめるのは組織の管理者の API だけ）。
///
/// 実 LLM のまま流すと決めたときだけ `E2E_ALLOW_REAL_LLM_DEFAULT=1` で飛ばす。
export default async function globalSetup(config: FullConfig): Promise<void> {
  if (process.env.E2E_ALLOW_REAL_LLM_DEFAULT === "1") return;
  const baseURL = config.projects[0]?.use.baseURL ?? process.env.E2E_BASE_URL ?? "http://localhost:3000";
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ baseURL, ignoreHTTPSErrors: true });
    await loginViaKeycloak(page);
    const problem = await orgDefaultProblem(page);
    if (problem) throw new Error(problem);
  } finally {
    await browser.close();
  }
}
