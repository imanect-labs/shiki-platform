"use client";

import * as React from "react";
import Link from "next/link";
import { usePathname, useSearchParams } from "next/navigation";
import {
  FileText,
  Link2,
  Loader2,
  MessageSquareText,
  MoreHorizontal,
  NotebookPen,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { formatThreadTime, groupThreadsByDate, type Thread } from "@/lib/chat-api";
import { useInfiniteSentinel } from "@/hooks/use-infinite-list";
import { useLocalStorage } from "@/hooks/use-local-storage";
import { useThreadList } from "@/hooks/use-thread-list";
import {
  threadDisplay,
  threadHref,
  threadOriginKind,
  type ThreadOriginKind,
} from "@/lib/thread-link";
import { toast } from "@/components/ui/use-toast";
import { ActiveIndicator } from "@/components/ui/motion-primitives";
import { SegmentedControl } from "@/components/ui/segmented-control";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useLongPress } from "./use-long-press";

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

/// 文書由来の会話の先頭に置く種類アイコン（小さな角丸タイル）。
function OriginTile({ kind }: { kind: Exclude<ThreadOriginKind, null> }) {
  const Icon = kind === "note" ? NotebookPen : FileText;
  return (
    <span
      aria-hidden
      className="flex size-5 shrink-0 items-center justify-center rounded-[6px] bg-sidebar-foreground/[0.07] text-sidebar-foreground/60 max-md:size-8 max-md:rounded-lg"
    >
      <Icon className="size-3 max-md:size-4" />
    </span>
  );
}

