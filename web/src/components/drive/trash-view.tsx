"use client";

import * as React from "react";
import { AlertTriangle, Loader2, RotateCcw, Trash2, XCircle } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/use-toast";
import { useInfiniteList, useInfiniteSentinel } from "@/hooks/use-infinite-list";
import {
  emptyTrash,
  isConflict,
  listTrash,
  purgeTrashItem,
  PurgeRefusedError,
  restoreNode,
  StorageApiError,
  type TrashItem,
} from "@/lib/storage";
import { DateTime } from "@/components/ui/date-time";
import { formatDateTime } from "@/lib/format";

import { LoadingRow, NodeIcon, nodeKindLabel } from "./primitives";

/// 完全削除に失敗したときの、利用者に見せる文言（GAP-108b）。
function purgeErrorMessage(e: unknown): string {
  if (e instanceof PurgeRefusedError) {
    switch (e.code) {
      case "live_descendants":
        return "このフォルダの中に、削除した後に作られたファイルがあります。フォルダを元に戻して中を確かめてください。";
      case "inside_deleted_folder":
        return "このファイルはフォルダごとゴミ箱にあります。フォルダを完全に削除してください。";
      case "not_in_trash":
        return "ゴミ箱にありません。ほかの人が元に戻した可能性があります。";
      default:
        return e.message;
    }
  }
  if (e instanceof StorageApiError) {
    if (e.status === 403) return "持ち主か管理者だけが完全に削除できます。";
    if (e.status === 404) return "見つかりません。すでに完全に削除された可能性があります。";
  }
  return e instanceof Error ? e.message : String(e);
}

/// 「空にする」の結果の文言。
function emptyResultMessage(purged: number, skipped: number): string {
  if (purged === 0 && skipped === 0) return "完全に削除できるものはありませんでした。";
  const head = `${purged} 件を削除しました。`;
  return skipped > 0 ? `${head}${skipped} 件は持ち主か管理者だけが削除できるため残しました。` : head;
}

