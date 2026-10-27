"use client";

/// 下書きスライド画面（Task 11.3）。/slides/draft?thread=&name= で未保存の下書きを詰める。
///
/// - 下書きは**クライアント内のみ**（サーバ未作成・[`slideDraftStore`] が真実源）。チャットで
///   「パワポを作って」→ save_slide が slide_draft を返し、本画面へ遷移して内容を用意する。
/// - 左はローカル Y.Doc に流し込んだ [`SlideWorkspace`]（editable・サーバ collab 非接続）。
///   編集は Y.Doc → 下書きストアへ書き戻す（リロード復元）。
/// - 同じ会話の複数下書きは**上部タブ**で切替（別名 = 別下書き・N 本並存）。
/// - 右上「保存」（PC は「ドライブに保存」）で POST /slides → 実体化して /slides/{id} へ（ノート下書きと同型）。

import { useRouter, useSearchParams } from "next/navigation";
import * as React from "react";
import * as Y from "yjs";

import { Conversation } from "@/components/chat/conversation";
import {
  DraftBadge,
  DraftLoading,
  DraftNotFound,
  DraftTabs,
  SaveDraftDialog,
  draftChrome,
  type SaveTarget,
} from "@/components/notes/save-draft-dialog";
import { AssistantLayout } from "@/components/shell/assistant-sheet";
import { EditorMobileHeading, useEditorBack } from "@/components/shell/editor-loading";
import { usePageChrome } from "@/components/shell/page-header-context";
import { SlideDesktopActions, slideMenuItems } from "@/components/slides/slide-header-slot";
import { SlideWorkspace, useSlideOverlays } from "@/components/slides/slide-workspace";
import { toast } from "@/components/ui/use-toast";
import { getThreadMessages } from "@/lib/chat-api";
import { createSlide } from "@/lib/slides-api";
import { parseSlideDraft, slideDraftStore } from "@/lib/slides/draft";
import { parseSlideDocJson, readSlidesJson, seedSlides } from "@/lib/slides-doc";
import { useDefaultSaveFolder } from "@/lib/drafts/save-folder";
import { useStorageScopeReady } from "@/lib/user-storage";

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
  // 保存先の初期値（AI の指定 → 由来ノートのフォルダ → ルート・#942）。
  const aiParentId = drafts.find((d) => d.name === activeName)?.parentId;
  const defaultFolder = useDefaultSaveFolder(threadId, aiParentId);
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

  const scopeReady = useStorageScopeReady();
  // 会話履歴の slide_draft ブロックから復元する（リロード/別端末）。ローカルに一部だけ
  // 残っている場合も履歴を常に取得し、**未登録の (threadId, name) のみ**追加する
  // （ローカルのユーザー編集は上書きしない・レビュー指摘対応）。
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
            if (b.type === "slide_draft") {
              const d = parseSlideDraft(b.draft);
              if (d && !slideDraftStore.get(threadId, d.name)) {
                slideDraftStore.upsert(threadId, d.name, d.content, "ai", d.parentId);
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

  // クローム（戻る → 保存 → アシスタント → ⋯）。エクスポート・プレゼンは ⋯（PC はバーにも）。
  const back = useEditorBack();
  const overlays = useSlideOverlays();
  const { onExport, onPresent } = overlays;
  const hasDoc = Boolean(doc && activeName);
  usePageChrome(
    () =>
      draftChrome({
        back,
        threadId,
        name: activeName || "下書きスライド",
        canSave: Boolean(activeName),
        onSave: () => setSaveOpen(true),
        chatOpen,
        onToggleChat: () => setChatOpen((v) => !v),
        extraMenu: hasDoc ? slideMenuItems({ onExport, onPresent }) : [],
        desktopExtra: hasDoc ? (
          <SlideDesktopActions onExport={onExport} onPresent={onPresent} />
        ) : undefined,
      }),
    [back, threadId, activeName, chatOpen, hasDoc, onExport, onPresent],
  );

  if (!threadId) {
    return <DraftNotFound what="スライド" />;
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* スマホ: 文書名＋状態（PC はバーに出す）。 */}
      <EditorMobileHeading name={activeName || "下書きスライド"} status={<DraftBadge />} />
      <AssistantLayout
        open={chatOpen}
        onClose={() => setChatOpen(false)}
        label="スライドのアシスタント"
        panel={
          // 下書き画面では遷移せずアクティブ下書きを切替（onSlideDraftOpened）。
          <Conversation
            threadId={threadId}
            variant="panel"
            onSlideDraftOpened={onSlideDraftOpened}
          />
        }
      >
        {/* 下書きタブ（同じ会話の複数下書きを切替・別名 = 別下書き）。 */}
        <DraftTabs
          drafts={drafts}
          activeName={activeName}
          onOpen={openDraft}
          className="px-4 pt-3 max-md:pb-1 max-md:pt-0"
        />

        <div className="min-h-0 flex-1">
          {doc && activeName ? (
            <SlideWorkspace
              key={activeName}
              doc={doc}
              editable
              name={activeName}
              presenting={overlays.presenting}
              onPresentingChange={overlays.setPresenting}
              exporting={overlays.exporting}
              onExportingChange={overlays.setExporting}
            />
          ) : (
            <DraftLoading />
          )}
        </div>
      </AssistantLayout>

      <SaveDraftDialog
        open={saveOpen}
        onOpenChange={setSaveOpen}
        defaultName={activeName}
        saving={saving}
        onConfirm={doSave}
        defaultFolder={defaultFolder}
        entityLabel="スライド"
        description="下書きをスライドとして保存します。保存後はバージョン管理・共有・共同編集ができます。"
      />
    </div>
  );
}
