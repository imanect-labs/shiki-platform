"use client";

/// ノートページ（Task 11P.3）。/notes/[id] で .md ファイルを共同編集する。
///
/// - 接続は CollabProvider（BFF セッション Cookie・y-websocket 互換ワイヤ）。
/// - 権限はサーバが強制する（viewer の update 不受理・剥奪切断）。ここでは
///   /collab/docs/{id}/access の表示用ヒントで editable を切り替える。
/// - 11P.5 で本ページが「ノート×チャット分割ビュー」のホストになる。

import { Eye, FileWarning, MessageSquare } from "lucide-react";
import Link from "next/link";
import { useParams, useSearchParams } from "next/navigation";
import * as React from "react";
import type { Editor } from "@tiptap/react";
import * as Y from "yjs";

import { embedSlashItems } from "@/components/notes/embed/embed-slash-items";
import { useCiteHighlight } from "@/components/notes/cite-highlight";
import { useCiteLocator } from "@/lib/citation-locate";
import { CiteHint, shortPhrase } from "@/components/shell/cite-hint";
import { MetadataPanel } from "@/components/notes/metadata-panel";
import { NoteChatPanel } from "@/components/notes/note-chat-panel";
import { NoteEditor } from "@/components/notes/note-editor";
import { useNoteChrome } from "@/components/notes/note-header-slot";
import { SyncPill } from "@/components/notes/note-status";
import { PresenceAvatars } from "@/components/notes/presence";
import { AssistantLayout } from "@/components/shell/assistant-sheet";
import {
  EditorLoading,
  EditorMobileHeading,
  EditorStatusBadge,
  EditorUnavailable,
} from "@/components/shell/editor-loading";
import { ShareLinkUnlock } from "@/components/share/share-link-unlock";
import { Button } from "@/components/ui/button";
import { useDocThreadDeepLink } from "@/hooks/use-doc-thread-deep-link";
import { useMe } from "@/hooks/use-me";
import { CollabProvider, type CollabStatus } from "@/lib/collab";
import { setPendingSelection } from "@/lib/selection-context";
import { getCollabAccess, type CollabAccess } from "@/lib/notes-api";

