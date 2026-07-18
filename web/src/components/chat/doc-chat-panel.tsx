"use client";

/// ドキュメントに紐づくチャットパネル（分割ビューのサイドパネル・汎用）。
///
/// ノート（Yjs meta にアクティブ会話 id）と Office 文書（localStorage にアクティブ会話 id）で
/// **アクティブ会話の保存先だけが違う**ため、そこを [`ActiveThreadStore`] に抽象化して共有する。
/// ドキュメント:会話 = 1:N（`origin_note_id` = ドキュメントの node id で会話一覧を引く）。
/// 会話 UI は既存 [`Conversation`]（variant=panel）を再利用する。

import { Check, ChevronDown, Loader2, MessagesSquare, Plus } from "lucide-react";
import * as React from "react";

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

/// アクティブ会話 id の保存先の抽象（ノート=Yjs meta・Office=localStorage）。
export interface ActiveThreadStore {
  read: () => string | null;
  write: (id: string) => void;
  /// 外部変更（別共同編集者の切替等）を購読する。無い保存先は no-op を返してよい。
  subscribe: (onChange: () => void) => () => void;
}

export function DocChatPanel({
  store,
  nodeId,
  title,
  label,
  editable,
  initialThreadId,
  testIdPrefix = "doc",
}: {
  store: ActiveThreadStore;
  /// ドキュメントのストレージ node id（origin_note_id に使う）。
  nodeId: string;
  /// 会話タイトルの接頭（例「ノート: 提案書」）。
  title: string;
  /// 一覧見出しの語（例「このノートの会話」「この文書の会話」）。
  label: string;
  /// editor のみが会話を新規作成できる（viewer は既存会話の閲覧のみ）。
  editable: boolean;
  /// サイドバー等から特定会話を開くときの初期アクティブ（?thread=）。
  initialThreadId?: string | null;
  /// data-testid の接頭（既存 e2e 互換: ノート="note"・Office="office"）。
  testIdPrefix?: string;
}) {
  const [activeThreadId, setActiveThreadIdState] = React.useState<string | null>(
    () => initialThreadId ?? store.read(),
  );
  const [conversations, setConversations] = React.useState<Thread[]>([]);
  const [error, setError] = React.useState<string | null>(null);
  const [busy, setBusy] = React.useState(false);
  const creatingRef = React.useRef(false);

  const setActive = React.useCallback(
    (id: string) => {
      store.write(id);
      setActiveThreadIdState(id);
    },
    [store],
  );

  // 外部変更（別クライアントの切替）に追従する。
  React.useEffect(() => {
    return store.subscribe(() => setActiveThreadIdState(store.read()));
  }, [store]);

  // ?thread= で来た会話をアクティブへ（値が変わるたび反映・deep link 切替対応）。
  const appliedInitialRef = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (!initialThreadId || appliedInitialRef.current === initialThreadId) return;
    appliedInitialRef.current = initialThreadId;
    if (initialThreadId !== store.read()) setActive(initialThreadId);
  }, [initialThreadId, store, setActive]);

  // このドキュメントの会話一覧を origin_note_id で引く。アクティブが一覧に無ければ補う。
  const refreshConversations = React.useCallback(async () => {
    try {
      const { threads } = await listThreads(undefined, { originNoteId: nodeId });
      let list = threads;
      const active = store.read();
      if (active && !list.some((t) => t.id === active)) {
        const t = await getThread(active).catch(() => null);
        if (t) list = [t, ...list];
      }
      list.sort((a, b) => a.createdAt.localeCompare(b.createdAt));
      setConversations(list);
    } catch {
      /* 一覧取得失敗は致命的ではない（アクティブ会話は表示できる）。 */
    }
  }, [nodeId, store]);

  React.useEffect(() => {
    void refreshConversations();
  }, [refreshConversations, activeThreadId]);

  // 会話未作成なら（editor のとき）最初の会話を作る。ref ガードで一度だけ。
  React.useEffect(() => {
    if (activeThreadId || !editable || creatingRef.current) return;
    creatingRef.current = true;
    createThread(title, false, { originNoteId: nodeId })
      .then((thread) => setActive(thread.id))
      .catch(() => {
        creatingRef.current = false;
        setError("チャットの準備に失敗しました。");
      });
  }, [activeThreadId, editable, title, nodeId, setActive]);

  const selectConversation = React.useCallback(
    (id: string) => {
      if (id !== activeThreadId) setActive(id);
    },
    [activeThreadId, setActive],
  );

  const startNewConversation = React.useCallback(() => {
    if (busy) return;
    setBusy(true);
    setError(null);
    const n = conversations.length + 1;
    const t = n <= 1 ? title : `${title} (${n})`;
    createThread(t, false, { originNoteId: nodeId })
      .then((thread) => {
        setActive(thread.id);
        void refreshConversations();
      })
      .catch(() => setError("新しい会話を作成できませんでした。"))
      .finally(() => setBusy(false));
  }, [busy, conversations.length, title, nodeId, setActive, refreshConversations]);

  const activeIndex = conversations.findIndex((c) => c.id === activeThreadId);

  return (
    <div
      className="flex h-full min-h-0 w-full flex-col"
      aria-label="ドキュメントのチャット"
      data-testid={`${testIdPrefix}-chat-panel`}
    >
      {activeThreadId ? (
        <div
          className="flex shrink-0 items-center gap-1 px-2 py-1.5 shiki-dash-bottom"
          data-testid={`${testIdPrefix}-conversation-switcher`}
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
              <DropdownMenuLabel>{label}</DropdownMenuLabel>
              {conversations.map((c, i) => (
                <DropdownMenuItem key={c.id} onClick={() => selectConversation(c.id)} className="gap-2">
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
              data-testid={`${testIdPrefix}-new-conversation`}
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

/// localStorage をアクティブ会話 id の保存先にする [`ActiveThreadStore`]（Office 文書用・
/// Yjs を持たないドキュメントの per-user 会話継続）。
export function useLocalActiveThreadStore(key: string): ActiveThreadStore {
  return React.useMemo<ActiveThreadStore>(() => {
    const storageKey = `docchat:active:${key}`;
    return {
      read: () => {
        try {
          return window.localStorage.getItem(storageKey);
        } catch {
          return null;
        }
      },
      write: (id) => {
        try {
          window.localStorage.setItem(storageKey, id);
        } catch {
          /* プライベートモード等では保存できない（セッション内は state が保持する） */
        }
      },
      // localStorage は同一タブ内の setItem では storage イベントが出ないため購読対象なし。
      subscribe: () => () => {},
    };
  }, [key]);
}
