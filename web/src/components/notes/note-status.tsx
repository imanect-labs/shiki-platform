"use client";

/// ノート・スライドの同期状態バッジ。状態はドットではなく文字（淡い塗りのバッジ）で示す。
/// 同期済み=夏の新緑 / 同期中=プライマリ / オフライン=破壊的（status-tokens 準拠）。
///
/// PC のバーでは**同期済みを視覚表示しない**（プレゼンスアバターが「つながっている」ことを
/// 表すため冗長・human 指示）。同期中/オフラインは注意を促すべきなので表示する。a11y/e2e 用に
/// テキストは sr-only で残す。スマホの本文先頭の状態行（`showSynced`）では同期済みも出す。

import { CloudOff, Loader2 } from "lucide-react";

import { EditorStatusBadge } from "@/components/shell/editor-loading";
import type { CollabStatus } from "@/lib/collab";

export function SyncPill({
  status,
  synced,
  showSynced = false,
  testId = "note-sync-status",
}: {
  status: CollabStatus;
  synced: boolean;
  /// 同期済みも見える形で出すか（スマホの状態行）。
  showSynced?: boolean;
  /// e2e 用の testId。同じ画面に 2 つ置くとき（PC バーとスマホの状態行）は片方を null にする。
  testId?: string | null;
}) {
  const state =
    status === "connected"
      ? synced
        ? "synced"
        : "syncing"
      : status === "connecting"
        ? "syncing"
        : "offline";

  const label = state === "synced" ? "同期済み" : state === "syncing" ? "同期中…" : "オフライン";

  if (state === "synced" && !showSynced) {
    return (
      <span className="sr-only" data-testid={testId ?? undefined} data-state={state}>
        {label}
      </span>
    );
  }

  return (
    <EditorStatusBadge
      tone={state === "synced" ? "success" : state === "syncing" ? "info" : "danger"}
      data-testid={testId ?? undefined}
      data-state={state}
      role="status"
    >
      {state === "syncing" ? (
        <Loader2 className="size-3 shrink-0 animate-spin" aria-hidden />
      ) : state === "offline" ? (
        <CloudOff className="size-3.5 shrink-0" aria-hidden />
      ) : null}
      {label}
    </EditorStatusBadge>
  );
}
