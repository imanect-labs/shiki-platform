import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";
import { ApiError } from "@/lib/admin-ai-api";

/// 監査ログ（#634）の API。型は OpenAPI(utoipa) からの生成物。
export type AuditEvent = components["schemas"]["AuditEvent"];
export type AuditEventsResponse = components["schemas"]["AuditEventsResponse"];

/// 検索条件（日付は日本時間・両端含む）。空文字の条件は送らない。
export type AuditQuery = {
  from: string;
  to: string;
  actor?: string;
  action?: string;
  decision?: "" | "allow" | "deny";
  object_type?: string;
  object_id?: string;
  chunk_id?: string;
  trace_id?: string;
};

/// 絞り込みに使うキー（URL の検索パラメータと同じ名前）。
export const AUDIT_FILTER_KEYS = [
  "from",
  "to",
  "actor",
  "action",
  "decision",
  "object_type",
  "object_id",
  "chunk_id",
  "trace_id",
] as const satisfies readonly (keyof AuditQuery)[];

export function auditParams(q: AuditQuery): URLSearchParams {
  const p = new URLSearchParams();
  for (const k of AUDIT_FILTER_KEYS) {
    const v = q[k]?.trim();
    if (v) p.set(k, v);
  }
  return p;
}

export async function fetchAuditEvents(q: AuditQuery, cursor?: string | null): Promise<AuditEventsResponse> {
  const p = auditParams(q);
  if (cursor) p.set("cursor", cursor);
  const res = await apiFetch(`/org-audit/events?${p.toString()}`);
  if (!res.ok) {
    let detail = "";
    try {
      detail = ((await res.json()) as { detail?: string }).detail ?? "";
    } catch {
      // 本文が JSON でない
    }
    const fallback =
      res.status === 403 ? "監査ログを見る権限がありません" : `エラーが発生しました（${res.status}）`;
    throw new ApiError(res.status, detail || fallback);
  }
  return (await res.json()) as AuditEventsResponse;
}

/// 書き出しの URL（ブラウザに直接ダウンロードさせる。サーバ側で監査に残る）。
export function auditExportUrl(q: AuditQuery, format: "csv" | "jsonl"): string {
  const p = auditParams(q);
  p.set("format", format);
  return `/api/org-audit/export?${p.toString()}`;
}
