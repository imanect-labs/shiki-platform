"use client";

import * as React from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";

import { AUDIT_FILTER_KEYS, auditParams, type AuditQuery } from "@/lib/audit-api";
import { addDays, todayJst } from "../format";
import { isAuditCategory } from "./audit-vocab";

/// 既定の期間（直近 7 日・日本時間・両端含む）。
export function defaultRange(): { from: string; to: string } {
  const to = todayJst();
  return { from: addDays(to, -6), to };
}

const DATE = /^\d{4}-\d{2}-\d{2}$/;

/// 検索条件は URL に持つ（再読み込み・共有・戻るで同じ結果になる）。
export function useAuditQuery(): {
  query: AuditQuery;
  setQuery: (next: AuditQuery) => void;
  patch: (p: Partial<AuditQuery>) => void;
} {
  const params = useSearchParams();
  const router = useRouter();
  const pathname = usePathname();
  const key = params.toString();
  const query = React.useMemo<AuditQuery>(() => {
    const p = new URLSearchParams(key);
    const range = defaultRange();
    const q: AuditQuery = {
      from: DATE.test(p.get("from") ?? "") ? (p.get("from") as string) : range.from,
      to: DATE.test(p.get("to") ?? "") ? (p.get("to") as string) : range.to,
    };
    for (const k of AUDIT_FILTER_KEYS) {
      if (k === "from" || k === "to") continue;
      const v = p.get(k);
      if (v) (q as Record<string, string>)[k] = v;
    }
    const d = p.get("decision");
    q.decision = d === "allow" || d === "deny" ? d : undefined;
    const c = p.get("category");
    q.category = isAuditCategory(c) ? c : undefined;
    return q;
  }, [key]);
  const setQuery = React.useCallback(
    (next: AuditQuery) => {
      const qs = auditParams(next).toString();
      router.replace(qs ? `${pathname}?${qs}` : pathname, { scroll: false });
    },
    [router, pathname],
  );
  const patch = React.useCallback(
    (p: Partial<AuditQuery>) => setQuery({ ...query, ...p }),
    [query, setQuery],
  );
  return { query, setQuery, patch };
}

const timeFmt = new Intl.DateTimeFormat("ja-JP", {
  timeZone: "Asia/Tokyo",
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});

/// `2026/10/22 09:21:44`（日本時間）。
export function formatTime(iso: string): string {
  return timeFmt.format(new Date(iso));
}

/// 引用チャンク ID（`rag.search` の metadata）。
export function citedChunks(metadata: unknown): string[] {
  if (!metadata || typeof metadata !== "object") return [];
  const v = (metadata as Record<string, unknown>).cited_chunk_ids;
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

/// チャットの回答のための検索・読み出しが持つ会話と回答（`metadata.thread_id` / `message_id`・#829）。
export function answerOf(metadata: unknown): { threadId?: string; messageId?: string } {
  if (!metadata || typeof metadata !== "object") return {};
  const m = metadata as Record<string, unknown>;
  return {
    threadId: typeof m.thread_id === "string" ? m.thread_id : undefined,
    messageId: typeof m.message_id === "string" ? m.message_id : undefined,
  };
}

/// 引用されたファイル ID（`rag.search` の metadata）。
export function citedFiles(metadata: unknown): string[] {
  if (!metadata || typeof metadata !== "object") return [];
  const v = (metadata as Record<string, unknown>).cited_file_ids;
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

/// 通常の画面で開ける対象のリンク（開けるかどうかは開いた先の ReBAC が決める・PIT-65）。
export function objectHref(objectType: string, objectId: string): string | null {
  if (objectType === "file") return `/file/${encodeURIComponent(objectId)}`;
  if (objectType === "thread") return `/c/${encodeURIComponent(objectId)}`;
  return null;
}
