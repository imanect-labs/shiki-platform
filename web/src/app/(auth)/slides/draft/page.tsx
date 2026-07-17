"use client";

/// 下書きスライド画面（Task 11.3）。/slides/draft?thread=&name= で未保存の下書きを詰める。
///
/// - 下書きは**クライアント内のみ**（サーバ未作成・[`slideDraftStore`] が真実源）。チャットで
///   「パワポを作って」→ save_slide が slide_draft を返し、本画面へ遷移して内容を用意する。
/// - 左はローカル Y.Doc に流し込んだ [`SlideWorkspace`]（editable・サーバ collab 非接続）。
///   編集は Y.Doc → 下書きストアへ書き戻す（リロード復元）。
/// - 同じ会話の複数下書きは**上部タブ**で切替（別名 = 別下書き・N 本並存）。
/// - 右上「ドライブに保存」で POST /slides → 実体化して /slides/{id} へ（ノート下書きと同型）。

import { ArrowLeft, FileWarning, MessageSquare, PencilLine, Save, X } from "lucide-react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { Conversation } from "@/components/chat/conversation";
import { SaveDraftDialog, type SaveTarget } from "@/components/notes/save-draft-dialog";
import { usePageHeader } from "@/components/shell/page-header-context";
import { SlideWorkspace } from "@/components/slides/slide-workspace";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { FadeSlide } from "@/components/ui/motion-primitives";
import { toast } from "@/components/ui/use-toast";
import { getThreadMessages } from "@/lib/chat-api";
import { createSlide } from "@/lib/slides-api";
import { parseSlideDraft, slideDraftStore } from "@/lib/slides/draft";
import { parseSlideDocJson, readSlidesJson, seedSlides } from "@/lib/slides-doc";
import { cn } from "@/lib/utils";

/// useSearchParams を使うため Suspense 境界でラップする（静的生成の CSR bailout 要件・Next.js）。
export default function DraftSlidePage() {
  return (
    <React.Suspense fallback={<div className="h-full" />}>
      <DraftSlidePageInner />
    </React.Suspense>
  );
}

