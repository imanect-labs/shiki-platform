"use client";

/// スライドページ（Task 11.1）。/slides/[id] で .slide ファイルを表示する。
///
/// - 接続は CollabProvider（BFF セッション Cookie・y-websocket 互換ワイヤ）。真実は Yjs。
/// - 本 PR は閲覧ビューのみ。編集（GrapesJS 砂箱エディタ・Task 11.2）は本ページの
///   editable 分岐に載る。描画は SlideFrame（DOMPurify＋sandbox iframe・PIT-40）。

import { EditorLoading } from "@/components/shell/editor-loading";
import { MessageSquare, X } from "lucide-react";
import { useParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { NoteChatPanel } from "@/components/notes/note-chat-panel";
import { SlideHeaderSlot } from "@/components/slides/slide-header-slot";
import { SlideWorkspace } from "@/components/slides/slide-workspace";
import { EmptyState } from "@/components/ui/empty-state";
import { ShareLinkUnlock, unlockTokenFromUrl } from "@/components/share/share-link-unlock";
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
  // 直近の要素選択（ヘッダの「AI に依頼」で開いた瞬間に挿入する材料）。
  const latestSelRef = React.useRef<{ slideId: string; html: string } | null>(null);

  const insertSelection = React.useCallback(
    (sel: { slideId: string; html: string }) => {
      setPendingSelection({
        kind: "slide_selection",
        node_id: nodeId,
        excerpt: sel.html,
        locator: { slide_id: sel.slideId },
      });
    },
    [nodeId],
  );

  // 選択の変化: パネルが開いていれば自動でチャットへ挿入する。
  const handleSelectionChange = React.useCallback(
    (sel: { slideId: string; html: string } | null) => {
      latestSelRef.current = sel;
      setChatOpen((open) => {
        if (open && sel) insertSelection(sel);
        return open;
      });
    },
    [insertSelection],
  );

  // 「AI に依頼」= アシスタントパネルを開く（＋その時の選択があればチャットへ挿入）。
  const openAssistant = React.useCallback(() => {
    setChatOpen((open) => {
      if (!open && latestSelRef.current) insertSelection(latestSelRef.current);
      return !open;
    });
  }, [insertSelection]);
  const [session, setSession] = React.useState<{
    doc: Y.Doc;
    provider: CollabProvider;
  } | null>(null);

  // アクセス判定の取得（解錠後に再取得できるよう callback 化）。
  const loadAccess = React.useCallback(() => {
    setAccess("loading");
    getCollabAccess(nodeId)
      .then((a) => setAccess(a ?? "notfound"))
      .catch(() => setAccess("notfound"));
  }, [nodeId]);

  React.useEffect(() => {
    loadAccess();
  }, [loadAccess]);

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
      <EditorLoading kind="slide" message="スライドを開いています…" />
    );
  }
  if (access === "notfound" || access === null) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-6 p-6">
        <EmptyState
          title="スライドが見つかりません"
          description="削除されたか、アクセス権がありません。パスワード付き共有リンクの場合はパスワードで開けます。"
        />
        {unlockTokenFromUrl() ? (
          <ShareLinkUnlock token={unlockTokenFromUrl()!} onUnlocked={loadAccess} autoFocus />
        ) : null}
      </div>
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
        onToggleChat={openAssistant}
      />
      <div className="relative min-h-0 flex-1">
        {session && synced ? (
          <div className={chatOpen ? "h-full lg:pr-[28rem]" : "h-full"}>
            <SlideWorkspace
              doc={session.doc}
              editable={editable}
              name={access.name.replace(/\.slide$/i, "")}
              // 選択→AI 指示（Task 11.10）: パネルが開いていれば選択要素を自動挿入する。
              onSelectionChange={handleSelectionChange}
            />
          </div>
        ) : (
          <EditorLoading kind="slide" message="同期しています…" />
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