/// 履歴 1 行。右端に更新時刻。アクティブは塗り。
///
/// 行の操作（今は「リンクをコピー」のみ。リネーム/削除は backend 未提供）はメニューにまとめる:
/// マウスはホバーで出る ⋯ か右クリック、タッチは長押し（Android は contextmenu、iOS はタイマー）。
/// ⋯ はタッチ端末では押せない（見えない操作を行の上に重ねない）が、メニューの位置合わせに使う。
///
/// **文書由来スレッド**は先頭に由来のアイコンを出し、行のリンク先を**その文書の画面（この会話を
/// アクティブに）**にする（文書からも履歴からも辿れる）。ノート由来（issue #282）はノートの分割
/// ビュー、Office 文書由来（#598）は Office 画面。通常チャットは従来どおり /c/{id}。
function ThreadRow({
  thread,
  active,
  mobile,
  onNavigate,
}: {
  thread: Thread;
  active: boolean;
  mobile: boolean;
  onNavigate?: () => void;
}) {
  const { kind, title, detail } = threadDisplay(thread);
  const href = threadHref(thread);
  const [menuOpen, setMenuOpen] = React.useState(false);
  const titleId = React.useId();
  // 長押し・右クリックで開いたメニューは、閉じたときに見えない ⋯ へフォーカスを戻さない。
  const openedByGesture = React.useRef(false);
  const openByGesture = React.useCallback(() => {
    openedByGesture.current = true;
    setMenuOpen(true);
  }, []);
  const longPress = useLongPress(openByGesture);

  const copyLink = async () => {
    try {
      await navigator.clipboard.writeText(`${window.location.origin}${href}`);
      toast({ description: "リンクをコピーしました。" });
    } catch {
      toast({ description: "リンクをコピーできませんでした。" });
    }
  };

  return (
    <li className="group/thread relative isolate">
      {active ? (
        mobile ? (
          <span aria-hidden className="absolute inset-0 -z-10 rounded-[10px] bg-sidebar-accent" />
        ) : (
          <ActiveIndicator
            layoutId="sidebar-active-thread"
            className="absolute inset-0 -z-10 rounded-[9px] bg-sidebar-accent"
          />
        )
      ) : null}
      <Link
        href={href}
        onClick={(e) => {
          if (longPress.consumeClick()) {
            e.preventDefault();
            return;
          }
          // モバイルのドロワーは行を選んだら閉じる（ナビ項目と同じ・開いたままだと遷移先が隠れる）。
          onNavigate?.();
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          openByGesture();
        }}
        {...longPress.handlers}
        aria-current={active ? "page" : undefined}
        title={detail ? `${title}（${detail}）` : title}
        className={cn(
          // 長押しで OS のリンクプレビュー・文字選択を出さない（行メニューを出すため）。
          "flex select-none items-center gap-2 rounded-[9px] pl-2.5 pr-2 outline-none [-webkit-touch-callout:none]",
          "h-8 text-[13px] transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring",
          "max-md:h-12 max-md:gap-3 max-md:rounded-[10px] max-md:px-3 max-md:text-[15px]",
          detail && "max-md:h-14",
          active
            ? "font-medium text-sidebar-foreground"
            : "text-sidebar-foreground/75 hover:bg-sidebar-accent/60 hover:text-sidebar-foreground active:bg-sidebar-accent/60",
        )}
      >
        {kind ? <OriginTile kind={kind} /> : null}
        <span className="min-w-0 flex-1">
          <span id={titleId} className="block truncate">
            {title}
          </span>
          {detail ? (
            // スマホは 2 行目に出す。PC は 1 行の密度を保ち、補足は読み上げとツールチップだけ。
            <>
              <span className="hidden truncate text-xs font-normal text-sidebar-foreground/50 max-md:block">
                {detail}
              </span>
              <span className="sr-only max-md:hidden">{detail}</span>
            </>
          ) : null}
        </span>
        <time
          dateTime={thread.updatedAt}
          className={cn(
            "shrink-0 text-[11px] font-normal tabular-nums text-sidebar-foreground/40 transition-opacity max-md:text-xs",
            // マウスでは ⋯ と同じ場所に出るので、⋯ が見える間は隠す。
            "pointer-fine:group-hover/thread:opacity-0 pointer-fine:group-has-[:focus-visible]/thread:opacity-0",
            menuOpen && "opacity-0",
          )}
        >
          {formatThreadTime(thread.updatedAt)}
        </time>
      </Link>
      <DropdownMenu
        open={menuOpen}
        onOpenChange={(open) => {
          if (open) openedByGesture.current = false;
          setMenuOpen(open);
        }}
      >
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            // 名前は固定にし、どのスレッドかは説明として読ませる（題名の語が他のボタン名と衝突しない）。
            aria-label="スレッドの操作"
            aria-describedby={titleId}
            className={cn(
              "absolute right-1 top-1/2 flex size-6 -translate-y-1/2 items-center justify-center rounded-md text-sidebar-foreground/55 outline-none",
              "opacity-0 transition-opacity duration-[var(--duration-fast)] focus-visible:opacity-100 data-[state=open]:opacity-100",
              "hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring",
              "pointer-fine:group-hover/thread:opacity-100 pointer-fine:group-has-[:focus-visible]/thread:opacity-100",
              "pointer-coarse:pointer-events-none max-md:right-2",
            )}
          >
            <MoreHorizontal className="size-4" aria-hidden />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align="end"
          className="min-w-44"
          onCloseAutoFocus={(e) => {
            if (openedByGesture.current) e.preventDefault();
          }}
        >
          <DropdownMenuItem onSelect={() => void copyLink()}>
            <Link2 aria-hidden />
            リンクをコピー
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </li>
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
  const done = React.useRef<{ id: string | null; pages: number; revealed: boolean }>({
    id: null,
    pages: 0,
    revealed: false,
  });
  const [mountedWith] = React.useState(activeId);

  // 描画前に位置を決める（先頭が一瞬見えてから飛ぶちらつきを出さない）。
  React.useLayoutEffect(() => {
    if (done.current.id !== activeId) done.current = { id: activeId, pages: 0, revealed: false };
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
    if (!hasMore || state.pages >= MAX_REVEAL_PAGES || UNREACHABLE.has(activeId)) {
      // 探しきれなかった会話は覚えておき、開き直すたびに同じページを取り直さない。
      if (hasMore) UNREACHABLE.add(activeId);
      state.revealed = true;
      return;
    }
    state.pages += 1;
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
  const { threads, loading, loadingMore, hasMore, loadMore } = useThreadList();
  // 種類フィルタで読み足した行が全部隠れると sentinel が見えたままになり、IntersectionObserver が
  // 再発火しない。取得完了ごとに件数を渡し、見えていれば続きを読む（!hasMore で止まる）。
  const sentinelRef = useInfiniteSentinel(
    loadMore,
    hasMore && !loading,
    loadingMore ? undefined : threads.length,
  );
  const pathname = usePathname();
  // 文書由来行は `/notes/{id}?thread=` ・ `/office/{id}?thread=` へ張るため、active 判定は thread
  // クエリまで見る（同一文書に複数会話がある 1:N で、全行が同時 active になるのを防ぐ）。
  const searchParams = useSearchParams();
  const [storedFilter, setFilter] = useLocalStorage<HistoryFilter>(FILTER_KEY, "all");
  const filter: HistoryFilter = FILTER_OPTIONS.some((o) => o.value === storedFilter)
    ? storedFilter
    : "all";

  const shown = React.useMemo(
    () => threads.filter((t) => matchesFilter(t, filter)),
    [threads, filter],
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
  } else if (groups.length === 0 && !hasMore) {
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
    body = groups.map((group) => (
      <div key={group.label} className="mb-1">
        <div className="px-2.5 pb-1 pt-3 text-[11px] font-semibold uppercase tracking-[0.06em] text-sidebar-foreground/40 max-md:px-3 max-md:text-xs">
          {group.label}
        </div>
        <ul className="flex flex-col gap-0.5">
          {group.threads.map((thread) => (
            <ThreadRow
              key={thread.id}
              thread={thread}
              active={isActiveThread(thread, pathname, searchParams.get("thread"))}
              mobile={mobile}
              onNavigate={onNavigate}
            />
          ))}
        </ul>
      </div>
    ));
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
    </div>
  );
}
