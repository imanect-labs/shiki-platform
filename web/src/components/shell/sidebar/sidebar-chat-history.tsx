"use client";

import * as React from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { Loader2, MessageSquareText } from "lucide-react";

import {
  deleteThread,
  groupThreadsByDate,
  renameThread,
  setThreadPinned,
  type Thread,
} from "@/lib/chat-api";
import { useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { useLocalStorage } from "@/hooks/use-local-storage";
import { usePinnedThreads, useThreadList } from "@/hooks/use-thread-list";
import { threadDisplay, threadHref, threadOriginKind } from "@/lib/thread-link";
import { toast } from "@/components/ui/use-toast";
import { SegmentedControl } from "@/components/ui/segmented-control";
import { ThreadShareDialog } from "@/components/chat/share-dialog";
import { ConfirmDialog } from "@/components/drive/dialogs";
import { ThreadRow, type ThreadRowActions } from "./sidebar-thread-row";

/// 履歴の種類フィルタ（通常のチャット / 文書の横で始めた会話）。
type HistoryFilter = "all" | "chat" | "doc";

const FILTER_KEY = "shiki:sidebar:history-filter";
const FILTER_OPTIONS = [
  { value: "all", label: "すべて" },
  { value: "chat", label: "チャット" },
  { value: "doc", label: "文書" },
];

function matchesFilter(thread: Thread, filter: HistoryFilter): boolean {
  if (filter === "all") return true;
  const doc = threadOriginKind(thread) !== null;
  return filter === "doc" ? doc : !doc;
}

/// 日付グループ・ピン留めの見出し。
function GroupLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="px-2.5 pb-1 pt-3 text-[11px] font-semibold uppercase tracking-[0.06em] text-sidebar-foreground/40 max-md:px-3 max-md:text-xs">
      {children}
    </div>
  );
}

/// 今の画面がこの会話か（文書由来は文書の画面かつ `?thread=` が一致したときだけ）。
function isActiveThread(thread: Thread, pathname: string, activeThreadId: string | null): boolean {
  const [path] = threadHref(thread).split("?");
  if (path === `/c/${thread.id}`) return pathname === path;
  return pathname === path && activeThreadId === thread.id;
}

/// サイドバー中段のチャット履歴。見出し「履歴」＋種類フィルタと、日付グループの一覧。
/// 末尾までスクロールすると続きを読み足す（先頭 30 件以外にも辿り着ける・#591）。
/// 種類フィルタは読み込み済みの行を絞る（該当が少なく sentinel が見えたままなら続きを読み足し続ける）。
///
/// `useSearchParams`（ノート由来行の active 判定に使用）を含むため Suspense 境界でラップする
/// （静的プリレンダされる (auth) ページ /apps 等で CSR bailout を起こさない・#282）。
export function SidebarChatHistory({
  collapsed,
  mobile = false,
  onNavigate,
}: {
  collapsed: boolean;
  /// スマホのドロワか（行を大きく・2 行目を出す・アクティブの塗りを静的にする）。
  mobile?: boolean;
  onNavigate?: () => void;
}) {
  if (collapsed) return <div className="flex-1" aria-hidden />;
  return (
    <React.Suspense fallback={<div className="min-h-0 flex-1" aria-hidden />}>
      <SidebarChatHistoryInner mobile={mobile} onNavigate={onNavigate} />
    </React.Suspense>
  );
}

