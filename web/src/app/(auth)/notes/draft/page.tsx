"use client";

/// 下書きノート画面（issue #282）。/notes/draft?thread=&name= で未保存の下書きを詰める。
///
/// - 下書きは**クライアント内のみ**（サーバ未作成・[`draft-store`] が真実源）。チャットで
///   「ドキュメントを作って」→ save_note が note_draft を返し、本画面へ遷移して本文を用意する。
/// - 同じ会話の複数下書きは**上部タブ**で切替（別名 = 別下書き・N 本並存）。
/// - 右上「ドライブに保存」で POST /notes → 実体化し、その会話をノートへ紐付けて /notes/{id} へ。
/// - アシスタント（同じ会話）を同席させ、保存前に AI と詰められる（流し込みで再シード）。

import { ArrowLeft, FileWarning, MessageSquare, PencilLine, Save, X } from "lucide-react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import * as React from "react";
import type { Editor } from "@tiptap/react";

import { Conversation } from "@/components/chat/conversation";
import { DraftNoteEditor } from "@/components/notes/draft-note-editor";
import { SaveDraftDialog, type SaveTarget } from "@/components/notes/save-draft-dialog";
import { usePageHeader } from "@/components/shell/page-header-context";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { FadeSlide } from "@/components/ui/motion-primitives";
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
import { cn } from "@/lib/utils";

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
      setSeed({ markdown: getDraft(threadId, name)?.markdown ?? "", nonce: nonceRef.current });
    },
    [threadId],
  );

  // 下書きストアが空なら、会話履歴の note_draft ブロックから復元する（リロード/別端末・#282）。
  React.useEffect(() => {
    if (!threadId) {
      setRecovered(true);
      return;
    }
    if (listDrafts(threadId).length > 0) {
      setRecovered(true);
      return;
    }
    let cancelled = false;
    getThreadMessages(threadId)
      .then(({ messages }) => {
        if (cancelled) return;
        for (const m of messages) {
          for (const b of m.content) {
            if (b.type === "note_draft") {
              const d = parseNoteDraft(b.draft);
              if (d) upsertDraft(threadId, d.name, d.markdown, "ai");
            }
          }
        }
      })
      .finally(() => {
        if (!cancelled) setRecovered(true);
      });
    return () => {
      cancelled = true;
    };
  }, [threadId]);

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
        : (getDraft(threadId, activeName)?.markdown ?? "");
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

  // 統一ヘッダへ注入（戻る/下書きバッジ/保存/アシスタント切替）。
  usePageHeader(
    () => (
      <div className="flex min-w-0 flex-1 items-center gap-2.5">
        <Link
          href="/drive"
          className="flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground active:scale-95"
          aria-label="ドライブへ戻る"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <span className="min-w-0 flex-1 truncate text-sm font-medium text-foreground">
          {activeName || "下書きノート"}
        </span>
        <span
          className="inline-flex items-center gap-1 rounded-full border border-dashed border-amber-500/50 bg-amber-500/10 px-2.5 py-0.5 text-xs font-medium text-amber-600 dark:text-amber-400"
          data-testid="draft-badge"
        >
          <PencilLine className="size-3.5" aria-hidden />
          下書き（未保存）
        </span>
        <Button
          type="button"
          size="sm"
          onClick={() => setSaveOpen(true)}
          disabled={!activeName}
          data-testid="draft-save-button"
        >
          <Save className="mr-1.5 size-4" aria-hidden />
          ドライブに保存
        </Button>
        <Button
          type="button"
          variant={chatOpen ? "secondary" : "ghost"}
          size="sm"
          onClick={() => setChatOpen((v) => !v)}
          aria-pressed={chatOpen}
          data-testid="note-chat-toggle"
        >
          <MessageSquare className="mr-1.5 size-4" aria-hidden />
          アシスタント
        </Button>
      </div>
    ),
    [activeName, chatOpen],
  );

  if (!threadId) {
    return (
      <EmptyState
        title="下書きが見つかりません"
        description="チャットから「ドキュメントを作って」と頼むと、ここに下書きが用意されます。"
      />
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="relative flex min-h-0 flex-1">
        <div
          className={cn(
            "min-w-0 flex-1 overflow-y-auto transition-[padding] duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
            chatOpen && "lg:pr-[28rem]",
          )}
        >
          <div className="mx-auto max-w-3xl px-4 pb-24 pt-4">
            {/* 下書きタブ（同じ会話の複数下書きを切替・別名 = 別下書き）。 */}
            {drafts.length > 1 ? (
              <div
                className="mb-3 flex flex-wrap items-center gap-1.5"
                data-testid="draft-tabs"
              >
                {drafts.map((d) => (
                  <button
                    key={d.name}
                    type="button"
                    onClick={() => openDraft(d.name)}
                    className={cn(
                      "inline-flex items-center gap-1.5 rounded-full border px-3 py-1 text-[13px] transition-colors",
                      d.name === activeName
                        ? "border-primary/40 bg-secondary font-medium text-foreground"
                        : "border-border/60 bg-card/40 text-muted-foreground hover:bg-secondary hover:text-foreground",
                    )}
                  >
                    <PencilLine className="size-3.5 shrink-0" aria-hidden />
                    <span className="max-w-[12rem] truncate">{d.name}</span>
                  </button>
                ))}
              </div>
            ) : null}

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

        {chatOpen && (
          <FadeSlide
            from="right"
            role="complementary"
            aria-label="ノートのアシスタント"
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
              {/* 下書き画面では遷移せずアクティブ下書きを切替（onNoteDraftOpened）。 */}
              <Conversation
                threadId={threadId}
                variant="panel"
                onNoteDraftOpened={onNoteDraftOpened}
              />
            </div>
          </FadeSlide>
        )}
      </div>

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
