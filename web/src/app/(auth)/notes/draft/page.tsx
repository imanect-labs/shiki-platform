"use client";

/// 下書きノート画面（issue #282）。/notes/draft?thread=&name= で未保存の下書きを詰める。
///
/// - 下書きは**クライアント内のみ**（サーバ未作成・[`draft-store`] が真実源）。チャットで
///   「ドキュメントを作って」→ save_note が note_draft を返し、本画面へ遷移して本文を用意する。
/// - 同じ会話の複数下書きは**上部タブ**で切替（別名 = 別下書き・N 本並存）。
/// - 右上「保存」（PC は「ドライブに保存」）で POST /notes → 実体化し、その会話をノートへ紐付けて /notes/{id} へ。
/// - アシスタント（同じ会話）を同席させ、保存前に AI と詰められる（流し込みで再シード）。

import { FileWarning } from "lucide-react";
import { useRouter, useSearchParams } from "next/navigation";
import * as React from "react";
import type { Editor } from "@tiptap/react";

import { Conversation } from "@/components/chat/conversation";
import { DraftNoteEditor } from "@/components/notes/draft-note-editor";
import {
  DraftBadge,
  DraftNotFound,
  DraftTabs,
  SaveDraftDialog,
  draftChrome,
  type SaveTarget,
} from "@/components/notes/save-draft-dialog";
import { AssistantLayout } from "@/components/shell/assistant-sheet";
import { EditorMobileHeading, useEditorBack } from "@/components/shell/editor-loading";
import { usePageChrome } from "@/components/shell/page-header-context";
import { toast } from "@/components/ui/use-toast";
import { getThreadMessages, setThreadOriginNote } from "@/lib/chat-api";
import { createNote } from "@/lib/notes-api";
import {
  getDraft,
  listDrafts,
  parseNoteDraft,
  removeDraft,
  upsertDraft,
  useDrafts,
} from "@/lib/notes/draft-store";
import { serializeFragment } from "@/lib/notes/markdown-serialize";
import { useStorageScopeReady } from "@/lib/user-storage";

/// useSearchParams を使うため Suspense 境界でラップする（静的生成の CSR bailout 要件・Next.js）。
export default function DraftNotePage() {
  return (
    <React.Suspense fallback={<div className="h-full" />}>
      <DraftNotePageInner />
    </React.Suspense>
  );
}

