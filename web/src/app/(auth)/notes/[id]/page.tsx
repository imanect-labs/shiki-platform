"use client";

/// ノートページ（Task 11P.3）。/notes/[id] で .md ファイルを共同編集する。
///
/// - 接続は CollabProvider（BFF セッション Cookie・y-websocket 互換ワイヤ）。
/// - 権限はサーバが強制する（viewer の update 不受理・剥奪切断）。ここでは
///   /collab/docs/{id}/access の表示用ヒントで editable を切り替える。
/// - 11P.5 で本ページが「ノート×チャット分割ビュー」のホストになる。

import { Loader2, MessageSquare, X } from "lucide-react";
import { useParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { embedSlashItems } from "@/components/notes/embed/embed-slash-items";
import { MetadataPanel } from "@/components/notes/metadata-panel";
import { NoteChatPanel } from "@/components/notes/note-chat-panel";
import { NoteEditor } from "@/components/notes/note-editor";
import { NoteSyncSlot } from "@/components/notes/note-header-slot";
import { EmptyState } from "@/components/ui/empty-state";
import { useMe } from "@/hooks/use-me";
import { CollabProvider, type CollabStatus } from "@/lib/collab";
import { getCollabAccess, type CollabAccess } from "@/lib/notes-api";

export default function NotePage() {
  const params = useParams<{ id: string }>();
  const nodeId = params.id;
  const me = useMe();
  const [access, setAccess] = React.useState<CollabAccess | null | "notfound" | "loading">(
    "loading",
  );
  const [status, setStatus] = React.useState<CollabStatus>("connecting");
  const [synced, setSynced] = React.useState(false);
  const [chatOpen, setChatOpen] = React.useState(false);
  // ヘッダスロットへ渡す安定参照（毎レンダーの再注入を避ける）。
  const toggleChat = React.useCallback(() => setChatOpen((v) => !v), []);

  // Yjs ドキュメントとプロバイダ（ノート単位で 1 つ・アンマウントで破棄）。
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

  if (access === "loading" || me.loading) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        ノートを開いています…
      </div>
    );
  }
  if (access === "notfound" || access === null) {
    return (
      <EmptyState
        title="ノートが見つかりません"
        description="削除されたか、アクセス権がありません。"
      />
    );
  }

  const editable = access.mode === "editor";
  const userId = me.data?.id ?? "unknown";
  const userName = me.data?.email?.split("@")[0] ?? userId;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* 統一ヘッダへ注入（横バー二重を解消・null を返すだけ） */}
      <NoteSyncSlot
        name={access.name.replace(/\.md$/i, "")}
        editable={editable}
        status={status}
        synced={synced}
        chatOpen={chatOpen}
        onToggleChat={toggleChat}
        provider={session?.provider ?? null}
      />

      {/* 分割ビュー: 本ページが一元ホスト（Conversation を再利用・実装は一箇所） */}
      <div className="flex min-h-0 flex-1">
        {session && (
          <div className="min-w-0 flex-1 overflow-y-auto">
            <div className="mx-auto max-w-3xl px-4 pb-24 pt-4">
              <MetadataPanel meta={session.doc.getMap("meta")} editable={editable} />
              <div className="mt-4">
                <NoteEditor
                  provider={session.provider}
                  editable={editable}
                  user={{ id: userId, name: userName }}
                  extraSlashItems={embedSlashItems}
                />
              </div>
            </div>
          </div>
        )}
        {session && chatOpen && (
          <aside className="flex w-full min-w-0 max-w-md shrink-0 flex-col border-l border-sidebar-border bg-sidebar/40 md:w-[440px]">
            {/* 分割ビューのチャット: パネル自前ヘッダ＋幅いっぱいの会話（variant=panel） */}
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
                noteName={access.name}
                editable={editable}
              />
            </div>
          </aside>
        )}
      </div>
    </div>
  );
}
