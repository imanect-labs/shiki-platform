"use client";

/// Office 文書編集ページ（Task 11.7 ＋ 11.10）。/office/[id] で docx/xlsx/pptx を Collabora
/// で開き、右にアシスタントパネル（この文書についての AI 会話）を分割表示する。
///
/// - `/office/sessions` の材料を [`OfficeEditor`] が form POST で iframe へ注入する
///   （トークンは URL に載せない・WOPI 側が毎呼び出しで ReBAC 再チェック・PIT-11）。
/// - 選択→AI（Task 11.10・Office 版）: エディタから postMessage で選択テキストを取得し、
///   コンテキストチップとしてアシスタントに添付する（office.edit がファイル単位で適用）。
/// - Collabora 未配備（office profile 未起動）は 503 → 案内表示へフォールバック。

import { FileWarning, MessageSquare, PlugZap, Sparkles, X } from "lucide-react";
import { useParams, useRouter } from "next/navigation";
import * as React from "react";

import { OfficeChatPanel } from "@/components/office/office-chat-panel";
import { OfficeEditor, type OfficeEditorHandle } from "@/components/office/office-editor";
import { EditorLoading } from "@/components/shell/editor-loading";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { FadeSlide } from "@/components/ui/motion-primitives";
import { toast } from "@/components/ui/use-toast";
import {
  createOfficeSession,
  OfficeSessionError,
  type OfficeSession,
} from "@/lib/office-api";
import { setPendingSelection } from "@/lib/selection-context";
import { getNode } from "@/lib/storage";

type LoadState =
  | { phase: "loading" }
  | { phase: "ready"; session: OfficeSession; fileName: string }
  | { phase: "notfound" }
  | { phase: "unavailable" }
  | { phase: "error"; message: string };

export default function OfficePage() {
  const params = useParams<{ id: string }>();
  const fileId = params.id;
  const router = useRouter();
  const [state, setState] = React.useState<LoadState>({ phase: "loading" });
  const [chatOpen, setChatOpen] = React.useState(false);
  const editorRef = React.useRef<OfficeEditorHandle>(null);

  React.useEffect(() => {
    let cancelled = false;
    // 文書名（パネルの会話タイトル用）と編集セッションを並行取得する。
    Promise.all([
      createOfficeSession(fileId),
      getNode(fileId).catch(() => null),
    ])
      .then(([session, node]) => {
        if (!cancelled) setState({ phase: "ready", session, fileName: node?.name ?? "文書" });
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof OfficeSessionError) {
          setState({ phase: e.kind });
        } else {
          setState({ phase: "error", message: e instanceof Error ? e.message : String(e) });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [fileId]);

  const askAiAboutSelection = React.useCallback(async () => {
    const text = await editorRef.current?.getSelectionText();
    if (!text) {
      toast({
        title: "選択範囲がありません",
        description: "文書内でテキストを選択してから「AI に依頼」を押してください。",
      });
      return;
    }
    setPendingSelection({ kind: "office_selection", node_id: fileId, excerpt: text });
    setChatOpen(true);
  }, [fileId]);

  if (state.phase === "loading") {
    // 拡張子から表計算/文書を推定して骨格を出し分ける（初回は URL しか無いので doc 既定）。
    const kind = /\.(xlsx|ods|csv)$/i.test(fileId) ? "sheet" : "doc";
    return <EditorLoading kind={kind} message="文書を開いています…" />;
  }
  if (state.phase === "notfound") {
    return (
      <EmptyState
        icon={FileWarning}
        title="この文書は開けません"
        description="ファイルが存在しないか、開く権限がないか、ブラウザ編集に対応していない形式です。"
      />
    );
  }
  if (state.phase === "unavailable") {
    return (
      <EmptyState
        icon={PlugZap}
        title="Office 編集サービスに接続できません"
        description="Collabora が起動していません。管理者に office profile の有効化（docker compose --profile office up）を確認してください。"
      />
    );
  }
  if (state.phase === "error") {
    return <EmptyState icon={FileWarning} title="読み込みに失敗しました" description={state.message} />;
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* ヘッダ: 選択→AI とアシスタント開閉。 */}
      <div className="flex h-11 shrink-0 items-center gap-2 px-3 shiki-dash-bottom">
        <span className="truncate text-sm font-medium">
          {state.fileName.replace(/\.(docx|xlsx|pptx|odt|ods|odp)$/i, "")}
        </span>
        <span className="flex-1" />
        <Button
          type="button"
          size="sm"
          variant="ghost"
          onClick={() => void askAiAboutSelection()}
          data-testid="office-ask-ai"
          className="gap-1.5"
        >
          <Sparkles className="size-4" aria-hidden />
          AI に依頼
        </Button>
        <Button
          type="button"
          size="sm"
          variant={chatOpen ? "secondary" : "ghost"}
          onClick={() => setChatOpen((v) => !v)}
          aria-pressed={chatOpen}
          data-testid="office-chat-toggle"
          className="gap-1.5"
        >
          <MessageSquare className="size-4" aria-hidden />
          アシスタント
        </Button>
      </div>

      <div className="relative min-h-0 flex-1">
        <div className={chatOpen ? "h-full lg:pr-[28rem]" : "h-full"}>
          <OfficeEditor
            ref={editorRef}
            session={state.session}
            onClose={() => router.push("/drive")}
          />
        </div>

        {chatOpen ? (
          <FadeSlide
            from="right"
            role="complementary"
            aria-label="文書のアシスタント"
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
              <OfficeChatPanel fileId={fileId} fileName={state.fileName} />
            </div>
          </FadeSlide>
        ) : null}
      </div>
    </div>
  );
}
