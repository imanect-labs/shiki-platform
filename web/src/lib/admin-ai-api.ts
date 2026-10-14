import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";

/// OpenAPI(utoipa) から生成した型（手書きの対応物を作らない）。
export type LlmModelsResponse = components["schemas"]["LlmModelsResponse"];
export type LlmModelOption = components["schemas"]["LlmModelOption"];
export type EffortLevel = components["schemas"]["EffortLevel"];
export type AdminProvider = components["schemas"]["AdminProvider"];
export type AdminModel = components["schemas"]["AdminModel"];
export type AdminModelsResponse = components["schemas"]["AdminModelsResponse"];
export type AiSettings = components["schemas"]["AiSettings"];
export type UpdateAiSettingsRequest = components["schemas"]["UpdateAiSettingsRequest"];
export type UsageResponse = components["schemas"]["UsageResponse"];
export type UsageRow = components["schemas"]["UsageRowDto"];

/// API のエラー（400 の detail をそのまま表示に使う）。
export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let detail = "";
    try {
      const body = (await res.json()) as { detail?: string };
      detail = body.detail ?? "";
    } catch {
      // 本文が JSON でない（プロキシのエラーページ等）
    }
    const fallback =
      res.status === 403
        ? "組織の管理者だけが使えます"
        : res.status === 503
          ? "AI が構成されていません"
          : `エラーが発生しました（${res.status}）`;
    throw new ApiError(res.status, detail || fallback);
  }
  return (await res.json()) as T;
}

/// チャットで選べるモデル（一般ユーザー）。
export async function fetchLlmModels(): Promise<LlmModelsResponse> {
  return json(await apiFetch("/llm/models"));
}

export async function fetchAdminProviders(): Promise<AdminProvider[]> {
  return json(await apiFetch("/org-admin/ai/providers"));
}

export async function fetchAdminModels(): Promise<AdminModelsResponse> {
  return json(await apiFetch("/org-admin/ai/models"));
}

export async function updateAiSettings(body: UpdateAiSettingsRequest): Promise<AiSettings> {
  return json(
    await apiFetch("/org-admin/ai/settings", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),
  );
}

export type UsageQuery = {
  from?: string;
  to?: string;
  model?: string;
  source?: string;
};

export async function fetchUsage(q: UsageQuery): Promise<UsageResponse> {
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v) params.set(k, v);
  const qs = params.toString();
  return json(await apiFetch(`/org-admin/ai/usage${qs ? `?${qs}` : ""}`));
}