/// ゴミ箱ビュー。削除の根ノードを新しい順に並べ、各行から復元・完全に削除できる（完全に削除は持ち主か管理者だけ）。
/// 上の「ゴミ箱を空にする」は、自分のゴミ箱に出ているもののうち、自分が完全に削除できるものをまとめて消す。
export function TrashView() {
  // テナントのゴミ箱の保持日数（自動で完全に削除されるまでの日数・無ければ null）。
  const [retentionDays, setRetentionDays] = React.useState<number | null>(null);
  const fetchPage = React.useCallback(
    (cursor?: string) =>
      listTrash({ cursor, limit: 50 }).then((page) => {
        setRetentionDays(page.retention_days ?? null);
        return page;
      }),
    [],
  );
  const list = useInfiniteList<TrashItem>(fetchPage, []);
  const sentinelRef = useInfiniteSentinel(list.loadMore, list.hasMore && !list.loading);
  const [pendingId, setPendingId] = React.useState<string | null>(null);
  // 完全に削除した行（読み直さずに一覧から外す）。
  const [gone, setGone] = React.useState<ReadonlySet<string>>(() => new Set());
  const [purging, setPurging] = React.useState<TrashItem | null>(null);
  const [emptyOpen, setEmptyOpen] = React.useState(false);
  // 「空にする」の結果（一覧の上に残す）。
  const [result, setResult] = React.useState<string | null>(null);
  // ダイアログを開いたボタン（閉じたら戻す）。行が消えていたら「ゴミ箱を空にする」か一覧へ戻す。
  const opener = React.useRef<HTMLElement | null>(null);
  const fallbackRef = React.useRef<HTMLDivElement>(null);

  const items = list.items.filter((n) => !gone.has(n.id));

  const rememberOpener = () => {
    const el = document.activeElement;
    opener.current = el instanceof HTMLElement && el !== document.body ? el : null;
  };
  const restoreFocus = (event: Event) => {
    event.preventDefault();
    const el = opener.current;
    opener.current = null;
    if (el?.isConnected) {
      el.focus();
      return;
    }
    const emptyButton = fallbackRef.current?.querySelector<HTMLButtonElement>("button:not(:disabled)");
    (emptyButton ?? fallbackRef.current)?.focus();
  };

  const restore = async (node: TrashItem) => {
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

  const onPurged = (node: TrashItem) => {
    setGone((prev) => new Set(prev).add(node.id));
    setResult(null);
    toast({ title: "完全に削除しました", description: `「${node.name}」を完全に削除しました` });
  };

  // 「空にする」の後に読み直した一覧が空なら、消えたボタンの代わりに操作の欄へフォーカスを置く。
  const focusAfterEmpty = React.useRef(false);
  const onEmptied = (purged: number, skipped: number) => {
    setResult(emptyResultMessage(purged, skipped));
    setGone(new Set());
    focusAfterEmpty.current = true;
    list.reload();
  };

  // 「空にする」の後の読み直しの間もボタンを残す（閉じたダイアログからフォーカスを戻す先・一覧のちらつきを避ける）。
  const hadItems = React.useRef(false);
  if (!list.loading) hadItems.current = items.length > 0 || list.hasMore;
  const hasItems = list.loading ? hadItems.current : items.length > 0 || list.hasMore;
  React.useEffect(() => {
    if (list.loading || !focusAfterEmpty.current) return;
    focusAfterEmpty.current = false;
    const active = document.activeElement;
    if (!hasItems && (active === null || active === document.body)) fallbackRef.current?.focus();
  }, [list.loading, hasItems]);

  let body: React.ReactNode;
  if (list.loading) {
    body = <LoadingRow />;
  } else if (list.error) {
    body = (
      <p role="alert" className="py-10 text-center text-sm text-destructive">
        {list.error}
      </p>
    );
  } else if (items.length === 0 && !list.hasMore) {
    body = (
      <EmptyState
        icon={Trash2}
        title="ゴミ箱は空です"
        description="削除したファイルやフォルダはここに表示され、復元できます。"
      />
    );
  } else {
    body = (
      <ul className="flex flex-col" aria-label="ゴミ箱の中身">
        {items.map((node) => (
          <li
            key={node.id}
            className="flex min-h-14 items-center gap-3 border-b border-border/50 px-3 py-2.5 transition-colors last:border-0 max-md:-mx-3 max-md:min-h-[64px] pointer-fine:hover:bg-accent"
          >
            <NodeIcon kind={node.kind} name={node.name} contentType={node.content_type} className="size-7 shrink-0" />
            <div className="min-w-0 flex-1">
              <p className="line-clamp-2 text-sm font-medium [overflow-wrap:anywhere] md:truncate">{node.name}</p>
              <p className="text-xs text-muted-foreground max-md:line-clamp-2 md:truncate">
                {nodeKindLabel(node)}・削除 <DateTime iso={node.deleted_at} />
                {node.purge_after ? (
                  <>
                    ・<time dateTime={node.purge_after}>{formatDateTime(node.purge_after)}</time>{" "}
                    ごろ自動で完全に削除
                  </>
                ) : null}
              </p>
            </div>
            <div className="flex shrink-0 items-center gap-1.5">
              <Button
                type="button"
                size="sm"
                variant="outline"
                className="pointer-coarse:h-9"
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
              {node.can_purge ? (
                <Button
                  type="button"
                  size="sm"
                  variant="ghost"
                  className="text-destructive hover:bg-destructive/10 hover:text-destructive pointer-coarse:h-9"
                  aria-label={`「${node.name}」を完全に削除`}
                  disabled={pendingId === node.id}
                  onClick={() => {
                    rememberOpener();
                    setPurging(node);
                  }}
                >
                  <XCircle className="size-4" aria-hidden />
                  <span className="max-sm:sr-only">完全に削除</span>
                </Button>
              ) : null}
            </div>
          </li>
        ))}
      </ul>
    );
  }

  return (
    <div className="flex flex-col gap-3">
      {retentionDays !== null ? (
        <p className="text-sm text-muted-foreground">
          ゴミ箱に入れてから {retentionDays} 日たつと、自動で完全に削除されます。
        </p>
      ) : null}
      <div
        ref={fallbackRef}
        tabIndex={-1}
        className="flex flex-wrap items-center justify-between gap-2 outline-none"
        aria-label="ゴミ箱の操作"
        role="group"
      >
        <p role="status" className="min-w-0 flex-1 text-sm text-muted-foreground">
          {result}
        </p>
        {hasItems ? (
          <Button
            type="button"
            size="sm"
            variant="outline"
            className="text-destructive hover:text-destructive pointer-coarse:h-9"
            onClick={() => {
              rememberOpener();
              setResult(null);
              setEmptyOpen(true);
            }}
          >
            <Trash2 className="size-4" aria-hidden />
            ゴミ箱を空にする
          </Button>
        ) : null}
      </div>
      {body}
      {list.hasMore && !list.loading ? (
        <div ref={sentinelRef}>{list.loadingMore ? <LoadingRow /> : null}</div>
      ) : null}
      <PurgeDialog
        item={purging}
        onOpenChange={(open) => !open && setPurging(null)}
        onCloseAutoFocus={restoreFocus}
        onPurged={onPurged}
        onGone={(node) => setGone((prev) => new Set(prev).add(node.id))}
        onStale={() => list.reload()}
      />
      <EmptyTrashDialog
        open={emptyOpen}
        onOpenChange={setEmptyOpen}
        onCloseAutoFocus={restoreFocus}
        onDone={onEmptied}
      />
    </div>
  );
}

/// 1 件を完全に削除する確かめのダイアログ。元に戻せないこと、フォルダなら配下の数と、ほかの人が作ったファイルも
/// 消えることを出す（D-201）。
function PurgeDialog({
  item,
  onOpenChange,
  onCloseAutoFocus,
  onPurged,
  onGone,
  onStale,
}: {
  item: TrashItem | null;
  onOpenChange: (open: boolean) => void;
  onCloseAutoFocus: (event: Event) => void;
  onPurged: (item: TrashItem) => void;
  /// もう無かった（404）。一覧から外す。
  onGone: (item: TrashItem) => void;
  /// 一覧が古い（ゴミ箱に無い・フォルダごとゴミ箱にある）。閉じたら読み直す。
  onStale: () => void;
}) {
  const [busy, setBusy] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const stale = React.useRef(false);
  // 閉じる間（アニメーション）も中身を出しておく。
  const [shown, setShown] = React.useState<TrashItem | null>(item);
  React.useEffect(() => {
    if (item) {
      setShown(item);
      setBusy(false);
      setError(null);
      stale.current = false;
    }
  }, [item]);

  const close = (open: boolean) => {
    if (busy) return;
    if (!open && stale.current) onStale();
    onOpenChange(open);
  };

  const confirm = async () => {
    if (!shown) return;
    setBusy(true);
    setError(null);
    try {
      await purgeTrashItem(shown.id);
      onPurged(shown);
      setBusy(false);
      onOpenChange(false);
    } catch (e) {
      setBusy(false);
      setError(purgeErrorMessage(e));
      if (e instanceof StorageApiError && e.status === 404) onGone(shown);
      if (e instanceof PurgeRefusedError && e.code !== "live_descendants") stale.current = true;
    }
  };

  const isFolder = shown?.kind === "folder";
  const files = shown?.file_count ?? 0;
  const others = shown?.others_file_count ?? 0;
  return (
    <Dialog open={item !== null} onOpenChange={close}>
      <DialogContent className="max-w-md" onCloseAutoFocus={onCloseAutoFocus}>
        <DialogHeader>
          <DialogTitle>完全に削除しますか？</DialogTitle>
          <DialogDescription>
            {shown
              ? isFolder
                ? `フォルダ「${shown.name}」と、中のファイル ${files} 件を完全に削除します。元に戻せません。`
                : `「${shown.name}」を完全に削除します。元に戻せません。`
              : null}
          </DialogDescription>
        </DialogHeader>
        <ul className="flex flex-col gap-1.5 text-sm text-muted-foreground">
          {isFolder && others > 0 ? (
            <li className="flex gap-2 text-foreground">
              <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" aria-hidden />
              <span>このうち {others} 件は、ほかの人が作ったファイルです。作った人も元に戻せなくなります。</span>
            </li>
          ) : null}
          <li>ワークフローやチャットの引用など、このファイルを指しているところからは開けなくなります。</li>
        </ul>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => close(false)} disabled={busy}>
            キャンセル
          </Button>
          <Button type="button" variant="destructive" onClick={() => void confirm()} disabled={busy}>
            {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            完全に削除
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// 「ゴミ箱を空にする」の確かめと進み具合。`next_cursor` の間は繰り返す。
function EmptyTrashDialog({
  open,
  onOpenChange,
  onCloseAutoFocus,
  onDone,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCloseAutoFocus: (event: Event) => void;
  onDone: (purged: number, skipped: number) => void;
}) {
  const [busy, setBusy] = React.useState(false);
  const [purged, setPurged] = React.useState(0);
  const [error, setError] = React.useState<string | null>(null);
  // 途中で失敗したときも、消した分は一覧に反映する（閉じたら読み直す）。
  const partial = React.useRef<{ purged: number; skipped: number } | null>(null);
  React.useEffect(() => {
    if (open) {
      setBusy(false);
      setPurged(0);
      setError(null);
      partial.current = null;
    }
  }, [open]);

  const close = (next: boolean) => {
    if (busy) return;
    if (!next && partial.current) onDone(partial.current.purged, partial.current.skipped);
    onOpenChange(next);
  };

  const run = async () => {
    setBusy(true);
    setError(null);
    let total = 0;
    let kept = 0;
    let cursor: string | undefined;
    try {
      for (;;) {
        const page = await emptyTrash(cursor);
        total += page.purged;
        kept += page.skipped;
        partial.current = { purged: total, skipped: kept };
        setPurged(total);
        if (!page.next_cursor) break;
        cursor = page.next_cursor;
      }
      partial.current = null;
      setBusy(false);
      onDone(total, kept);
      onOpenChange(false);
    } catch (e) {
      setBusy(false);
      const done = total > 0 ? `${total} 件は削除しました。` : "";
      setError(`${done}途中で失敗しました: ${e instanceof Error ? e.message : String(e)}`);
    }
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-w-md" onCloseAutoFocus={onCloseAutoFocus}>
        <DialogHeader>
          <DialogTitle>ゴミ箱を空にしますか？</DialogTitle>
          <DialogDescription>
            ゴミ箱にあるもののうち、あなたが完全に削除できるものをすべて削除します。元に戻せません。
          </DialogDescription>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">持ち主か管理者だけが削除できるものは残ります。</p>
        {busy ? (
          <p role="status" className="flex items-center gap-2 text-sm">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            削除しています…（{purged} 件）
          </p>
        ) : null}
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button type="button" variant="ghost" onClick={() => close(false)} disabled={busy}>
            キャンセル
          </Button>
          <Button type="button" variant="destructive" onClick={() => void run()} disabled={busy}>
            {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
            空にする
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
