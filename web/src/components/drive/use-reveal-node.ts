"use client";

/// 作った・名前を変えたファイルやフォルダーを、一覧の最初のページの外でも、すぐに見せる（BUG-125）。
///
/// 一覧は名前の順に 50 件ずつ読むので、作ったものが最初のページに入らないと、作った直後に見えない（スクロールで続きを
/// 読むまで現れない）。[`revealAfterReload`] は一覧を読み直し、読み直しが終わってから、その行が出るまで続きを読み込む。
/// 見つかったら行へスクロールし、行の主ボタン（`data-node-primary`）にフォーカスする。続きを読み切っても無ければ
/// （別の場所へ保存された・すぐに消された）何もしない。読み込むページは [`MAX_PAGES`] までにする。

import * as React from "react";

import type { InfiniteList } from "@/hooks/use-infinite-list";
import type { NodeResponse } from "@/lib/storage";

/// 探すために続けて読み込むページの上限（1 ページ 50 件なので、先頭から 1,000 件まで）。
const MAX_PAGES = 20;

export function useRevealNode(list: InfiniteList<NodeResponse>): (id: string) => void {
  const [target, setTarget] = React.useState<string | null>(null);
  // 読み直しが始まるのを待っている（読み直す前の一覧で探さない）。
  const awaitingReload = React.useRef(false);
  const pages = React.useRef(0);
  const { items, loading, loadingMore, hasMore, loadMore, reload } = list;

  React.useEffect(() => {
    if (!target) return;
    if (awaitingReload.current) {
      if (loading) awaitingReload.current = false;
      return;
    }
    if (loading || loadingMore) return;
    if (items.some((n) => n.id === target)) {
      const row = document.querySelector<HTMLElement>(`[data-node-id="${CSS.escape(target)}"]`);
      row?.scrollIntoView({ block: "center" });
      row?.querySelector<HTMLElement>("[data-node-primary]")?.focus({ preventScroll: true });
      setTarget(null);
      return;
    }
    if (hasMore && pages.current < MAX_PAGES) {
      pages.current += 1;
      loadMore();
      return;
    }
    setTarget(null);
  }, [target, items, loading, loadingMore, hasMore, loadMore]);

  return React.useCallback(
    (id: string) => {
      pages.current = 0;
      awaitingReload.current = true;
      setTarget(id);
      reload();
    },
    [reload],
  );
}