function DraftSlidePageInner() {
  const searchParams = useSearchParams();
  const router = useRouter();
  const threadId = searchParams.get("thread") ?? "";
  const nameParam = searchParams.get("name") ?? "";

  const drafts = slideDraftStore.useDrafts(threadId);
  const [activeName, setActiveName] = React.useState(nameParam);
  const [chatOpen, setChatOpen] = React.useState(true);
  const [saveOpen, setSaveOpen] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const [recovered, setRecovered] = React.useState(false);
  // ローカル Y.Doc（サーバ collab 非接続・SlideWorkspace のホスト）。下書き切替で作り直す。
  const [doc, setDoc] = React.useState<Y.Doc | null>(null);
  const docRef = React.useRef<Y.Doc | null>(null);
  // アクティブ下書きのメタ（title/theme_id 等）。SlideWorkspace は slides のみ編集するため、
  // メタは seed 時に保持して保存時にそのまま content へ戻す。
  const metaRef = React.useRef<Record<string, unknown>>({});
  // 自分が Y.Doc へ seed / 書き戻した直近のスライド JSON（エコーでの rev 空回りを抑止）。
  const lastSlidesRef = React.useRef<string>("");

  // 対象の下書きを開く（新しい Y.Doc に content を流し込む）。手編集では呼ばない。
  const openDraft = React.useCallback(
    (name: string) => {
      setActiveName(name);
      const content = slideDraftStore.get(threadId, name)?.content ?? "";
      const parsed = parseSlideDocJson(content);
      metaRef.current = parsed?.meta ?? {};
      const next = new Y.Doc();
      seedSlides(next, parsed?.slides ?? []);
      lastSlidesRef.current = JSON.stringify(readSlidesJson(next));
      docRef.current?.destroy();
      docRef.current = next;
      setDoc(next);
    },
    [threadId],
  );

  // アンマウント時に現在の Y.Doc を破棄する（切替時の破棄は openDraft が行う）。
  React.useEffect(() => () => docRef.current?.destroy(), []);

  // 手編集（Y.Doc の変化）は下書きストアへ書き戻す（source=user＝再シードしない）。
  React.useEffect(() => {
    if (!doc || !activeName) return;
    const arr = doc.getArray("slides");
    const onChange = () => {
      const slides = readSlidesJson(doc);
      const key = JSON.stringify(slides);
      if (key === lastSlidesRef.current) return;
      lastSlidesRef.current = key;
      slideDraftStore.upsert(
        threadId,
        activeName,
        JSON.stringify({ version: 1, meta: metaRef.current, slides }),
        "user",
      );
    };
    arr.observeDeep(onChange);
    return () => arr.unobserveDeep(onChange);
  }, [doc, threadId, activeName]);

  // 会話履歴の slide_draft ブロックから復元する（リロード/別端末）。ローカルに一部だけ
  // 残っている場合も履歴を常に取得し、**未登録の (threadId, name) のみ**追加する
  // （ローカルのユーザー編集は上書きしない・レビュー指摘対応）。
  React.useEffect(() => {
    if (!threadId) {
      setRecovered(true);
      return;
    }
    let cancelled = false;
    getThreadMessages(threadId)
      .then(({ messages }) => {
        if (cancelled) return;
        for (const m of messages) {
          for (const b of m.content) {
            if (b.type === "slide_draft") {
              const d = parseSlideDraft(b.draft);
              if (d && !slideDraftStore.get(threadId, d.name)) {
                slideDraftStore.upsert(threadId, d.name, d.content, "ai");
              }
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

  // 対象下書きをアクティブへ（?name 優先・無ければ最新）。復元完了後、**nameParam ごとに一度**開く。
  const openedForRef = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (!recovered) return;
    const list = slideDraftStore.list(threadId);
    if (list.length === 0) return;
    const key = nameParam || "__latest__";
    if (openedForRef.current === key) return;
    openedForRef.current = key;
    const target =
      (nameParam && list.find((d) => d.name === nameParam)?.name) ??
      list[list.length - 1].name;
    openDraft(target);
  }, [recovered, threadId, nameParam, openDraft]);

  // アシスタントが同じ会話で流し込んだとき（slide_draft）は、その下書きを開き直す（再シード）。
  const onSlideDraftOpened = React.useCallback((name: string) => openDraft(name), [openDraft]);

  const doSave = React.useCallback(
    (target: SaveTarget) => {
      if (!activeName || !doc || saving) return;
      setSaving(true);
      const content = {
        version: 1,
        meta: { ...metaRef.current, title: target.name },
        slides: readSlidesJson(doc),
      };
      createSlide({ parentId: target.parentId, name: target.name, content })
        .then((node) => {
          slideDraftStore.remove(threadId, activeName);
          toast({ description: `「${node.name.replace(/\.slide$/i, "")}」を保存しました。` });
          router.replace(`/slides/${node.id}`);
        })
        .catch(() => {
          toast({ description: "保存に失敗しました。" });
          setSaving(false);
        });
    },
    [activeName, doc, saving, threadId, router],
  );

  // 統一ヘッダへ注入（戻る/下書きバッジ/保存/アシスタント切替・ノート下書きと同型）。
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
          {activeName || "下書きスライド"}
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
        description="チャットから「スライドを作って」と頼むと、ここに下書きが用意されます。"
      />
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="relative flex min-h-0 flex-1">
        <div
          className={cn(
            "flex min-w-0 flex-1 flex-col transition-[padding] duration-[var(--duration-normal)] ease-[var(--ease-standard)]",
            chatOpen && "lg:pr-[28rem]",
          )}
        >
          {/* 下書きタブ（同じ会話の複数下書きを切替・別名 = 別下書き）。 */}
          {drafts.length > 1 ? (
            <div
              className="flex flex-wrap items-center gap-1.5 px-4 pt-3"
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
                      ? "bg-accent font-medium text-foreground"
                      : "border-border/60 bg-card/40 text-muted-foreground hover:bg-secondary hover:text-foreground",
                  )}
                >
                  <PencilLine className="size-3.5 shrink-0" aria-hidden />
                  <span className="max-w-[12rem] truncate">{d.name}</span>
                </button>
              ))}
            </div>
          ) : null}

          <div className="min-h-0 flex-1">
            {doc && activeName ? (
              <SlideWorkspace key={activeName} doc={doc} editable />
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
              {/* 下書き画面では遷移せずアクティブ下書きを切替（onSlideDraftOpened）。 */}
              <Conversation
                threadId={threadId}
                variant="panel"
                onSlideDraftOpened={onSlideDraftOpened}
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
        entityLabel="スライド"
        description="下書きをスライドとして保存します。保存後はバージョン管理・共有・共同編集ができます。"
      />
    </div>
  );
}
