"use client";

import * as React from "react";

/// いま画面に出している会話（[`Conversation`](@/components/chat/conversation) がマウント中のもの）。
///
/// サイドバー履歴の既読判定に使う。URL（`/c/{id}`・`?thread=`）では、文書の横のパネルで会話を
/// 切り替えた・パネルを閉じたときに実際の表示とずれるため、表示している本体から登録する。
/// 同じ会話を複数の場所で出すこともあるので数で持つ。
const counts = new Map<string, number>();
let snapshot: ReadonlySet<string> = new Set();
const listeners = new Set<() => void>();

function publish(): void {
  snapshot = new Set(counts.keys());
  for (const l of listeners) l();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

const EMPTY: ReadonlySet<string> = new Set();

/// この会話を表示している間、表示中として登録する。
export function useMarkThreadViewing(threadId: string | null): void {
  React.useEffect(() => {
    if (!threadId) return;
    counts.set(threadId, (counts.get(threadId) ?? 0) + 1);
    publish();
    return () => {
      const n = (counts.get(threadId) ?? 1) - 1;
      if (n > 0) counts.set(threadId, n);
      else counts.delete(threadId);
      publish();
    };
  }, [threadId]);
}

/// いま表示している会話の id（変わったときだけ別の参照になる）。
export function useViewingThreadIds(): ReadonlySet<string> {
  return React.useSyncExternalStore(
    subscribe,
    () => snapshot,
    () => EMPTY,
  );
}
