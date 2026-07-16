"use client";

/// スライドページ（Task 11.1）。/slides/[id] で .slide ファイルを表示する。
///
/// - 接続は CollabProvider（BFF セッション Cookie・y-websocket 互換ワイヤ）。真実は Yjs。
/// - 本 PR は閲覧ビューのみ。編集（GrapesJS 砂箱エディタ・Task 11.2）は本ページの
///   editable 分岐に載る。描画は SlideFrame（DOMPurify＋sandbox iframe・PIT-40）。

import { Loader2, MessageSquare, X } from "lucide-react";
import { useParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { NoteChatPanel } from "@/components/notes/note-chat-panel";
import { SlideHeaderSlot } from "@/components/slides/slide-header-slot";
import { SlideWorkspace } from "@/components/slides/slide-workspace";
import { EmptyState } from "@/components/ui/empty-state";
import { FadeSlide } from "@/components/ui/motion-primitives";
import { CollabProvider, type CollabStatus } from "@/lib/collab";
import { getCollabAccess, type CollabAccess } from "@/lib/notes-api";
import { setPendingSelection } from "@/lib/selection-context";

export default function SlidePage() {
  const params = useParams<{ id: string }>();
  const nodeId = params.id;
  const [access, setAccess] = React.useState<CollabAccess | null | "notfound" | "loading">(
    "loading",
  );
  const [status, setStatus] = React.useState<CollabStatus>("connecting");
  const [synced, setSynced] = React.useState(false);
  // アシスタントパネル（ノートと同じ分割ビュー・meta の active_thread_id を共用・Task 11.10）。
  const [chatOpen, setChatOpen] = React.useState(false);
  const toggleChat = React.useCallback(() => setChatOpen((v) => !v), []);
  const [session, setSession] = React.useState<{
    doc: Y.Doc;
    provider: CollabProvider;
  } | null>(null);

  React.useEffect(() => {
    let cancelled = false;
    getCollabAccess(nodeId)
      .then((a) => {
        if (!cancelled) setAccess(a ?? "notfound");
      })
      .catch(() => {
        if (!cancelled) setAccess("notfound");
      });
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  React.useEffect(() => {
    if (typeof access === "string" || access === null) return;
    const doc = new Y.Doc();
    const provider = new CollabProvider(nodeId, doc);
    const offStatus = provider.onStatus(setStatus);
    const offSynced = provider.onSynced(() => setSynced(true));
    setSession({ doc, provider });
    return () => {
      offStatus();
      offSynced();
      provider.destroy();
      doc.destroy();
      setSession(null);
      setSynced(false);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodeId, typeof access === "string" ? access : "ready"]);

  if (access === "loading") {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        スライドを開いています…
      </div>
    );
  }
  if (access === "notfound" || access === null) {
    return (
      <EmptyState
        title="スライドが見つかりません"
        description="削除されたか、アクセス権がありません。"
      />
    );
  }

  const editable = access.mode === "editor";

  return (
    <div className="flex h-full min-h-0 flex-col">
      <SlideHeaderSlot
        name={access.name.replace(/\.slide$/i, "")}
        editable={editable}
        status={status}
        synced={synced}
        provider={session?.provider ?? null}
        chatOpen={chatOpen}
        onToggleChat={toggleChat}
      />
      <div className="relative min-h-0 flex-1">
        {session && synced ? (
          <div className={chatOpen ? "h-full lg:pr-[28rem]" : "h-full"}>
            <SlideWorkspace
              doc={session.doc}
              editable={editable}
              name={access.name.replace(/\.slide$/i, "")}
              // 選択→AI 指示（Task 11.10）: 要素の HTML 抜粋をチップ化してアシスタントを開く。
              onAskAi={({ slideId, html }) => {
                setPendingSelection({
                  kind: "slide_selection",
                  node_id: nodeId,
                  excerpt: html,
                  locator: { slide_id: slideId },
                });
                setChatOpen(true);
              }}
            />
          </div>
        ) : (
          <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            同期しています…
          </div>
        )}
        {/* アシスタントパネル（ノートページと同型の浮遊カード・meta active_thread_id 共用） */}
        {session && chatOpen ? (
          <FadeSlide
            from="right"
            role="complementary"
            aria-label="スライドのアシスタント"
            className="absolute inset-y-3 right-3 z-20 flex w-[min(420px,calc(100%-1.5rem))] flex-col overflow-hidden rounded-2xl border bg-card shadow-lg"
          >
            <div className="flex h-11 shrink-0 items-center gap-2 px-3 shiki-dash-bottom">
              <MessageSquare className="size-4 text-muted-foreground" aria-hidden />
              <span className="flex-1 text-sm font-medium">アシスタント</span>
              <button
                type="button"
                onClick={() => setChatOpen(false)}
                aria-label="チャットを閉じる"
                className="flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground active:scale-90"
              >
                <X className="size-4" aria-hidden />
              </button>
            </div>
            <div className="min-h-0 flex-1">
              <NoteChatPanel
                meta={session.doc.getMap("meta")}
                noteId={nodeId}
                noteName={access.name}
                editable={editable}
                initialThreadId={null}
              />
            </div>
          </FadeSlide>
        ) : null}
      </div>
    </div>
  );
}
