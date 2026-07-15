"use client";

/// ノートに紐づくチャットパネル（Task 11P.5 ＋ issue #282・分割ビューのサイドパネル）。
///
/// - ノート:会話 = **1:N**。アクティブ会話 id は Yjs Map "meta" の **active_thread_id**（旧
///   `thread_id` は後方互換で読む）。会話一覧はサーバの `origin_note_id` を単一真実源として引く。
/// - 「新しい会話」で新スレッド（origin_note_id 付き）を作り、アクティブを切替。旧会話は
///   ノート紐付けを保持したまま履歴に残る（サイドバーにも「ノート由来」で出る）。
/// - 既存チャット UI（`Conversation`）をそのまま再利用。ノート共有とスレッド共有は別 ReBAC・
///   fail-closed（スレッド閲覧権が無ければ `Conversation` が「見つかりません」を出す）。

import { Check, ChevronDown, Loader2, MessagesSquare, Plus } from "lucide-react";
import * as React from "react";
import type * as Y from "yjs";

import { Conversation } from "@/components/chat/conversation";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { createThread, getThread, listThreads, type Thread } from "@/lib/chat-api";
import { cn } from "@/lib/utils";

/// meta からアクティブ会話 id を読む（active_thread_id 優先・旧 thread_id 後方互換）。
function readActiveThreadId(meta: Y.Map<unknown>): string | null {
  const v = meta.get("active_thread_id") ?? meta.get("thread_id");
  return typeof v === "string" && v.length > 0 ? v : null;
}

function setActiveThreadId(meta: Y.Map<unknown>, id: string): void {
  // 新旧キーを両方書く（他の共同編集者/旧クライアントも追従できるよう）。
  meta.set("active_thread_id", id);
  meta.set("thread_id", id);
}

