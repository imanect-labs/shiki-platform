import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";
import { ApiError } from "@/lib/admin-ai-api";

/// AI 活用分析（#751）と回答の評価の API。型は OpenAPI(utoipa) からの生成物。
export type InsightsOverview = components["schemas"]["Overview"];
export type InsightsAdoption = components["schemas"]["Adoption"];
export type InsightsPerson = components["schemas"]["PersonRow"];
export type InsightsUseCases = components["schemas"]["UseCases"];
export type InsightsImpact = components["schemas"]["Impact"];
export type InsightDepartment = components["schemas"]["InsightDepartment"];
export type InsightSettings = components["schemas"]["InsightSettingsDto"];
export type UpdateInsightSettings = components["schemas"]["UpdateInsightSettingsRequest"];
export type FeedbackItem = components["schemas"]["FeedbackItem"];
export type FeedbackRating = components["schemas"]["FeedbackRating"];
export type FeedbackCategory = components["schemas"]["FeedbackCategory"];

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let detail = "";
    try {
      detail = ((await res.json()) as { detail?: string }).detail ?? "";
    } catch {
      // 本文が JSON でない
    }
    const fallback =
      res.status === 403 ? "組織の管理者だけが使えます" : `エラーが発生しました（${res.status}）`;
    throw new ApiError(res.status, detail || fallback);
  }
  return (await res.json()) as T;
}

/// 期間と部署（日付は日本時間・両端含む）。
export type InsightsQuery = { from: string; to: string; dept?: string };

function qs(q: InsightsQuery): string {
  const p = new URLSearchParams({ from: q.from, to: q.to });
  if (q.dept) p.set("dept", q.dept);
  return p.toString();
}

export async function fetchOverview(q: InsightsQuery): Promise<InsightsOverview> {
  return json(await apiFetch(`/org-admin/insights/overview?${qs(q)}`));
}

export async function fetchAdoption(q: InsightsQuery): Promise<InsightsAdoption> {
  return json(await apiFetch(`/org-admin/insights/adoption?${qs(q)}`));
}

/// 個人別（組織の設定でオフなら 403）。
export async function fetchPeople(q: InsightsQuery): Promise<InsightsPerson[]> {
  return json(await apiFetch(`/org-admin/insights/people?${qs(q)}`));
}

export async function fetchUseCases(q: InsightsQuery): Promise<InsightsUseCases> {
  return json(await apiFetch(`/org-admin/insights/use-cases?${qs(q)}`));
}

export async function fetchImpact(q: InsightsQuery): Promise<InsightsImpact> {
  return json(await apiFetch(`/org-admin/insights/impact?${qs(q)}`));
}

export async function fetchInsightDepartments(): Promise<InsightDepartment[]> {
  return json(await apiFetch("/org-admin/insights/departments"));
}

export async function fetchInsightSettings(): Promise<InsightSettings> {
  return json(await apiFetch("/org-admin/insights/settings"));
}

export async function updateInsightSettings(body: UpdateInsightSettings): Promise<InsightSettings> {
  return json(
    await apiFetch("/org-admin/insights/settings", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),
  );
}

/// 自分がこの会話で付けた評価。
export async function fetchThreadFeedback(threadId: string): Promise<FeedbackItem[]> {
  const r = await json<{ items: FeedbackItem[] }>(
    await apiFetch(`/threads/${encodeURIComponent(threadId)}/feedback`),
  );
  return r.items;
}

export async function setMessageFeedback(
  threadId: string,
  messageId: string,
  rating: FeedbackRating,
  category?: FeedbackCategory | null,
): Promise<void> {
  const res = await apiFetch(
    `/threads/${encodeURIComponent(threadId)}/messages/${encodeURIComponent(messageId)}/feedback`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ rating, category: category ?? null }),
    },
  );
  if (!res.ok) await json(res);
}

export async function clearMessageFeedback(threadId: string, messageId: string): Promise<void> {
  const res = await apiFetch(
    `/threads/${encodeURIComponent(threadId)}/messages/${encodeURIComponent(messageId)}/feedback`,
    { method: "DELETE" },
  );
  if (!res.ok) await json(res);
}

/// 👎 の理由（API の FeedbackCategory と同じ集合・表示順）。
export const FEEDBACK_REASONS: { value: FeedbackCategory; label: string }[] = [
  { value: "inaccurate", label: "内容が不正確" },
  { value: "not_found", label: "社内の情報が見つからない" },
  { value: "not_followed", label: "指示どおりでない" },
  { value: "slow", label: "遅い" },
  { value: "how_to", label: "やり方がわからなかった" },
  { value: "other", label: "その他" },
];
