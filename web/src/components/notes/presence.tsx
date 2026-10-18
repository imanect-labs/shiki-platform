"use client";

/// 参加者プレゼンス表示（Task 11P.3）: awareness の user 状態をアバターチップで並べる。
/// PC のバーでは最大 6 人、スマホの状態行では先頭 1 人＋「+N」（`max`）に畳む。

import * as React from "react";

import type { CollabProvider } from "@/lib/collab";

interface Participant {
  clientId: number;
  name: string;
  color: string;
}

export function PresenceAvatars({
  provider,
  max = 6,
  testId = "note-presence",
}: {
  provider: CollabProvider;
  /// 並べる最大人数（超えた分は「+N」）。
  max?: number;
  /// e2e 用の testId。同じ画面に 2 つ置くとき（PC バーとスマホの状態行）は片方を null にする。
  testId?: string | null;
}) {
  const [participants, setParticipants] = React.useState<Participant[]>([]);

  React.useEffect(() => {
    const read = () => {
      const list: Participant[] = [];
      for (const [clientId, state] of provider.awareness.getStates()) {
        const user = (state as { user?: { name?: string; color?: string } }).user;
        if (!user?.name) continue;
        list.push({
          clientId,
          name: user.name,
          color: user.color ?? "var(--muted-foreground)",
        });
      }
      list.sort((a, b) => a.clientId - b.clientId);
      setParticipants(list);
    };
    read();
    provider.awareness.on("change", read);
    return () => provider.awareness.off("change", read);
  }, [provider]);

  if (participants.length === 0) return null;
  return (
    <div
      className="flex items-center -space-x-1.5"
      role="group"
      aria-label={`参加者 ${participants.length} 人`}
      data-testid={testId ?? undefined}
    >
      {participants.slice(0, max).map((p) => (
        <span
          key={p.clientId}
          title={p.name}
          role="img"
          aria-label={p.name}
          className="flex size-7 items-center justify-center rounded-full border-2 border-background text-xs font-semibold text-white"
          style={{ backgroundColor: p.color }}
        >
          {p.name.charAt(0).toUpperCase()}
        </span>
      ))}
      {participants.length > max && (
        <span className="flex h-7 min-w-7 items-center justify-center rounded-full border-2 border-background bg-muted px-1 text-xs font-medium tabular-nums text-muted-foreground">
          +{participants.length - max}
        </span>
      )}
    </div>
  );
}
