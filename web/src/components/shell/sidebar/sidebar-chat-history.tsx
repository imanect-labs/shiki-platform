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

/// 今いる会話の行を最初から見える位置に置く（開いた瞬間に先頭が出て、探して下へ送らずに済む）。
///
/// 履歴を開いた（スマホのドロワ・PC の折りたたみ解除＝マウント）とき、または今いる会話が
/// 変わったときに 1 度だけ行う。見えていれば動かさない（PC で行を押した直後に一覧が跳ねない）。
/// 読み込み済みに無い古い会話は、続きを [`MAX_REVEAL_PAGES`] ページまで読み足して探す。
/// 読み足すのは**開いた時点で今いた会話**だけ。開いたまま移った先（作ったばかりの会話は
/// 先頭ページの取り直しより先に遷移する・共有された他人の会話は一覧に載らない）のために
/// 古いページを取りに行かない。
/// 1 度見せた後は追わない（自分でスクロールした位置を、読み足しや通知で巻き戻さない）。
function useRevealActiveThread({
  containerRef,
  activeId,
  threads,
  loading,
  loadingMore,
  hasMore,
  loadMore,
}: {
  containerRef: React.RefObject<HTMLDivElement | null>;
  activeId: string | null;
  threads: Thread[];
  loading: boolean;
  loadingMore: boolean;
  hasMore: boolean;
  loadMore: () => void;
}) {
  const done = React.useRef<{
    id: string | null;
    pages: number;
    revealed: boolean;
    /// 読み足しを頼んだ時点の件数（null は未依頼）。増えていなければ取得に失敗した。
    requestedAt: number | null;
  }>({ id: null, pages: 0, revealed: false, requestedAt: null });
  const [mountedWith] = React.useState(activeId);

  // 描画前に位置を決める（先頭が一瞬見えてから飛ぶちらつきを出さない）。
  React.useLayoutEffect(() => {
    if (done.current.id !== activeId) {
      done.current = { id: activeId, pages: 0, revealed: false, requestedAt: null };
    }
    const state = done.current;
    const container = containerRef.current;
    if (!activeId || state.revealed || loading || !container) return;
    const row = container.querySelector<HTMLElement>('[aria-current="page"]');
    if (row) {
      state.revealed = true;
      const box = container.getBoundingClientRect();
      const r = row.getBoundingClientRect();
      if (r.top >= box.top && r.bottom <= box.bottom) return;
      // 一覧の中ほどに置く（前後の会話も見えるように）。
      container.scrollTop += r.top - box.top - (box.height - r.height) / 2;
      return;
    }
    // 読み込み済みなのに行が無い（種類フィルタで隠れている）なら探さない。
    if (threads.some((t) => t.id === activeId)) {
      state.revealed = true;
      return;
    }
    // 開いたまま移った先は読み足さず、通知での取り直しで行が現れるのを待つ。
    if (activeId !== mountedWith) return;
    // 読み足し中は結果を待つ（最後のページに載っているかを見てから諦める）。
    if (loadingMore) return;
    if (state.requestedAt !== null) {
      // 取得できたページだけを数える。失敗したら今回は諦める（見つからない扱いにはしない・
      // 失敗を繰り返し叩かない）。
      const loaded = threads.length > state.requestedAt;
      state.requestedAt = null;
      if (!loaded) {
        state.revealed = true;
        return;
      }
      state.pages += 1;
    }
    if (!hasMore || state.pages >= MAX_REVEAL_PAGES || UNREACHABLE.has(activeId)) {
      // 探しきれなかった会話は覚えておき、開き直すたびに同じページを取り直さない。
      if (hasMore) UNREACHABLE.add(activeId);
      state.revealed = true;
      return;
    }
    state.requestedAt = threads.length;
    loadMore();
  }, [containerRef, activeId, mountedWith, threads, loading, loadingMore, hasMore, loadMore]);
}

/// 今いる会話を探して読み足す上限（1 ページ 30 件）。これより古い会話は先頭のままにする。
const MAX_REVEAL_PAGES = 5;
/// 上限まで読み足しても見つからなかった会話（ページ内で共有・ドロワを開き直しても探さない）。
const UNREACHABLE = new Set<string>();

/// 今の画面の会話 ID（通常は `/c/{id}`、文書の横の会話は `?thread=`）。
function activeThreadIdOf(pathname: string, threadParam: string | null): string | null {
  const m = /^\/c\/([^/]+)/.exec(pathname);
  return m ? decodeURIComponent(m[1]) : threadParam;
}

function SidebarChatHistoryInner({
  mobile,
  onNavigate,
}: {
  mobile: boolean;
  onNavigate?: () => void;
}) {
  const { threads, loading, loadingMore, hasMore, loadMore, patchLocal, upsertLocal } =
    useThreadList();
  const { threads: pinned, patchLocal: patchPinned } = usePinnedThreads();
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
  const scrollRef = React.useRef<HTMLDivElement | null>(null);
  useRevealActiveThread({
    containerRef: scrollRef,
    activeId: activeThreadIdOf(pathname, searchParams.get("thread")),
    threads,
    loading,
    loadingMore,
    hasMore,
    loadMore,
  });

  const actions = React.useMemo<ThreadRowActions>(
    () => ({
      rename: async (thread, title) => {
        await renameThread(thread.id, title);
        patchLocal(thread.id, { title });
        patchPinned(thread.id, { title });
      },
      togglePin: (thread, pin) => {
        setThreadPinned(thread.id, pin)
          .then(() => {
            // 外した行は日付の欄へ戻す。先頭ページより古い行は通知の取り直しでは戻らないので、
            // 手元に無ければここで足す（読み込み済みの範囲に入るときだけ）。
            if (pin) patchLocal(thread.id, { pinnedAt: new Date().toISOString() });
            else upsertLocal({ ...thread, pinnedAt: null });
          })
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
    [patchLocal, patchPinned, upsertLocal],
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
    <div
      ref={scrollRef}
      className="scrollbar-subtle min-h-0 flex-1 overflow-y-auto overscroll-y-contain px-2.5 pb-2 max-md:px-2"
    >
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
