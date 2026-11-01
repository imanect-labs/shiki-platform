"use client";

/// 一覧の末尾の続き（ドライブのフォルダーの中身・名前の検索・ドライブから選ぶダイアログ）。
///
/// 見えないものが多い場所（ドライブのトップ・名前の検索）では、サーバーが 1 ページを短いか 0 件のまま続きの位置だけ
/// 付けて返すことがある（BUG-187）。直前の読み込みで何も見つからなかったら、自動では読まずに「さらに読み込む」を出す
/// （見えないものばかりの組織で、操作なしに背後で読み続けない）。見つかっているあいだは、下までスクロールしたときに
/// 自動で読み足す（無限スクロールの目印）。文言は「共有済み」の続き（shared-view.tsx）とそろえる。

import * as React from "react";
import { Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import type { InfiniteList } from "@/hooks/use-infinite-list";

import { LoadingRow } from "./primitives";

/// 続きはあるが、直前の読み込みで何も見つからなかった（続きは押して探す）。
export function isStalled(list: Pick<InfiniteList<unknown>, "hasMore" | "lastAdded" | "loading">): boolean {
  return !list.loading && list.hasMore && list.lastAdded === 0;
}

/// 無限スクロールの目印を有効にしてよいか（続きがあり、読み込み中でも、止まってもいない）。
export function sentinelEnabled(list: Pick<InfiniteList<unknown>, "hasMore" | "lastAdded" | "loading">): boolean {
  return list.hasMore && !list.loading && !isStalled(list);
}

export function MoreRows<T>({
  list,
  sentinelRef,
  testId = "drive-more",
}: {
  list: InfiniteList<T>;
  sentinelRef: React.RefObject<HTMLDivElement | null>;
  /// 「さらに読み込む」のボタンの testid（囲みは `${testId}-stalled`）。
  testId?: string;
}) {
  if (!list.hasMore) return null;
  if (isStalled(list)) {
    return (
      <div className="flex flex-col items-center gap-2 px-3 py-6 text-center" data-testid={`${testId}-stalled`}>
        <p className="text-balance text-sm text-muted-foreground [word-break:auto-phrase]">
          {list.items.length === 0
            ? "まだ見つかっていません。「さらに読み込む」で続きを探します。"
            : "続きはまだ見つかっていません。「さらに読み込む」で続きを探します。"}
        </p>
        <Button variant="outline" size="sm" onClick={list.loadMore} disabled={list.loadingMore} data-testid={testId}>
          {list.loadingMore ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
          さらに読み込む
        </Button>
      </div>
    );
  }
  return <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div>;
}
