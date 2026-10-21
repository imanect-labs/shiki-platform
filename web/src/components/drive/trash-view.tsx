"use client";

import * as React from "react";
import { Loader2, RotateCcw, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { isConflict, listTrash, restoreNode, type NodeResponse } from "@/lib/storage";
import { formatDateTime } from "@/lib/format";

import { LoadingRow, NodeIcon, nodeKindLabel } from "./primitives";

/// ゴミ箱ビュー。削除の根ノードを新しい順に並べ、各行から復元できる。
export function TrashView() {
  const fetchPage = React.useCallback((cursor?: string) => listTrash({ cursor, limit: 50 }), []);
  const list = useInfiniteList<NodeResponse>(fetchPage, []);
  const sentinelRef = useInfiniteSentinel(list.loadMore, list.hasMore && !list.loading);
  const [pendingId, setPendingId] = React.useState<string | null>(null);

  const restore = async (node: NodeResponse) => {
    setPendingId(node.id);
    try {
      await restoreNode(node);
      toast({ title: "復元しました", description: `「${node.name}」を元に戻しました` });
      list.reload();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "復元に失敗しました",
        description: isConflict(e)
          ? "元の場所に同じ名前のファイルまたはフォルダがあります。名前を変えてから復元してください。"
          : e instanceof Error
            ? e.message
            : String(e),
      });
    } finally {
      setPendingId(null);
    }
  };

  if (list.loading) return <LoadingRow />;
  if (list.error) return <p className="py-10 text-center text-sm text-destructive">{list.error}</p>;
  if (list.items.length === 0) {
    return (
      <EmptyState
        icon={Trash2}
        title="ゴミ箱は空です"
        description="削除したファイルやフォルダはここに表示され、復元できます。"
      />
    );
  }

  return (
    <div className="flex flex-col">
      {list.items.map((node) => (
        <div
          key={node.id}
          className="flex min-h-14 items-center gap-3 border-b border-border/50 px-3 py-2.5 transition-colors last:border-0 max-md:-mx-3 max-md:min-h-[64px] pointer-fine:hover:bg-accent"
        >
          <NodeIcon kind={node.kind} name={node.name} contentType={node.content_type} className="size-7 shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="line-clamp-2 text-sm font-medium [overflow-wrap:anywhere] md:truncate">{node.name}</p>
            <p className="truncate text-xs text-muted-foreground">
              {nodeKindLabel(node)}・更新 {formatDateTime(node.updated_at)}
            </p>
          </div>
          <Button
            type="button"
            size="sm"
            variant="outline"
            className="shrink-0 pointer-coarse:h-9"
            aria-label={`「${node.name}」を復元`}
            disabled={pendingId === node.id}
            onClick={() => void restore(node)}
          >
            {pendingId === node.id ? (
              <Loader2 className="size-4 animate-spin" aria-hidden />
            ) : (
              <RotateCcw className="size-4" aria-hidden />
            )}
            復元
          </Button>
        </div>
      ))}
      {list.hasMore ? <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div> : null}
    </div>
  );
}
