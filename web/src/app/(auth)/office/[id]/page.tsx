"use client";

/// Office 文書編集ページ（Task 11.7 ＋ 11.10）。/office/[id] で docx/xlsx/pptx を Collabora
/// で開き、右にアシスタントパネル（この文書についての AI 会話）を分割表示する。
///
/// - `/office/sessions` の材料を [`OfficeEditor`] が form POST で iframe へ注入する
///   （トークンは URL に載せない・WOPI 側が毎呼び出しで ReBAC 再チェック・PIT-11）。
/// - 選択→AI（Task 11.10・Office 版）: エディタから postMessage で選択テキストを取得し、
///   コンテキストチップとしてアシスタントに添付する（office.edit がファイル単位で適用）。
/// - Collabora 未配備（office profile 未起動）は 503 → 案内表示へフォールバック。

import { Download, FileWarning, History, MessageSquare, PlugZap, Share2 } from "lucide-react";
import Link from "next/link";
import { useParams, useSearchParams } from "next/navigation";
import * as React from "react";

import { ShareDialog } from "@/components/drive/share-dialog";
import { VersionsDialog } from "@/components/drive/versions-dialog";
import { AssistantLayout } from "@/components/shell/assistant-sheet";
import { CiteHint, shortPhrase } from "@/components/shell/cite-hint";
import { useCiteLocator } from "@/lib/citation-locate";
import { OfficeChatPanel } from "@/components/office/office-chat-panel";
import { OfficeEditor, type OfficeEditorHandle } from "@/components/office/office-editor";
import {
  EditorLoading,
  EditorMobileHeading,
  EditorUnavailable,
  useEditorBack,
} from "@/components/shell/editor-loading";
import { ChromeIconButton, usePageChrome } from "@/components/shell/page-header-context";
import { Button } from "@/components/ui/button";
import { ShareLinkUnlock, unlockTokenFromUrl } from "@/components/share/share-link-unlock";
import { toast } from "@/components/ui/use-toast";
import {
  createOfficeSession,
  OfficeSessionError,
  type OfficeSession,
} from "@/lib/office-api";
import { setPendingSelection } from "@/lib/selection-context";
import { downloadUrl, getNode, type NodeResponse } from "@/lib/storage";
import { useDocThreadDeepLink } from "@/hooks/use-doc-thread-deep-link";

type LoadState =
  | { phase: "loading" }
  | { phase: "ready"; session: OfficeSession; fileName: string; node: NodeResponse | null }
  | { phase: "notfound" }
  | { phase: "unavailable" }
  | { phase: "error"; message: string };