function DraftNotePageInner() {
  const searchParams = useSearchParams();
  const router = useRouter();
  const threadId = searchParams.get("thread") ?? "";
  const nameParam = searchParams.get("name") ?? "";

  const drafts = useDrafts(threadId);
  const [activeName, setActiveName] = React.useState(nameParam);
  const [seed, setSeed] = React.useState<{ markdown: string; nonce: number }>({
    markdown: "",
    nonce: 0,
  });
  const nonceRef = React.useRef(0);
  const [chatOpen, setChatOpen] = React.useState(true);
  const [saveOpen, setSaveOpen] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const [recovered, setRecovered] = React.useState(false);
  const editorRef = React.useRef<Editor | null>(null);

  // 対象の下書きを開く（seed の nonce を進めて再シードを発火）。手編集では呼ばない。
  const openDraft = React.useCallback(
    (name: string) => {
      nonceRef.current += 1;
      setActiveName(name);
      setSeed({ markdown: getDraft(threadId, name)?.content ?? "", nonce: nonceRef.current });
    },
    [threadId],
  );

  const scopeReady = useStorageScopeReady();
  // 会話履歴の note_draft ブロックから復元する（リロード/別端末・#282）。ローカルに一部だけ
  // 残っている場合も履歴を常に取得し、**未登録の (threadId, name) のみ**追加する
  // （ローカルのユーザー編集は上書きしない・スライド下書きと同型）。
  React.useEffect(() => {
    if (!threadId) {
      setRecovered(true);
      return;
    }
    // ユーザー確定（/me）前の get() は空のメモリを見て保存済みの下書きを「未登録」と誤判定し、
    // AI 出力で開いてしまう（手編集が画面から消え、次の編集で上書きされる）ので待つ（#817）。
    if (!scopeReady) return;
    let cancelled = false;
    getThreadMessages(threadId)
      .then(({ messages }) => {
        if (cancelled) return;
        for (const m of messages) {
          for (const b of m.content) {
            if (b.type === "note_draft") {
              const d = parseNoteDraft(b.draft);
              if (d && !getDraft(threadId, d.name)) {
                upsertDraft(threadId, d.name, d.markdown, "ai");
              }
            }
          }
        }
      })
      // 履歴を読めなくても（会話が消えた等）ローカルの下書きで開く。
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setRecovered(true);
      });
    return () => {
      cancelled = true;
    };
  }, [threadId, scopeReady]);

  // 対象下書きをアクティブへ（?name 優先・無ければ最新）。復元完了後、**nameParam ごとに一度**開く
  // （同じ ?name の再処理は抑止しつつ、?name が変わる deep link 切替では再オープンする）。
  const openedForRef = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (!recovered) return;
    const list = listDrafts(threadId);
    if (list.length === 0) return;
    const key = nameParam || "__latest__";
    if (openedForRef.current === key) return;
    openedForRef.current = key;
    const target =
      (nameParam && list.find((d) => d.name === nameParam)?.name) ??
      list[list.length - 1].name;
    openDraft(target);
  }, [recovered, threadId, nameParam, openDraft]);

  // 手編集は下書きストアへ書き戻す（source=user＝再シードしない）。
  const onChangeMarkdown = React.useCallback(
    (markdown: string) => {
      if (activeName) upsertDraft(threadId, activeName, markdown, "user");
    },
    [threadId, activeName],
  );

  // アシスタントが同じ会話で流し込んだとき（note_draft）は、その下書きをアクティブにして再シード。
  const onNoteDraftOpened = React.useCallback((name: string) => openDraft(name), [openDraft]);

  const handleEditorReady = React.useCallback((e: Editor | null) => {
    editorRef.current = e;
  }, []);

  const doSave = React.useCallback(
    (target: SaveTarget) => {
      if (!activeName || saving) return;
      setSaving(true);
      const markdown = editorRef.current
        ? serializeFragment(editorRef.current.state.doc.content)
        : (getDraft(threadId, activeName)?.content ?? "");
      createNote({ parentId: target.parentId, name: target.name, markdown })
        .then(async (node) => {
          // この会話をノートへ紐付けて「ノート由来」にする（best-effort・失敗しても保存は成立）。
          if (threadId) await setThreadOriginNote(threadId, node.id).catch(() => {});
          removeDraft(threadId, activeName);
          toast({ description: `「${node.name.replace(/\.md$/i, "")}」を保存しました。` });
          // 実体化したノートを、この会話をアクティブにして開く（通常 collab へ昇格）。
          router.replace(threadId ? `/notes/${node.id}?thread=${threadId}` : `/notes/${node.id}`);
        })
        .catch(() => {
          toast({ description: "保存に失敗しました。" });
          setSaving(false);
        });
    },
    [activeName, saving, threadId, router],
  );

  // クローム（戻る → 保存 → アシスタント → ⋯）。下書きが無い画面でも戻るを出す。
  const back = useEditorBack();
  usePageChrome(
    () =>
      draftChrome({
        back,
        threadId,
        name: activeName || "下書きノート",
        canSave: Boolean(activeName),
        onSave: () => setSaveOpen(true),
        chatOpen,
        onToggleChat: () => setChatOpen((v) => !v),
      }),
    [back, threadId, activeName, chatOpen],
  );

  if (!threadId) {
    return <DraftNotFound what="ドキュメント" />;
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <AssistantLayout
        open={chatOpen}
        onClose={() => setChatOpen(false)}
        label="ノートのアシスタント"
        panel={
          // 下書き画面では遷移せずアクティブ下書きを切替（onNoteDraftOpened）。
          <Conversation threadId={threadId} variant="panel" onNoteDraftOpened={onNoteDraftOpened} />
        }
      >
        <div className="min-h-0 min-w-0 flex-1 overflow-y-auto overscroll-y-contain">
          <div className="mx-auto max-w-3xl px-4 pb-24 pt-4 max-md:pt-1">
            {/* スマホ: 本文の大きな題名（先頭の見出し）の上に小さく状態行。 */}
            <EditorMobileHeading className="px-2 pb-1 pt-0" status={<DraftBadge />} />
            {/* 下書きタブ（同じ会話の複数下書きを切替・別名 = 別下書き）。 */}
            <DraftTabs
              drafts={drafts}
              activeName={activeName}
              onOpen={openDraft}
              className="mb-3"
            />

            {activeName ? (
              <DraftNoteEditor
                key={activeName}
                seed={seed}
                onChangeMarkdown={onChangeMarkdown}
                onReady={handleEditorReady}
              />
            ) : (
              <div className="flex min-h-[40vh] flex-col items-center justify-center gap-2 text-center text-sm text-muted-foreground">
                <FileWarning className="size-6 text-muted-foreground/60" aria-hidden />
                下書きを読み込んでいます…
              </div>
            )}
          </div>
        </div>
      </AssistantLayout>

      <SaveDraftDialog
        open={saveOpen}
        onOpenChange={setSaveOpen}
        defaultName={activeName}
        saving={saving}
        onConfirm={doSave}
      />
    </div>
  );
}
