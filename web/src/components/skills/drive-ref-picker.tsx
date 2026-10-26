"use client";

/// スキルエディタのドライブ参照ピッカー（#919）。ID をコピーして貼る代わりに、ドライブを
/// 辿るか名前で探して、ファイル・フォルダを選ぶ。選んだ項目はパス（パンくず）を引いてから
/// 返す（本文のラベルにも、スキルを使う AI が読む文字列にもパスが載るように）。
///
/// `accept="folder"` は知識スコープ用（フォルダだけを選べる）。

import { ChevronLeft, ChevronRight, Loader2, Search, X } from "lucide-react";
import * as React from "react";

import { LoadingRow, NodeIcon } from "@/components/drive/primitives";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import type { DriveRef } from "@/lib/skills/drive-ref";
import { primeNodePath, useNodePath } from "@/lib/skills/node-path";
import { breadcrumb, listChildren, type NodeResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";

/// 検索結果の置き場所（親フォルダまでのパス）。引けるまでは何も出さない。
function ParentPath({ id }: { id: string }) {
  const path = useNodePath(id);
  if (!path) return null;
  const parent = path.split("/").slice(0, -1).join("/");
  return (
    <span className="mt-0.5 block truncate text-xs text-muted-foreground">
      {parent || "マイドライブ"}
    </span>
  );
}

/// 名前検索の入力を確定とみなすまでの待ち時間。
const SEARCH_DEBOUNCE_MS = 250;

export function DriveRefPicker({
  open,
  onOpenChange,
  onSelect,
  accept = "any",
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (ref: DriveRef) => void;
  accept?: "any" | "folder";
}) {
  // (id, name) のスタック。先頭はルート（マイドライブ）。
  const [stack, setStack] = React.useState<{ id?: string; name: string }[]>([
    { name: "マイドライブ" },
  ]);
  const [query, setQuery] = React.useState("");
  const [debounced, setDebounced] = React.useState("");
  const [resolving, setResolving] = React.useState<string | null>(null);
  const current = stack[stack.length - 1];
  const searching = debounced.trim().length > 0;
  const folderOnly = accept === "folder";

  React.useEffect(() => {
    if (!open) return;
    setStack([{ name: "マイドライブ" }]);
    setQuery("");
    setDebounced("");
    setResolving(null);
  }, [open]);

  React.useEffect(() => {
    const t = window.setTimeout(() => setDebounced(query), SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(t);
  }, [query]);

  const fetchPage = React.useCallback(
    (cursor?: string) => {
      if (!open) return Promise.resolve({ items: [] as NodeResponse[], next_cursor: null });
      return searching
        ? listChildren({ q: debounced.trim(), cursor, limit: 50 })
        : listChildren({ parentId: current.id, cursor, limit: 50 });
    },
    [open, current.id, searching, debounced],
  );
  const list = useInfiniteList<NodeResponse>(fetchPage, [open, current.id, debounced]);
  const sentinelRef = useInfiniteSentinel(list.loadMore, open && list.hasMore && !list.loading);
  const items = folderOnly ? list.items.filter((n) => n.kind === "folder") : list.items;

  // パスを引いてから返す（引けなければ名前だけ）。二重に押しても 1 回だけ選ぶ。
  const pick = async (node: { id: string; name: string; kind: string }) => {
    if (resolving) return;
    setResolving(node.id);
    const kind = node.kind === "folder" ? "folder" : "file";
    let label = node.name;
    try {
      const crumbs = await breadcrumb(node.id);
      const names = crumbs.map((c) => c.name.trim()).filter(Boolean);
      if (names.length > 0) label = names.join("/");
    } catch {
      /* パスが引けなくても名前で挿入する（表示は後で引き直す）。 */
    }
    primeNodePath(node.id, label);
    onSelect({ kind, id: node.id, label });
    setResolving(null);
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{folderOnly ? "フォルダを選択" : "ドライブから参照"}</DialogTitle>
          <DialogDescription>
            {folderOnly
              ? "知識スコープに含めるフォルダを選んでください（配下のファイルすべてが対象になります）。"
              : "スキルの指示文で参照するファイルかフォルダを選んでください。本文にはパスのラベルとして入ります。"}
          </DialogDescription>
        </DialogHeader>

        <div className="relative">
          <Search
            className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
            aria-hidden
          />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={folderOnly ? "フォルダ名で検索" : "ファイル名・フォルダ名で検索"}
            aria-label="名前で検索"
            className="pl-9 pr-9"
            autoComplete="off"
            data-testid="drive-ref-search"
          />
          {query ? (
            <button
              type="button"
              onClick={() => setQuery("")}
              aria-label="検索をクリア"
              className="absolute right-2 top-1/2 flex size-6 -translate-y-1/2 items-center justify-center rounded text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <X className="size-3.5" aria-hidden />
            </button>
          ) : null}
        </div>

        {!searching ? (
          <div className="flex min-h-9 flex-wrap items-center justify-between gap-2">
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
                variant="outline"
                disabled={resolving !== null}
                onClick={() => pick({ id: current.id!, name: current.name, kind: "folder" })}
              >
                「{current.name}」を選択
              </Button>
            ) : null}
          </div>
        ) : null}

        {/* 一覧の高さはシート（92dvh）の中に収まる範囲に抑え、外側と二重に伸びないようにする。 */}
        <div className="max-h-[min(55dvh,28rem)] min-h-[240px] min-w-0 overflow-y-auto overscroll-contain rounded-lg border border-border">
          {list.loading ? (
            <LoadingRow />
          ) : list.error ? (
            <div className="px-3 py-10 text-center text-sm">
              <p className="text-destructive">読み込みに失敗しました。</p>
              <button
                type="button"
                onClick={list.reload}
                className="mt-2 text-muted-foreground underline underline-offset-2 hover:text-foreground"
              >
                再試行
              </button>
            </div>
          ) : items.length === 0 && !list.hasMore ? (
            <p className="px-3 py-10 text-center text-sm text-muted-foreground">
              {searching
                ? "見つかりませんでした。"
                : folderOnly
                  ? "ここにはフォルダがありません。"
                  : "ここには何もありません。"}
            </p>
          ) : (
            <ul className="divide-y divide-border/60" data-testid="drive-ref-list">
              {items.map((n) => {
                const isFolder = n.kind === "folder";
                const busy = resolving === n.id;
                return (
                  <li
                    key={n.id}
                    className="flex items-center gap-2 pr-2 transition-colors pointer-fine:hover:bg-secondary"
                  >
                    <button
                      type="button"
                      onClick={() => {
                        // フォルダは辿る（検索結果のフォルダも辿れる）。ファイルはその場で選ぶ。
                        if (isFolder) {
                          setQuery("");
                          setDebounced("");
                          setStack((s) => [...(searching ? s.slice(0, 1) : s), { id: n.id, name: n.name }]);
                        } else void pick(n);
                      }}
                      disabled={resolving !== null && !isFolder}
                      className="flex min-w-0 flex-1 items-center gap-3.5 px-4 py-3 text-left text-[15px] disabled:opacity-60"
                    >
                      <NodeIcon
                        kind={n.kind}
                        name={n.name}
                        contentType={n.content_type}
                        className="size-7 shrink-0"
                      />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate">{n.name}</span>
                        {/* 検索結果は同名を見分けられるよう置き場所も出す。 */}
                        {searching ? <ParentPath id={n.id} /> : null}
                      </span>
                      {busy ? (
                        <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" aria-hidden />
                      ) : isFolder ? (
                        <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                      ) : null}
                    </button>
                    {isFolder ? (
                      <Button
                        type="button"
                        size="sm"
                        variant="outline"
                        disabled={resolving !== null}
                        onClick={() => void pick(n)}
                        className={cn("shrink-0", busy && "opacity-60")}
                        aria-label={`「${n.name}」を選択`}
                      >
                        選択
                      </Button>
                    ) : null}
                  </li>
                );
              })}
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
