"use client";

import * as React from "react";
import Link from "next/link";
import {
  FileText,
  Link2,
  MoreHorizontal,
  NotebookPen,
  Pencil,
  Pin,
  PinOff,
  Share2,
  Trash2,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { formatThreadTime, type Thread } from "@/lib/chat-api";
import { threadDisplay, threadHref, type ThreadOriginKind } from "@/lib/thread-link";
import { toast } from "@/components/ui/use-toast";
import { ActiveIndicator } from "@/components/ui/motion-primitives";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useLongPress } from "./use-long-press";

/// 行メニューから親へ渡す操作（ダイアログ・一覧の書き換えは親が持つ）。
export type ThreadRowActions = {
  rename: (thread: Thread, title: string) => Promise<void>;
  togglePin: (thread: Thread, pinned: boolean) => void;
  share: (thread: Thread) => void;
  remove: (thread: Thread) => void;
};

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

/// 長いタイトルは 1 行で末尾をフェードさせて切る（「…」で語を欠けさせない・ChatGPT / Claude と同じ）。
const TITLE_FADE =
  "overflow-hidden whitespace-nowrap [mask-image:linear-gradient(to_right,#000_calc(100%-1.5rem),transparent)]";

/// タイトルが変わったとき（自動タイトルが付いた・改名した）だけ、そっと差し替える。
function useTitleSwap(title: string): number {
  const [swaps, setSwaps] = React.useState(0);
  const prev = React.useRef(title);
  React.useEffect(() => {
    if (prev.current === title) return;
    prev.current = title;
    setSwaps((n) => n + 1);
  }, [title]);
  return swaps;
}

/// 行の中で名前を直接書き換える入力欄。Enter・フォーカスを外すと確定、Esc で取り消し。
function RenameField({
  initial,
  label,
  mobile,
  onDone,
}: {
  initial: string;
  label: string;
  mobile: boolean;
  onDone: (title: string | null) => void;
}) {
  const ref = React.useRef<HTMLInputElement>(null);
  const settled = React.useRef(false);
  React.useEffect(() => {
    // メニューが閉じてフォーカスを戻し終えてから入れる（でないと ⋯ に奪われる）。
    const id = requestAnimationFrame(() => {
      ref.current?.focus();
      ref.current?.select();
    });
    return () => cancelAnimationFrame(id);
  }, []);
  const finish = (value: string | null) => {
    if (settled.current) return;
    settled.current = true;
    onDone(value);
  };
  return (
    <input
      ref={ref}
      defaultValue={initial}
      aria-label={label}
      maxLength={200}
      enterKeyHint="done"
      onKeyDown={(e) => {
        if (e.key === "Enter" && !e.nativeEvent.isComposing) {
          e.preventDefault();
          finish(e.currentTarget.value);
        } else if (e.key === "Escape") {
          e.preventDefault();
          finish(null);
        }
      }}
      onBlur={(e) => finish(e.currentTarget.value)}
      className={cn(
        "h-8 w-full min-w-0 rounded-[9px] border border-sidebar-ring/50 bg-background px-2.5 text-[13px] text-sidebar-foreground outline-none",
        "focus-visible:ring-[3px] focus-visible:ring-sidebar-ring/20",
        // iOS は 16px 未満の入力欄でズームするため、スマホは 16px にする。
        mobile && "h-12 rounded-[10px] px-3 text-base",
      )}
    />
  );
}

