"use client";

/// ノートの同期状態インジケータ。色枠のピルは安っぽく見えるため、控えめな
/// 「小さなドット＋muted テキスト」の上品な表示にする。色はドットにだけ乗せる。
/// 同期済み=夏の新緑 / 同期中=プライマリ＋パルス / オフライン=破壊的（status-tokens 準拠）。
///
/// **同期済み状態は視覚表示しない**（プレゼンスアバターが「つながっている」ことを表すため
/// 冗長・human 指示）。同期中/オフラインは注意を促すべきなので表示する。a11y/e2e 用に
/// テキストは sr-only で残す（スクリーンリーダーは状態を読み上げ、テキスト検証も通る）。

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
  const dotColor =
    state === "syncing" ? "var(--primary)" : "var(--destructive)";

  // 同期済みは視覚的に隠す（アバターで足りる）。状態テキストは sr-only で保持する。
  if (state === "synced") {
    return (
      <span className="sr-only" data-testid="note-sync-status" data-state={state}>
        {label}
      </span>
    );
  }

  return (
    <span
      className="inline-flex select-none items-center gap-1.5 text-xs text-muted-foreground"
      data-testid="note-sync-status"
      data-state={state}
      title={label}
    >
      {/* 色はドットにだけ。同期中は控えめなパルス、オフラインはリング付きで注意を促す。 */}
      <span className="relative flex size-2 items-center justify-center" aria-hidden>
        {state === "syncing" ? (
          <span
            className="absolute inline-flex size-2 animate-ping rounded-full opacity-60"
            style={{ backgroundColor: dotColor }}
          />
        ) : null}
        <span
          className={cn("relative size-1.5 rounded-full", state === "offline" && "ring-2 ring-destructive/30")}
          style={{ backgroundColor: dotColor }}
        />
      </span>
      {label}
    </span>
  );
}
