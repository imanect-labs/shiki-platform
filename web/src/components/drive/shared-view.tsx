"use client";

import * as React from "react";
import { useRouter } from "next/navigation";
import { Download, Share2 } from "lucide-react";

import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { sharedWithMe, triggerDownload, type NodeResponse } from "@/lib/storage";
import { formatBytes, formatDateTime } from "@/lib/format";

import { LoadingRow, NodeIcon } from "./primitives";

/// 共有ビュー。自分に共有されたファイル/フォルダを新しい順に並べる。
/// フォルダは開く（ブラウズ）、ファイルはダウンロードできる。
export function SharedView() {
  const router = useRouter();
  const fetchPage = React.useCallback((cursor?: string) => sharedWithMe({ cursor, limit: 50 }), []);
  const list = useInfiniteList<NodeResponse>(fetchPage, []);
  const sentinelRef = useInfiniteSentinel(list.loadMore, list.hasMore && !list.loading);

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
          className="group flex items-center gap-3 border-b border-border/50 px-3 py-2.5 text-left transition-colors last:border-0 hover:bg-accent"
        >
          <NodeIcon kind={node.kind} name={node.name} contentType={node.content_type} className="size-6 shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="truncate text-sm font-medium">{node.name}</p>
            <p className="truncate text-xs text-muted-foreground">
              更新 {formatDateTime(node.updated_at)}
              {node.kind === "folder" ? "" : ` · ${formatBytes(node.size_bytes)}`}
            </p>
          </div>
          {node.kind === "file" ? (
            <Download className="size-4 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" aria-hidden />
          ) : null}
        </button>
      ))}
      {list.hasMore ? <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div> : null}
    </div>
  );
}
