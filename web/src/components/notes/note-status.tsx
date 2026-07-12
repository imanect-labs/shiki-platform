"use client";

/// ノートの同期状態ピル。muted テキストではなく、状態が一目で分かる小さな pill にする。
/// 同期済み=夏の新緑 / 同期中=プライマリ＋スピナ / オフライン=破壊的（status-tokens 準拠）。

import { Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";
import type { CollabStatus } from "@/lib/collab";

export function SyncPill({ status, synced }: { status: CollabStatus; synced: boolean }) {
  const state =
    status === "connected"
      ? synced
        ? "synced"
        : "syncing"
      : status === "connecting"
        ? "syncing"
        : "offline";

  const label = state === "synced" ? "同期済み" : state === "syncing" ? "同期中…" : "オフライン";
  // 色はトークン経由（ライト/ダーク自動反転）。
  const color =
    state === "synced"
      ? "var(--season-summer)"
      : state === "syncing"
        ? "var(--primary)"
        : "var(--destructive)";

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs font-medium",
        "transition-colors duration-[var(--duration-normal)]",
      )}
      style={{ color, borderColor: "color-mix(in oklab, currentColor 30%, transparent)" }}
      data-testid="note-sync-status"
      data-state={state}
    >
      {state === "syncing" ? (
        <Loader2 className="size-3 animate-spin" aria-hidden />
      ) : (
        <span className="size-1.5 rounded-full bg-current" aria-hidden />
      )}
      {label}
    </span>
  );
}