export function NoteChatPanel({
  meta,
  noteId,
  noteName,
  editable,
  initialThreadId,
}: {
  meta: Y.Map<unknown>;
  /// ノートのストレージ node id（origin_note_id に使う）。
  noteId: string;
  noteName: string;
  /// editor のみが会話を新規作成できる（viewer は既存会話の閲覧のみ）。
  editable: boolean;
  /// サイドバー等から特定会話を開くときの初期アクティブ（?thread=）。
  initialThreadId?: string | null;
}) {
  const cleanName = React.useMemo(() => noteName.replace(/\.md$/i, ""), [noteName]);
  // ?thread=（サイドバー/保存直後の遷移）があればそれを初期アクティブにする。null 初期だと
  // 「initial 適用」と「初回会話の自動作成」が同一初回レンダーで競合し、余分な会話が作られる。
  const [activeThreadId, setActiveThreadIdState] = React.useState<string | null>(
    () => initialThreadId ?? readActiveThreadId(meta),
  );
  const [conversations, setConversations] = React.useState<Thread[]>([]);
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  // 作成の多重発火を防ぐ ref（StrictMode の二重実行・再マウントに耐える）。
  const creatingRef = React.useRef(false);

  // meta の active 変化（他の共同編集者が作成/切替）に追従する。
  React.useEffect(() => {
    const update = () => setActiveThreadIdState(readActiveThreadId(meta));
    meta.observe(update);
    return () => meta.unobserve(update);
  }, [meta]);

  // ?thread= で来た会話をアクティブへ（サイドバーの「ノート由来」からの遷移）。同一ノートの別会話
  // リンク（?thread が A→B に変わる）でもコンポーネントは再マウントされないため、**値が変わるたび**
  // 反映する（一度きりにすると deep link 切替が効かない）。
  const appliedInitialRef = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (!initialThreadId || appliedInitialRef.current === initialThreadId) return;
    appliedInitialRef.current = initialThreadId;
    if (initialThreadId !== readActiveThreadId(meta)) {
      setActiveThreadId(meta, initialThreadId);
      setActiveThreadIdState(initialThreadId);
    }
  }, [initialThreadId, meta]);

  // このノートの会話一覧を origin_note_id で引く。アクティブが一覧に無い（旧スレッド）場合は補う。
  const refreshConversations = React.useCallback(async () => {
    try {
      const { threads } = await listThreads(undefined, { originNoteId: noteId });
      let list = threads;
      const active = readActiveThreadId(meta);
      if (active && !list.some((t) => t.id === active)) {
        // 旧 note（origin 無しで作られた thread）でも一覧に載せる。
        const t = await getThread(active).catch(() => null);
        if (t) list = [t, ...list];
      }
      // 作成順（古い→新しい）で番号付けできるよう昇順に。
      list.sort((a, b) => a.createdAt.localeCompare(b.createdAt));
      setConversations(list);
    } catch {
      // 一覧取得失敗は致命的ではない（アクティブ会話は表示できる）。
    }
  }, [noteId, meta]);

  React.useEffect(() => {
    void refreshConversations();
  }, [refreshConversations, activeThreadId]);

  // 会話未作成なら（editor のとき）最初の会話を作る。ref ガードで一度だけ。
  React.useEffect(() => {
    if (activeThreadId || !editable || creatingRef.current) return;
    creatingRef.current = true;
    createThread(`ノート: ${cleanName}`, false, { originNoteId: noteId })
      .then((thread) => {
        setActiveThreadId(meta, thread.id);
        setActiveThreadIdState(thread.id);
      })
      .catch(() => {
        creatingRef.current = false;
        setError("チャットの準備に失敗しました。");
      });
  }, [activeThreadId, editable, meta, cleanName, noteId]);

  const selectConversation = React.useCallback(
    (id: string) => {
      if (id === activeThreadId) return;
      setActiveThreadId(meta, id);
      setActiveThreadIdState(id);
    },
    [activeThreadId, meta],
  );

  const startNewConversation = React.useCallback(() => {
    if (busy) return;
    setBusy(true);
    setError(null);
    // 連番のタイトル（サイドバー履歴で識別しやすく）。
    const n = conversations.length + 1;
    const title = n <= 1 ? `ノート: ${cleanName}` : `ノート: ${cleanName} (${n})`;
    createThread(title, false, { originNoteId: noteId })
      .then((thread) => {
        setActiveThreadId(meta, thread.id);
        setActiveThreadIdState(thread.id);
        void refreshConversations();
      })
      .catch(() => setError("新しい会話を作成できませんでした。"))
      .finally(() => setBusy(false));
  }, [busy, conversations.length, cleanName, noteId, meta, refreshConversations]);

  const activeIndex = conversations.findIndex((c) => c.id === activeThreadId);

  return (
    <div
      className="flex h-full min-h-0 w-full flex-col"
      aria-label="ノートのチャット"
      data-testid="note-chat-panel"
    >
      {/* 会話スイッチャ＋新しい会話（1:N・issue #282）。会話が確定してから出す。 */}
      {activeThreadId ? (
        <div
          className="flex shrink-0 items-center gap-1 px-2 py-1.5 shiki-dash-bottom"
          data-testid="note-conversation-switcher"
        >
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                className="inline-flex min-w-0 items-center gap-1.5 rounded-md px-2 py-1 text-[13px] text-foreground/80 outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
                title="会話を切り替える"
              >
                <MessagesSquare className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
                <span className="truncate">
                  {activeIndex >= 0 ? `会話 ${activeIndex + 1}` : "会話"}
                </span>
                {conversations.length > 1 ? (
                  <span className="rounded-full bg-muted px-1.5 text-[11px] text-muted-foreground">
                    {conversations.length}
                  </span>
                ) : null}
                <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" aria-hidden />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="w-56">
              <DropdownMenuLabel>このノートの会話</DropdownMenuLabel>
              {conversations.map((c, i) => (
                <DropdownMenuItem
                  key={c.id}
                  onClick={() => selectConversation(c.id)}
                  className="gap-2"
                >
                  <Check
                    className={cn(
                      "size-3.5 shrink-0",
                      c.id === activeThreadId ? "opacity-100" : "opacity-0",
                    )}
                    aria-hidden
                  />
                  <span className="flex-1 truncate">会話 {i + 1}</span>
                  <span className="shrink-0 text-[11px] text-muted-foreground">
                    {new Date(c.updatedAt).toLocaleDateString("ja-JP", {
                      month: "numeric",
                      day: "numeric",
                    })}
                  </span>
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <span className="flex-1" />
          {editable ? (
            <button
              type="button"
              onClick={startNewConversation}
              disabled={busy}
              data-testid="note-new-conversation"
              className="inline-flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-[12px] text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
              title="新しい会話を開始（この会話は履歴に残ります）"
            >
              {busy ? (
                <Loader2 className="size-3.5 animate-spin" aria-hidden />
              ) : (
                <Plus className="size-3.5" aria-hidden />
              )}
              新しい会話
            </button>
          ) : null}
        </div>
      ) : null}

      <div className="min-h-0 flex-1">
        {activeThreadId ? (
          // key で会話切替時に確実に再マウント（別スレッドのメッセージを引き直す）。
          <Conversation key={activeThreadId} threadId={activeThreadId} variant="panel" />
        ) : error ? (
          <div className="flex h-full items-center justify-center px-4 text-center text-sm text-muted-foreground">
            {error}
          </div>
        ) : editable ? (
          <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            チャットを準備しています…
          </div>
        ) : (
          <div className="flex h-full items-center justify-center px-4 text-center text-sm text-muted-foreground">
            まだチャットが開始されていません。
          </div>
        )}
      </div>
    </div>
  );
}
