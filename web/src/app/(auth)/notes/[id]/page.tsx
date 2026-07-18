"use client";

/// ノートページ（Task 11P.3）。/notes/[id] で .md ファイルを共同編集する。
///
/// - 接続は CollabProvider（BFF セッション Cookie・y-websocket 互換ワイヤ）。
/// - 権限はサーバが強制する（viewer の update 不受理・剥奪切断）。ここでは
///   /collab/docs/{id}/access の表示用ヒントで editable を切り替える。
/// - 11P.5 で本ページが「ノート×チャット分割ビュー」のホストになる。

import { Loader2, MessageSquare, X } from "lucide-react";
import Link from "next/link";
import { useParams, useSearchParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { embedSlashItems } from "@/components/notes/embed/embed-slash-items";
import { MetadataPanel } from "@/components/notes/metadata-panel";
import { NoteChatPanel } from "@/components/notes/note-chat-panel";
import { NoteEditor } from "@/components/notes/note-editor";
import { NoteSyncSlot } from "@/components/notes/note-header-slot";
import { EmptyState } from "@/components/ui/empty-state";
import { FadeSlide } from "@/components/ui/motion-primitives";
import { useMe } from "@/hooks/use-me";
import { CollabProvider, type CollabStatus } from "@/lib/collab";
import { setPendingSelection } from "@/lib/selection-context";
import { getCollabAccess, type CollabAccess } from "@/lib/notes-api";
import { cn } from "@/lib/utils";

/// useSearchParams を使うため Suspense 境界でラップする（App Router の CSR bailout 対策・#282）。
export default function NotePage() {
  return (
    <React.Suspense
      fallback={
        <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" aria-hidden />
          ノートを開いています…
        </div>
      }
    >
      <NotePageInner />
    </React.Suspense>
  );
}

function NotePageInner() {
  const params = useParams<{ id: string }>();
  const nodeId = params.id;
  const searchParams = useSearchParams();
  // サイドバーの「ノート由来」履歴から来たときは、その会話を開く（?thread=）。
  const initialThreadId = searchParams.get("thread");
  const me = useMe();
  const [access, setAccess] = React.useState<CollabAccess | null | "notfound" | "loading">(
    "loading",
  );
  const [status, setStatus] = React.useState<CollabStatus>("connecting");
  const [synced, setSynced] = React.useState(false);
  // ?thread= 指定時はアシスタントを開いた状態で見せる（その会話を辿るのが目的のため）。
  const [chatOpen, setChatOpen] = React.useState(Boolean(initialThreadId));
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
    // ノート由来履歴（?thread=）から来てノートが読めない（削除/権限剥奪）場合でも、会話自体が
    // 閲覧可能なら通常のチャットへ辿れるようにする（履歴からの導線を失わせない・#282）。
    return (
      <EmptyState
        title="ノートが見つかりません"
        description="削除されたか、アクセス権がありません。"
        action={
          initialThreadId ? (
            <Link
              href={`/c/${initialThreadId}`}
              className="inline-flex items-center gap-1.5 rounded-full border px-3.5 py-1.5 text-sm font-medium transition-colors hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <MessageSquare className="size-4" aria-hidden />
              この会話をチャットで開く
            </Link>
          ) : undefined
        }
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

      {/* 分割ビュー: 本ページが一元ホスト（Conversation を再利用・実装は一箇所）。
          アシスタントは「きっかけ」のように浮遊した角丸カード（本文の右側に重ねる）。 */}
      <div className="relative flex min-h-0 flex-1">
        {session && (
          <div
            className={cn(
              "min-w-0 flex-1 overflow-y-auto transition-[padding] duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
              // アシスタント表示中は本文を「パネルを除いた領域」の中央へ寄せる（右寄り解消）。
              chatOpen && "lg:pr-[28rem]",
            )}
          >
            <div className="mx-auto max-w-3xl px-4 pb-24 pt-4">
              <MetadataPanel meta={session.doc.getMap("meta")} editable={editable} />
              <div className="mt-4">
                <NoteEditor
                  provider={session.provider}
                  editable={editable}
                  user={{ id: userId, name: userName }}
                  extraSlashItems={embedSlashItems}
                  // 選択→AI 指示（Task 11.10）: 選択をチップ化してアシスタントを開く。
                  onAskAi={({ text, headingPath }) => {
                    setPendingSelection({
                      kind: "note_selection",
                      node_id: nodeId,
                      excerpt: text,
                      locator: { heading_path: headingPath },
                    });
                    setChatOpen(true);
                  }}
                />
              </div>
            </div>
          </div>
        )}
        {session && chatOpen && (
          <FadeSlide
            from="right"
            role="complementary"
            aria-label="ノートのアシスタント"
            className="absolute inset-y-3 right-3 z-20 flex w-[min(420px,calc(100%-1.5rem))] flex-col overflow-hidden rounded-2xl border bg-card shadow-lg"
          >
            {/* 浮遊カードの自前ヘッダ＋幅いっぱいの会話（variant=panel） */}
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
                initialThreadId={initialThreadId}
              />
            </div>
          </FadeSlide>
        )}
      </div>
    </div>
  );
}
