"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { Download, Loader2, Share2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { sharedWithMeUpTo, triggerDownload, type NodeResponse } from "@/lib/storage";
import { formatBytes, formatDateTime } from "@/lib/format";

import { LoadingRow, NodeIcon, nodeKindLabel } from "./primitives";

/// 共有ビュー。自分に共有されたファイル/フォルダを新しい順に並べる。
/// フォルダは開く（ブラウズ）、ファイルはダウンロードできる。
///
/// 共有されたものが多い人では、サーバーが 1 ページを短いか 0 件のまま続きの位置だけ付けて返すことがある（BUG-162）。
/// 1 回の読み込みで続きを数回たどり、それでも見つからないあいだは空の状態に見せずに「さらに読み込む」を出す。
export function SharedView() {
  const router = useRouter();
  const fetchPage = React.useCallback((cursor?: string) => sharedWithMeUpTo({ cursor, want: 50 }), []);
  const list = useInfiniteList<NodeResponse>(fetchPage, []);
  // 0 件のページでも、読み終えたときに sentinel が見えていれば続きを読む（progress は取得の回数）。sentinel は
  // 1 件目が見つかってから置く（それまでは「さらに読み込む」）ので、見つかったときに監視を始め直す。
  const sentinelRef = useInfiniteSentinel(
    list.loadMore,
    list.hasMore && !list.loading && list.items.length > 0,
    list.fetches,
  );

  const open = (node: NodeResponse) => {
    if (node.kind === "folder") {
      router.push(`/drive?folder=${node.id}`, { scroll: false });
    } else {
      triggerDownload(node.id).catch((e) =>
        toast({
          variant: "destructive",
          title: "ダウンロードに失敗しました",
          description: e instanceof Error ? e.message : String(e),
        }),
      );
    }
  };

  if (list.loading) return <LoadingRow />;
  if (list.error) return <p className="py-10 text-center text-sm text-destructive">{list.error}</p>;
  if (list.items.length === 0 && list.hasMore) {
    return (
      <div className="flex flex-col items-center gap-3 py-10 text-center" data-testid="drive-shared-pending">
        <p className="text-sm text-muted-foreground">まだ見つかっていません。「さらに読み込む」で続きを探します。</p>
        <Button variant="outline" size="sm" onClick={list.loadMore} disabled={list.loadingMore} data-testid="drive-shared-more">
          {list.loadingMore ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
          さらに読み込む
        </Button>
      </div>
    );
  }
  if (list.items.length === 0) {
    return (
      <EmptyState
        icon={Share2}
        title="共有されたアイテムはありません"
        description="他のユーザーから共有されると、ここに表示されます。"
      />
    );
  }

  return (
    <div className="flex flex-col">
      {list.items.map((node) => (
        <button
          key={node.id}
          type="button"
          onClick={() => open(node)}
          className="group flex min-h-14 items-center gap-3 border-b border-border/50 px-3 py-2.5 text-left outline-none transition-colors last:border-0 hover:bg-accent focus-visible:bg-accent active:bg-accent max-md:-mx-3 max-md:min-h-[64px]"
        >
          <NodeIcon kind={node.kind} name={node.name} contentType={node.content_type} className="size-7 shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="line-clamp-2 text-sm font-medium [overflow-wrap:anywhere] md:truncate">{node.name}</p>
            <p className="truncate text-xs text-muted-foreground">
              {nodeKindLabel(node)}・更新 {formatDateTime(node.updated_at)}
              {node.kind === "folder" ? "" : `・${formatBytes(node.size_bytes)}`}
            </p>
          </div>
          {/* 押すと何が起きるか（ファイルはダウンロード）。マウスはホバーで、タッチは常に見せる。 */}
          {node.kind === "file" ? (
            <Download
              className="size-4 shrink-0 text-muted-foreground transition-opacity pointer-fine:opacity-0 pointer-fine:group-hover:opacity-100 pointer-fine:group-focus-visible:opacity-100"
              aria-label="ダウンロード"
            />
          ) : null}
        </button>
      ))}
      {list.hasMore ? <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div> : null}
    </div>
  );
}
