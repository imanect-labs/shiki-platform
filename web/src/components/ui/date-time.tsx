"use client";

import * as React from "react";

import { formatDateTime, relativeTime } from "@/lib/format";
import { usePreferences } from "@/lib/preferences";

/// 経過時間の表示を進める時計（表示している間だけ 1 分ごとに進める・ページで 1 つ）。
let now = 0;
let timer: ReturnType<typeof setInterval> | null = null;
const clockListeners = new Set<() => void>();

function subscribeClock(cb: () => void): () => void {
  if (clockListeners.size === 0) {
    now = Date.now();
    timer = setInterval(() => {
      now = Date.now();
      clockListeners.forEach((l) => l());
    }, 60_000);
  }
  clockListeners.add(cb);
  return () => {
    clockListeners.delete(cb);
    if (clockListeners.size === 0 && timer) {
      clearInterval(timer);
      timer = null;
    }
  };
}

const noSubscribe = () => () => {};

/// 一覧の「いつ」の表示（ユーザー設定の日時の表示・#952）。経過時間にしていれば「3 分前」の
/// ように出し、正確な日時はツールチップに残す。サーバ描画と初回は日時（hydration を揃える）。
///
/// `absolute` は日時の書式（画面ごとの既定。ワークフローの実行履歴は秒まで等）。
export function DateTime({
  iso,
  absolute = formatDateTime,
  className,
}: {
  iso: string | null | undefined;
  absolute?: (iso: string) => string;
  className?: string;
}) {
  const relative = usePreferences().prefs.time_format === "relative";
  const current = React.useSyncExternalStore(
    relative ? subscribeClock : noSubscribe,
    () => (relative ? now : 0),
    () => 0,
  );
  if (!iso) return <span className={className}>—</span>;
  const exact = absolute(iso);
  if (!relative || current === 0) {
    return (
      <time dateTime={iso} className={className}>
        {exact}
      </time>
    );
  }
  return (
    <time dateTime={iso} title={exact} className={className}>
      {relativeTime(iso, current, absolute)}
    </time>
  );
}
