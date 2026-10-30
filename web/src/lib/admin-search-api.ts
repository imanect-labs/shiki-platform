import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";
import { ApiError } from "@/lib/admin-ai-api";

/// 社内文書の検索の取り込み（組織管理者・`/org-admin/search/*`・D-126）。型は OpenAPI(utoipa) から生成した
/// ものだけを使う（手書きの対応物を作らない）。
export type SearchIngestStatus = components["schemas"]["SearchIngestStatus"];
export type BackfillStatus = components["schemas"]["BackfillStatus"];
export type StartBackfillResponse = components["schemas"]["StartBackfillResponse"];

export { ApiError };

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = "";
    try {
      const body = (await res.json()) as { message?: string };
      message = body.message ?? "";
    } catch {
      // 本文が JSON でない（プロキシのエラーページ等）
    }
    const fallback =
      res.status === 403
        ? "組織の管理者だけが使えます。"
        : "サーバーにつながりませんでした。少し待ってから、もう一度読み込んでください。";
    throw new ApiError(res.status, message || fallback);
  }
  return (await res.json()) as T;
}

/// 取り込みの様子（待っている件数と、いちばん新しいまとめての取り込みの進み具合）。
export async function fetchSearchIngest(): Promise<SearchIngestStatus> {
  return json(await apiFetch("/org-admin/search/backfill"));
}

/// 取り込みの記録が無いファイルのまとめての取り込みを始める（動いていれば、それを返す）。
export async function startSearchBackfill(): Promise<StartBackfillResponse> {
  return json(await apiFetch("/org-admin/search/backfill", { method: "POST" }));
}