export default function OfficePage() {
  const params = useParams<{ id: string }>();
  const fileId = params.id;
  const [state, setState] = React.useState<LoadState>({ phase: "loading" });
  const searchParams = useSearchParams();
  // サイドバーの履歴・検索から来たときは、アシスタントを開いてその会話を指定する（?thread=・#598）。
  const initialThreadId = searchParams.get("thread");
  const { chatOpen, setChatOpen, deepLinkThreadId } = useDocThreadDeepLink(initialThreadId);
  const [shareOpen, setShareOpen] = React.useState(false);
  const [versionsOpen, setVersionsOpen] = React.useState(false);
  // 解錠（redeem）後にセッション取得をやり直すためのリロードキー。
  const [reloadKey, setReloadKey] = React.useState(0);
  const editorRef = React.useRef<OfficeEditorHandle>(null);
  // 引用箇所から開いたとき（?cite=）: 読み込み後にこの一節を文書内で検索する（#508）。
  // 段落全体で探すと、該当の段落がまるごと選択される（先頭だけだと途中までしか選ばれない）。
  const locator = useCiteLocator(searchParams.get("cite"));
  const findText = locator?.paragraph ?? locator?.find ?? null;

  React.useEffect(() => {
    let cancelled = false;
    setState({ phase: "loading" });
    // 文書名（パネルの会話タイトル用）と編集セッションを並行取得する。
    Promise.all([
      createOfficeSession(fileId),
      getNode(fileId).catch(() => null),
    ])
      .then(([session, node]) => {
        if (!cancelled) {
          setState({ phase: "ready", session, fileName: node?.name ?? "文書", node });
        }
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
  }, [fileId, reloadKey]);

  // 直近で挿入した選択テキスト（ポーリングの重複挿入を避ける）。
  const lastSelRef = React.useRef<string | null>(null);

  const captureSelectionIntoChat = React.useCallback(async () => {
    const text = await editorRef.current?.getSelectionText();
    if (text && text !== lastSelRef.current) {
      lastSelRef.current = text;
      setPendingSelection({ kind: "office_selection", node_id: fileId, excerpt: text });
    }
  }, [fileId]);

  // 「AI に依頼」= アシスタントパネルを開く（＋その時の選択があればチャットへ挿入）。
  const openAssistant = React.useCallback(() => {
    setChatOpen((open) => {
      if (!open) void captureSelectionIntoChat();
      return !open;
    });
  }, [captureSelectionIntoChat, setChatOpen]);

  // パネルを開いている間は選択を監視してチャットへ自動挿入する。Collabora は選択変更
  // イベントを出さないため、軽いポーリングで現在の選択を拾う（変化時のみ挿入）。
  React.useEffect(() => {
    if (!chatOpen) {
      lastSelRef.current = null;
      return;
    }
    const id = window.setInterval(() => void captureSelectionIntoChat(), 1000);
    return () => window.clearInterval(id);
  }, [chatOpen, captureSelectionIntoChat]);

  // ダウンロード（保存済みの最新版。Collabora の未保存の編集は含まない）。
  const download = React.useCallback(() => {
    downloadUrl(fileId)
      .then(({ url }) => {
        window.location.href = url;
      })
      .catch(() => toast({ variant: "destructive", description: "ダウンロードできませんでした。" }));
  }, [fileId]);

  // クローム（戻る → 共有 → AI に依頼 → ⋯）。読み込み中・開けないときも戻るを出す。
  const back = useEditorBack();
  const ready = state.phase === "ready";
  const displayName =
    state.phase === "ready" ? state.fileName.replace(/\.(docx|xlsx|pptx|odt|ods|odp)$/i, "") : "";
  const hasNode = state.phase === "ready" && state.node !== null;
  usePageChrome(() => {
    if (!ready) return { back };
    return {
      title: displayName,
      menuTitle: displayName,
      back,
      actions: (
        <ChromeIconButton
          label="共有"
          onClick={() => setShareOpen(true)}
          data-testid="office-share"
        >
          <Share2 aria-hidden />
        </ChromeIconButton>
      ),
      assistant: { active: chatOpen, onToggle: openAssistant, testId: "office-ask-ai" },
      menu: [
        { key: "share", label: "共有", icon: Share2, onSelect: () => setShareOpen(true) },
        {
          key: "download",
          label: "ダウンロード",
          icon: Download,
          onSelect: download,
          testId: "office-menu-download",
        },
        ...(hasNode
          ? [
              {
                key: "versions",
                label: "版履歴",
                icon: History,
                onSelect: () => setVersionsOpen(true),
                testId: "office-menu-versions",
              },
            ]
          : []),
      ],
    };
  }, [back, ready, displayName, hasNode, chatOpen, openAssistant, download]);

  // 文書を開けなくても（存在しない・権限なし・Collabora 停止・読み込み失敗）、会話そのものは
  // 通常のチャットで辿れるようにする（ノートと同じ・#282）。
  const openInChat = initialThreadId ? (
    <Button asChild>
      <Link href={`/c/${initialThreadId}`}>
        <MessageSquare aria-hidden />
        この会話をチャットで開く
      </Link>
    </Button>
  ) : undefined;

  if (state.phase === "loading") {
    // 拡張子から表計算/文書を推定して骨格を出し分ける（初回は URL しか無いので doc 既定）。
    const kind = /\.(xlsx|ods|csv)$/i.test(fileId) ? "sheet" : "doc";
    return <EditorLoading kind={kind} message="文書を開いています…" />;
  }
  if (state.phase === "notfound") {
    return (
      <EditorUnavailable
        icon={FileWarning}
        title="この文書は開けません"
        description="ファイルが存在しないか、開く権限がないか、ブラウザ編集に対応していない形式です。パスワード付き共有リンクの場合はパスワードで開けます。"
        actions={openInChat}
      >
        {/* パスワード付き共有リンクの解錠（#342）。?lt=<token> がある時のみ表示。 */}
        {unlockTokenFromUrl() ? (
          <ShareLinkUnlock
            token={unlockTokenFromUrl()!}
            onUnlocked={() => setReloadKey((k) => k + 1)}
            autoFocus
          />
        ) : null}
      </EditorUnavailable>
    );
  }
  if (state.phase === "unavailable") {
    return (
      <EditorUnavailable
        icon={PlugZap}
        title="Office 編集サービスに接続できません"
        description="Collabora が起動していません。管理者に office profile の有効化（docker compose --profile office up）を確認してください。"
        actions={openInChat}
      />
    );
  }
  if (state.phase === "error") {
    return (
      <EditorUnavailable
        icon={FileWarning}
        title="読み込みに失敗しました"
        description={state.message}
        actions={
          <>
            {openInChat}
            <Button variant="outline" onClick={() => setReloadKey((k) => k + 1)}>
              もう一度試す
            </Button>
          </>
        }
      />
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <ShareDialog
        open={shareOpen}
        onOpenChange={setShareOpen}
        node={{ id: fileId, name: state.fileName }}
        shareUrl={`/office/${fileId}`}
      />
      {state.node ? (
        <VersionsDialog
          open={versionsOpen}
          onOpenChange={setVersionsOpen}
          node={state.node}
          // 版を戻したら新しい版で開き直す（編集セッションを取り直す）。
          onRestored={() => setReloadKey((k) => k + 1)}
        />
      ) : null}
      {/* スマホ: 文書名（PC はバーに出す）。 */}
      <EditorMobileHeading name={displayName} className="pb-1" />

      <AssistantLayout
        open={chatOpen}
        onClose={() => setChatOpen(false)}
        label="文書のアシスタント"
        overlay={
          findText ? (
            <CiteHint tone="approx">
              引用箇所「{shortPhrase(findText)}」を文書内で検索しました（選択されている箇所）
            </CiteHint>
          ) : null
        }
        panel={
          <OfficeChatPanel
            fileId={fileId}
            fileName={state.fileName}
            initialThreadId={deepLinkThreadId}
          />
        }
      >
        <div className="min-h-0 flex-1">
          <OfficeEditor
            ref={editorRef}
            session={state.session}
            // Collabora 側の閉じる（スマホ左上の戻る等）もヘッダの戻ると同じ行き先にする
            // （チャットから開いたらチャットへ戻る。直前が無いときだけドライブ）。
            onClose={back.onClick}
            findText={findText}
          />
        </div>
      </AssistantLayout>
    </div>
  );
}
