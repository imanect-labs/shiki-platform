"use client";

/// 参加者プレゼンス表示（Task 11P.3）: awareness の user 状態をアバターチップで並べる。

import * as React from "react";

import type { CollabProvider } from "@/lib/collab";

interface Participant {
  clientId: number;
  name: string;
  color: string;
}

export function PresenceAvatars({ provider }: { provider: CollabProvider }) {
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
    <div className="flex items-center -space-x-1.5" aria-label="参加者" data-testid="note-presence">
      {participants.slice(0, 6).map((p) => (
        <span
          key={p.clientId}
          title={p.name}
          className="flex size-7 items-center justify-center rounded-full border-2 border-background text-xs font-semibold text-white"
          style={{ backgroundColor: p.color }}
        >
          {p.name.charAt(0).toUpperCase()}
        </span>
      ))}
      {participants.length > 6 && (
        <span className="flex size-7 items-center justify-center rounded-full border-2 border-background bg-muted text-xs font-medium text-muted-foreground">
          +{participants.length - 6}
        </span>
      )}
    </div>
  );
}