function SidebarChatHistoryInner({
  mobile,
  onNavigate,
}: {
  mobile: boolean;
  onNavigate?: () => void;
}) {
  const { threads, loading, loadingMore, hasMore, loadMore, patchLocal } = useThreadList();
  const pinned = usePinnedThreads();
  // 種類フィルタで読み足した行が全部隠れると sentinel が見えたままになり、IntersectionObserver が
  // 再発火しない。取得完了ごとに件数を渡し、見えていれば続きを読む（!hasMore で止まる）。
  const sentinelRef = useInfiniteSentinel(
    loadMore,
    hasMore && !loading,
    loadingMore ? undefined : threads.length,
  );
  const router = useRouter();
  const pathname = usePathname();
  // 文書由来行は `/notes/{id}?thread=` ・ `/office/{id}?thread=` へ張るため、active 判定は thread
  // クエリまで見る（同一文書に複数会話がある 1:N で、全行が同時 active になるのを防ぐ）。
  const searchParams = useSearchParams();
  const activeThreadId = searchParams.get("thread");
  const [storedFilter, setFilter] = useLocalStorage<HistoryFilter>(FILTER_KEY, "all");
  const filter: HistoryFilter = FILTER_OPTIONS.some((o) => o.value === storedFilter)
    ? storedFilter
    : "all";
  const [shareTarget, setShareTarget] = React.useState<Thread | null>(null);
  const [deleteTarget, setDeleteTarget] = React.useState<Thread | null>(null);

  // ピン留めは先頭の欄にだけ出す（日付の欄からは除く）。判定はピン留め一覧を正にする:
  // 一覧の行は先頭ページより古いとピン留めの付け外しが反映されないため。
  const pinnedIds = React.useMemo(() => new Set(pinned.map((t) => t.id)), [pinned]);
  const pinnedShown = React.useMemo(
    () => pinned.filter((t) => matchesFilter(t, filter)),
    [pinned, filter],
  );
  const shown = React.useMemo(
    () => threads.filter((t) => !pinnedIds.has(t.id) && matchesFilter(t, filter)),
    [threads, pinnedIds, filter],
  );
  const groups = groupThreadsByDate(shown);

  const actions = React.useMemo<ThreadRowActions>(
    () => ({
      rename: async (thread, title) => {
        await renameThread(thread.id, title);
        patchLocal(thread.id, { title });
      },
      togglePin: (thread, pin) => {
        setThreadPinned(thread.id, pin)
          .then(() => patchLocal(thread.id, { pinnedAt: pin ? new Date().toISOString() : null }))
          .catch((e: unknown) =>
            toast({
              variant: "destructive",
              description: e instanceof Error ? e.message : "ピン留めを変更できませんでした。",
            }),
          );
      },
      share: setShareTarget,
      remove: setDeleteTarget,
    }),
    [patchLocal],
  );

  const confirmDelete = async () => {
    const thread = deleteTarget;
    if (!thread) return;
    await deleteThread(thread.id);
    patchLocal(thread.id, null);
    toast({ description: "チャットを削除しました。" });
    // 開いている会話を消したら、通常のチャットは新しいチャットへ、文書の会話は文書の画面へ戻る。
    if (isActiveThread(thread, pathname, activeThreadId)) {
      const [docPath] = threadHref(thread).split("?");
      router.push(threadOriginKind(thread) === null ? "/" : docPath);
    }
  };

  const renderRows = (rows: Thread[], isPinned: boolean) => (
    <ul className="flex flex-col gap-0.5">
      {rows.map((thread) => (
        <ThreadRow
          key={thread.id}
          thread={thread}
          active={isActiveThread(thread, pathname, activeThreadId)}
          mobile={mobile}
          pinned={isPinned}
          actions={actions}
          onNavigate={onNavigate}
        />
      ))}
    </ul>
  );

  let body: React.ReactNode;
  if (loading) {
    // 取得前はスケルトン（空状態のちらつきを防ぐ）。
    body = (
      <div className="flex flex-col gap-1.5 pt-1.5">
        {[68, 52, 60, 44, 56].map((w, i) => (
          <div key={i} className="flex h-8 items-center px-2.5 max-md:h-12 max-md:px-3">
            <div className="h-3.5 rounded bg-sidebar-foreground/10" style={{ width: `${w}%` }} />
          </div>
        ))}
      </div>
    );
  } else if (groups.length === 0 && pinnedShown.length === 0 && !hasMore) {
    body = (
      <div className="flex flex-col items-center gap-2 px-3 py-7 text-center">
        <MessageSquareText className="size-5 text-sidebar-foreground/35" aria-hidden />
        <p className="text-xs leading-relaxed text-sidebar-foreground/45 max-md:text-sm">
          {threads.length === 0 ? (
            <>
              まだチャットはありません。
              <br />
              「新しいチャット」から始めましょう。
            </>
          ) : filter === "doc" ? (
            "文書の横で始めた会話はありません。"
          ) : (
            "チャットはありません。"
          )}
        </p>
      </div>
    );
  } else {
    body = (
      <>
        {pinnedShown.length > 0 ? (
          <div className="mb-1">
            <GroupLabel>ピン留め</GroupLabel>
            {renderRows(pinnedShown, true)}
          </div>
        ) : null}
        {groups.map((group) => (
          <div key={group.label} className="mb-1">
            <GroupLabel>{group.label}</GroupLabel>
            {renderRows(group.threads, false)}
          </div>
        ))}
      </>
    );
  }

  // 見出しと種類フィルタは一覧と一緒にスクロールして上へ流す（固定すると一覧の縦幅を常に食う）。
  return (
    <div className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto overscroll-y-contain px-2.5 pb-2 max-md:px-2">
      <div className="flex items-center justify-between gap-2 px-2.5 pt-2 max-md:px-3 max-md:pt-3">
        <h2 className="text-[11px] font-semibold uppercase tracking-[0.07em] text-sidebar-foreground/45 max-md:text-xs">
          履歴
        </h2>
        <SegmentedControl
          size="sm"
          aria-label="履歴の種類"
          options={FILTER_OPTIONS}
          value={filter}
          onValueChange={(v) => setFilter(v as HistoryFilter)}
          className="border-sidebar-border bg-sidebar-accent/40"
        />
      </div>
      {body}
      {!loading && hasMore ? (
        <div ref={sentinelRef} className="flex h-8 items-center justify-center" aria-hidden>
          {loadingMore ? (
            <Loader2 className="size-3.5 animate-spin text-sidebar-foreground/40" />
          ) : null}
        </div>
      ) : null}
      <ThreadShareDialog
        open={shareTarget !== null}
        onOpenChange={(open) => {
          if (!open) setShareTarget(null);
        }}
        threadId={shareTarget?.id ?? null}
        shareUrl={
          shareTarget && typeof window !== "undefined"
            ? `${window.location.origin}${threadHref(shareTarget)}`
            : undefined
        }
      />
      <ConfirmDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
        title="チャットを削除しますか？"
        description={
          deleteTarget
            ? `「${threadDisplay(deleteTarget).title}」を削除します。共有している相手も開けなくなります。この操作は取り消せません。`
            : undefined
        }
        confirmLabel="削除"
        destructive
        onConfirm={confirmDelete}
      />
    </div>
  );
}