/// useSearchParams を使うため Suspense 境界でラップする（App Router の CSR bailout 対策・#282）。
export default function NotePage() {
  return (
    <React.Suspense
      fallback={
        <EditorLoading kind="doc" message="ノートを開いています…" />
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
  // 本文エディタ（エクスポートの md 直列化に使う・#334）。
  const [editor, setEditor] = React.useState<Editor | null>(null);
  // ?thread= 指定時はアシスタントを開いた状態で見せる（その会話を辿るのが目的のため・#598）。
  const { chatOpen, setChatOpen, deepLinkThreadId } = useDocThreadDeepLink(initialThreadId);
  // 引用箇所から開いたとき（?cite=）: その一節をハイライトしてスクロールする（#508）。
  // 一節そのものは URL に載せず、クリック時に置かれた手がかりを読む。
  const locator = useCiteLocator(searchParams.get("cite"));
  const cite = useCiteHighlight(editor, synced, {
    find: locator?.find ?? null,
    end: locator?.end ?? null,
    heading: locator?.heading ?? null,
  });
  // 直近の選択（ヘッダの「AI に依頼」で開いた瞬間に挿入する材料）。
  const latestSelRef = React.useRef<{ text: string; headingPath: string[] } | null>(null);

  const insertSelection = React.useCallback(
    (sel: { text: string; headingPath: string[] }) => {
      setPendingSelection({
        kind: "note_selection",
        node_id: nodeId,
        excerpt: sel.text,
        locator: { heading_path: sel.headingPath },
      });
    },
    [nodeId],
  );

  // 選択の変化を受ける: パネルが開いていれば自動でチャットへ挿入する。
  const handleSelectionChange = React.useCallback(
    (sel: { text: string; headingPath: string[] } | null) => {
      latestSelRef.current = sel;
      setChatOpen((open) => {
        if (open && sel) insertSelection(sel);
        return open;
      });
    },
    [insertSelection, setChatOpen],
  );

  // 「AI に依頼」= アシスタントパネルを開く（＋その時の選択があればチャットへ挿入）。
  const openAssistant = React.useCallback(() => {
    setChatOpen((open) => {
      if (!open && latestSelRef.current) insertSelection(latestSelRef.current);
      return !open;
    });
  }, [insertSelection, setChatOpen]);

  // Yjs ドキュメントとプロバイダ（ノート単位で 1 つ・アンマウントで破棄）。
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

  const ready = typeof access === "object" && access !== null && !me.loading;
  // 拡張子を落としたファイル名（バー・タイトル欄のフォールバックで共有する表示名）。
  const displayName = ready ? access.name.replace(/\.md$/i, "") : "";
  const editable = ready && access.mode === "editor";
  // クロームはページ最上位で登録する（読み込み中・notfound でも戻る導線を残す）。
  const shareDialog = useNoteChrome(
    nodeId,
    ready
      ? {
          name: displayName,
          editable,
          status,
          synced,
          chatOpen,
          onToggleChat: openAssistant,
          provider: session?.provider ?? null,
          editor,
        }
      : null,
  );

  if (access === "loading" || me.loading) {
    return <EditorLoading kind="doc" message="ノートを開いています…" />;
  }
  if (access === "notfound" || access === null) {
    // ノート由来履歴（?thread=）から来てノートが読めない（削除/権限剥奪）場合でも、会話自体が
    // 閲覧可能なら通常のチャットへ辿れるようにする（履歴からの導線を失わせない・#282）。
    return (
      <EditorUnavailable
        icon={FileWarning}
        title="ノートが見つかりません"
        description="削除されたか、アクセス権がありません。パスワード付き共有リンクの場合はパスワードで開けます。"
        actions={
          initialThreadId ? (
            <Button asChild>
              <Link href={`/c/${initialThreadId}`}>
                <MessageSquare aria-hidden />
                この会話をチャットで開く
              </Link>
            </Button>
          ) : undefined
        }
      >
        {/* パスワード付き共有リンクの解錠（#342）。?lt=<token> がある時のみ表示。 */}
        {searchParams.get("lt") ? (
          <ShareLinkUnlock token={searchParams.get("lt")!} onUnlocked={loadAccess} autoFocus />
        ) : null}
      </EditorUnavailable>
    );
  }

  const userId = me.data?.id ?? "unknown";
  const userName = me.data?.email?.split("@")[0] ?? userId;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {shareDialog}
      {/* 分割ビュー: 本ページが一元ホスト（Conversation を再利用・実装は一箇所）。
          アシスタントは PC では本文の右に浮かぶカード、スマホでは下からのシート。 */}
      <AssistantLayout
        open={Boolean(session) && chatOpen}
        onClose={() => setChatOpen(false)}
        label="ノートのアシスタント"
        overlay={
          cite ? (
            <CiteHint tone={cite.status === "found" ? "found" : cite.status === "heading" ? "approx" : "missing"}>
              {cite.status === "found"
                ? `引用箇所「${shortPhrase(cite.phrase)}」をハイライトしています`
                : cite.status === "heading"
                  ? `該当の一節は編集されています。見出し「${shortPhrase(cite.heading)}」へ移動しました`
                  : "引用した一節が見つかりませんでした（編集・削除された可能性があります）"}
            </CiteHint>
          ) : null
        }
        panel={
          session ? (
            <NoteChatPanel
              meta={session.doc.getMap("meta")}
              noteId={nodeId}
              noteName={access.name}
              editable={editable}
              initialThreadId={deepLinkThreadId}
            />
          ) : null
        }
      >
        {session && (
          <div className="min-h-0 min-w-0 flex-1 overflow-y-auto overscroll-y-contain">
            <div className="mx-auto max-w-3xl px-4 pb-24 pt-4 max-md:pt-1">
              {/* スマホ: 大きな題名の上に小さく状態行（PC はバーに出す）。 */}
              <EditorMobileHeading
                className="px-2 pb-1 pt-0"
                status={
                  <>
                    {!editable ? (
                      <EditorStatusBadge icon={Eye}>閲覧のみ</EditorStatusBadge>
                    ) : null}
                    <SyncPill status={status} synced={synced} showSynced testId={null} />
                    <PresenceAvatars provider={session.provider} max={1} testId={null} />
                  </>
                }
              />
              <MetadataPanel
                meta={session.doc.getMap("meta")}
                editable={editable}
                fallbackTitle={displayName}
              />
              <div className="mt-4">
                <NoteEditor
                  provider={session.provider}
                  editable={editable}
                  user={{ id: userId, name: userName }}
                  extraSlashItems={embedSlashItems}
                  // 選択→AI 指示（Task 11.10）: パネルが開いていれば選択を自動挿入する。
                  onSelectionChange={handleSelectionChange}
                  onEditorReady={setEditor}
                />
              </div>
            </div>
          </div>
        )}
      </AssistantLayout>
    </div>
  );
}