/// 履歴 1 行。右端に更新時刻。アクティブは塗り。
///
/// 行の操作（共有・名前を変更・ピン留め・リンクをコピー・削除）はメニューにまとめる:
/// マウスはホバーで出る ⋯ か右クリック、タッチは長押し（Android は contextmenu、iOS はタイマー）。
/// ⋯ はタッチ端末では押せない（見えない操作を行の上に重ねない）が、メニューの位置合わせに使う。
/// 名前の変更は行の中で直接書き換える（ダイアログを挟まない・ChatGPT と同じ）。
///
/// **文書由来スレッド**は先頭に由来のアイコンを出し、行のリンク先を**その文書の画面（この会話を
/// アクティブに）**にする（文書からも履歴からも辿れる）。ノート由来（issue #282）はノートの分割
/// ビュー、Office 文書由来（#598）は Office 画面。通常チャットは従来どおり /c/{id}。
export function ThreadRow({
  thread,
  active,
  mobile,
  pinned,
  actions,
  onNavigate,
}: {
  thread: Thread;
  active: boolean;
  mobile: boolean;
  /// ピン留めの欄に出している行か（メニューの文言と、時刻の代わりのピンの印に使う）。
  pinned: boolean;
  actions: ThreadRowActions;
  onNavigate?: () => void;
}) {
  const { kind, title, detail } = threadDisplay(thread);
  const href = threadHref(thread);
  const [menuOpen, setMenuOpen] = React.useState(false);
  const [editing, setEditing] = React.useState(false);
  // 確定した名前を保存が返るまで先に見せる（失敗したら元に戻す）。
  const [pendingTitle, setPendingTitle] = React.useState<string | null>(null);
  const shownTitle = pendingTitle ?? title;
  const swaps = useTitleSwap(shownTitle);
  const titleId = React.useId();
  // 長押し・右クリックで開いたメニューは、閉じたときに見えない ⋯ へフォーカスを戻さない。
  // 名前の変更を選んだときも戻さない（入力欄へ入れる）。
  const skipFocusReturn = React.useRef(false);
  const openByGesture = React.useCallback(() => {
    skipFocusReturn.current = true;
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

  const finishRename = (value: string | null) => {
    setEditing(false);
    const next = value?.replace(/\s+/g, " ").trim() ?? "";
    // 空・変更なしは取り消し扱い（空の名前は付けられない）。
    if (!next || next === thread.title) return;
    setPendingTitle(next);
    actions
      .rename(thread, next)
      .catch((e: unknown) => {
        toast({
          variant: "destructive",
          description: e instanceof Error ? e.message : "名前を変更できませんでした。",
        });
      })
      .finally(() => setPendingTitle(null));
  };

  if (editing) {
    return (
      <li className="relative">
        <RenameField
          initial={thread.title}
          label="チャットの名前"
          mobile={mobile}
          onDone={finishRename}
        />
      </li>
    );
  }

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
        title={detail ? `${shownTitle}（${detail}）` : shownTitle}
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
          <span
            key={swaps}
            id={titleId}
            className={cn(
              "block",
              TITLE_FADE,
              swaps > 0 && "animate-in fade-in-0 duration-500 motion-reduce:animate-none",
            )}
          >
            {shownTitle}
          </span>
          {detail ? (
            // スマホは 2 行目に出す。PC は 1 行の密度を保ち、補足は読み上げとツールチップだけ。
            <>
              <span
                className={cn(
                  "hidden text-xs font-normal text-sidebar-foreground/50 max-md:block",
                  TITLE_FADE,
                )}
              >
                {detail}
              </span>
              <span className="sr-only max-md:hidden">{detail}</span>
            </>
          ) : null}
        </span>
        <span
          className={cn(
            "shrink-0 text-[11px] font-normal tabular-nums text-sidebar-foreground/40 transition-opacity max-md:text-xs",
            // マウスでは ⋯ と同じ場所に出るので、⋯ が見える間は隠す。
            "pointer-fine:group-hover/thread:opacity-0 pointer-fine:group-has-[:focus-visible]/thread:opacity-0",
            menuOpen && "opacity-0",
          )}
        >
          {pinned ? (
            <>
              <Pin className="size-3" aria-hidden />
              <span className="sr-only">ピン留め</span>
            </>
          ) : (
            <time dateTime={thread.updatedAt}>{formatThreadTime(thread.updatedAt)}</time>
          )}
        </span>
      </Link>
      <DropdownMenu
        open={menuOpen}
        onOpenChange={(open) => {
          if (open) skipFocusReturn.current = false;
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
            if (skipFocusReturn.current) e.preventDefault();
          }}
        >
          <DropdownMenuItem onSelect={() => actions.share(thread)}>
            <Share2 aria-hidden />
            共有
          </DropdownMenuItem>
          <DropdownMenuItem
            onSelect={() => {
              skipFocusReturn.current = true;
              setEditing(true);
            }}
          >
            <Pencil aria-hidden />
            名前を変更
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => actions.togglePin(thread, !pinned)}>
            {pinned ? <PinOff aria-hidden /> : <Pin aria-hidden />}
            {pinned ? "ピン留めを外す" : "ピン留め"}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void copyLink()}>
            <Link2 aria-hidden />
            リンクをコピー
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onSelect={() => actions.remove(thread)}>
            <Trash2 aria-hidden />
            削除
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </li>
  );
}
