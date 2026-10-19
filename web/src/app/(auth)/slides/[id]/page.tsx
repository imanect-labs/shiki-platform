"use client";

/// スライドページ（Task 11.1）。/slides/[id] で .slide ファイルを表示する。
///
/// - 接続は CollabProvider（BFF セッション Cookie・y-websocket 互換ワイヤ）。真実は Yjs。
/// - 本 PR は閲覧ビューのみ。編集（GrapesJS 砂箱エディタ・Task 11.2）は本ページの
///   editable 分岐に載る。描画は SlideFrame（DOMPurify＋sandbox iframe・PIT-40）。

import { Eye, FileWarning } from "lucide-react";
import { useParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { NoteChatPanel } from "@/components/notes/note-chat-panel";
import { SyncPill } from "@/components/notes/note-status";
import { PresenceAvatars } from "@/components/notes/presence";
import { AssistantLayout } from "@/components/shell/assistant-sheet";
import {
  EditorLoading,
  EditorMobileHeading,
  EditorStatusBadge,
  EditorUnavailable,
} from "@/components/shell/editor-loading";
import { useSlideChrome } from "@/components/slides/slide-header-slot";
import { SlideWorkspace, useSlideOverlays } from "@/components/slides/slide-workspace";
import { ShareLinkUnlock, unlockTokenFromUrl } from "@/components/share/share-link-unlock";
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

  const overlays = useSlideOverlays();
  const ready = typeof access === "object" && access !== null;
  const displayName = ready ? access.name.replace(/\.slide$/i, "") : "";
  const editable = ready && access.mode === "editor";
  // クロームはページ最上位で登録する（読み込み中・notfound でも戻る導線を残す）。
  useSlideChrome(
    ready
      ? {
          name: displayName,
          editable,
          status,
          synced,
          provider: session?.provider ?? null,
          chatOpen,
          onToggleChat: openAssistant,
          onExport: overlays.onExport,
          onPresent: overlays.onPresent,
        }
      : null,
  );

  if (access === "loading") {
    return <EditorLoading kind="slide" message="スライドを開いています…" />;
  }
  if (access === "notfound" || access === null) {
    return (
      <EditorUnavailable
        icon={FileWarning}
        title="スライドが見つかりません"
        description="削除されたか、アクセス権がありません。パスワード付き共有リンクの場合はパスワードで開けます。"
      >
        {unlockTokenFromUrl() ? (
          <ShareLinkUnlock token={unlockTokenFromUrl()!} onUnlocked={loadAccess} autoFocus />
        ) : null}
      </EditorUnavailable>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* スマホ: 文書名＋状態（PC はバーに出す）。 */}
      <EditorMobileHeading
        name={displayName}
        status={
          <>
            {!editable ? <EditorStatusBadge icon={Eye}>閲覧のみ</EditorStatusBadge> : null}
            <SyncPill status={status} synced={synced} showSynced testId={null} />
            {session ? (
              <PresenceAvatars provider={session.provider} max={1} testId={null} />
            ) : null}
          </>
        }
      />
      {/* アシスタント（ノートページと同型・meta active_thread_id 共用）。 */}
      <AssistantLayout
        open={Boolean(session) && chatOpen}
        onClose={() => setChatOpen(false)}
        label="スライドのアシスタント"
        panel={
          session ? (
            <NoteChatPanel
              meta={session.doc.getMap("meta")}
              noteId={nodeId}
              noteName={access.name}
              editable={editable}
              initialThreadId={null}
            />
          ) : null
        }
      >
        <div className="min-h-0 flex-1">
          {session && synced ? (
            <SlideWorkspace
              doc={session.doc}
              editable={editable}
              name={displayName}
              // 選択→AI 指示（Task 11.10）: パネルが開いていれば選択要素を自動挿入する。
              onSelectionChange={handleSelectionChange}
              presenting={overlays.presenting}
              onPresentingChange={overlays.setPresenting}
              exporting={overlays.exporting}
              onExportingChange={overlays.setExporting}
            />
          ) : (
            <EditorLoading kind="slide" message="同期しています…" />
          )}
        </div>
      </AssistantLayout>
    </div>
  );
}
