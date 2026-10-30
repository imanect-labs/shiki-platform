"use client";

/// ドライブのフォルダを選ぶダイアログ（Task 6.11・Phase 6 UX）。
///
/// 2 用途:
/// - `purpose="scope"`（既定・skill の知識スコープ）: フォルダを 1 つ選ぶ（mode は "existing" 固定）。
/// - `purpose="workspace"`（エージェントの作業場所）: 各フォルダを「このフォルダを使う」（existing）で
///   選ぶか、現在地に「ここに新規作成」（new_under）できる。
/// - `purpose="destination"`（下書きの保存先）: 選び方は scope と同じで、説明文だけ保存先向けにする。
/// - `purpose="search"`（ホームの社内検索で探すフォルダ・#965）: 選び方は scope と同じで、説明文だけ検索向けにする。

import * as React from "react";
import { ChevronLeft, FolderPlus } from "lucide-react";

import { listChildren, type NodeResponse } from "@/lib/storage";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { NodeIcon, LoadingRow } from "@/components/drive/primitives";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";

export type FolderChoice = { id: string; name: string; mode: "existing" | "new_under" };

export function FolderPicker({
  open,
  onOpenChange,
  onSelect,
  purpose = "scope",
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (folder: FolderChoice) => void;
  purpose?: "scope" | "workspace" | "destination" | "search";
}) {
  // (id, name) のスタック。先頭はルート（マイドライブ）。
  const [stack, setStack] = React.useState<{ id?: string; name: string }[]>([
    { name: "マイドライブ" },
  ]);
  const current = stack[stack.length - 1];
  const isWorkspace = purpose === "workspace";

  const fetchPage = React.useCallback(
    (cursor?: string) => {
      if (!open) return Promise.resolve({ items: [] as NodeResponse[], next_cursor: null });
      return listChildren({ parentId: current.id, cursor, limit: 50 });
    },
    [open, current.id],
  );
  const list = useInfiniteList<NodeResponse>(fetchPage, [open, current.id]);
  // 一覧はフォルダだけに絞って出すため、読み足した行がファイルばかりだと sentinel が見えたままになり
  // IntersectionObserver が再発火しない。取得完了ごとに件数を渡し、見えていれば続きを読む。
  const sentinelRef = useInfiniteSentinel(
    list.loadMore,
    open && list.hasMore && !list.loading,
    list.loadingMore ? undefined : list.items.length,
  );

  React.useEffect(() => {
    if (open) setStack([{ name: "マイドライブ" }]);
  }, [open]);

  const pick = (folder: { id: string; name: string }, mode: "existing" | "new_under") => {
    onSelect({ ...folder, mode });
    onOpenChange(false);
  };

  const folders = list.items.filter((n) => n.kind === "folder");

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>フォルダを選択</DialogTitle>
          <DialogDescription>
            {isWorkspace
              ? "エージェントが作業するワークスペースの場所を選んでください（このフォルダを使うか、配下に新しく作れます）。"
              : purpose === "destination"
                ? "保存先のフォルダを選んでください（フォルダを開くと、その中のフォルダも選べます）。"
                : purpose === "search"
                  ? "社内文書を探すフォルダを選んでください（配下の文書すべてから探します）。"
                  : "知識スコープに含めるフォルダを選んでください（配下のファイルすべてが対象になります）。"}
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-wrap items-center justify-between gap-2">
          {stack.length > 1 ? (
            <button
              type="button"
              onClick={() => setStack((s) => s.slice(0, -1))}
              className="-ml-2 flex h-9 min-w-0 items-center gap-1 rounded-md px-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground pointer-coarse:h-11"
            >
              <ChevronLeft className="size-4 shrink-0" aria-hidden />
              {/* どこへ戻るかを見せる（「戻る」だけだと深い階層で迷う）。 */}
              <span className="truncate">{stack[stack.length - 2].name}へ戻る</span>
            </button>
          ) : (
            <span className="text-sm text-muted-foreground">{current.name}</span>
          )}
          {current.id ? (
            <Button
              type="button"
              size="sm"
              onClick={() =>
                pick(
                  { id: current.id!, name: current.name },
                  isWorkspace ? "new_under" : "existing",
                )
              }
            >
              <FolderPlus className="size-4" aria-hidden />
              {isWorkspace ? `「${current.name}」の配下に作成` : `「${current.name}」を選択`}
            </Button>
          ) : null}
        </div>

        {/* 一覧の高さはシート（92dvh）の中に収まる範囲に抑え、外側と二重に伸びないようにする。 */}
        <div className="max-h-[min(55dvh,28rem)] min-h-[200px] min-w-0 overflow-y-auto overscroll-contain rounded-lg border border-border">
          {list.loading ? (
            <LoadingRow />
          ) : folders.length === 0 && !list.hasMore ? (
            <p className="px-3 py-10 text-center text-sm text-muted-foreground">
              ここにはフォルダがありません。
            </p>
          ) : (
            <ul className="divide-y divide-border/60">
              {folders.map((n) => (
                <li key={n.id} className="flex items-center gap-2 pr-2 transition-colors pointer-fine:hover:bg-secondary">
                  <button
                    type="button"
                    onClick={() => setStack((s) => [...s, { id: n.id, name: n.name }])}
                    className="flex min-w-0 flex-1 items-center gap-3.5 px-4 py-3 text-left text-[15px]"
                  >
                    <NodeIcon kind={n.kind} name={n.name} contentType={n.content_type} className="size-7 shrink-0" />
                    <span className="truncate">{n.name}</span>
                  </button>
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    onClick={() => pick({ id: n.id, name: n.name }, "existing")}
                  >
                    {isWorkspace ? "このフォルダを使う" : "選択"}
                  </Button>
                </li>
              ))}
              {list.hasMore ? (
                <li>
                  <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div>
                </li>
              ) : null}
            </ul>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
