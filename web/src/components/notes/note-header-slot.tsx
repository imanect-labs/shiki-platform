"use client";

/// ノート詳細のクローム（シェルの `usePageChrome` へ登録する）。
///
/// - 左: 戻る（履歴があれば直前へ、無ければドライブ）。
/// - 右: AI に依頼（✦）→ ⋯（共有・PDF/Markdown/Word）。外に出すアイコンは持たない。
/// - PC のバー: 名前＋状態（閲覧のみ・同期中/オフライン）、補助にプレゼンス・共有・エクスポート。
/// - スマホ: 名前と状態は本文先頭の状態行（ページ側）に出す。
///
/// 読み込み中・notfound でも戻るを出すため、ページ最上位（早期 return より前）で呼ぶ。

import { Eye, FileText, FileType, Printer, Share2 } from "lucide-react";
import * as React from "react";
import type { Editor } from "@tiptap/react";

import { Button } from "@/components/ui/button";
import { ShareDialog } from "@/components/drive/share-dialog";
import { NoteExportMenu, useNoteExport } from "@/components/notes/note-export-menu";
import { PresenceAvatars } from "@/components/notes/presence";
import { EditorStatusBadge, useEditorBack } from "@/components/shell/editor-loading";
import { usePageChrome } from "@/components/shell/page-header-context";
import type { CollabProvider, CollabStatus } from "@/lib/collab";
import { SyncPill } from "./note-status";

export type NoteChromeState = {
  name: string;
  editable: boolean;
  status: CollabStatus;
  synced: boolean;
  chatOpen: boolean;
  onToggleChat: () => void;
  provider: CollabProvider | null;
  /// 本文エディタ（エクスポートの md 直列化・genui スナップショットに使う・未生成は null）。
  editor: Editor | null;
};

/// クロームを登録し、共有ダイアログ（ページに置く要素）を返す。
/// `state` が null の間（読み込み中・開けない）は戻るだけを出す。
export function useNoteChrome(nodeId: string, state: NoteChromeState | null): React.ReactNode {
  const [shareOpen, setShareOpen] = React.useState(false);
  const back = useEditorBack();
  const exp = useNoteExport({
    editor: state?.editor ?? null,
    nodeId,
    name: state?.name ?? "",
    synced: state?.synced ?? false,
  });

  const name = state?.name;
  const editable = state?.editable ?? false;
  const status = state?.status;
  const synced = state?.synced ?? false;
  const chatOpen = state?.chatOpen ?? false;
  const onToggleChat = state?.onToggleChat;
  const provider = state?.provider ?? null;

  usePageChrome(() => {
    if (!state || !status) return { back };
    return {
      title: name,
      menuTitle: name,
      back,
      subtitle: (
        <span className="inline-flex items-center gap-1.5">
          {!editable ? (
            <EditorStatusBadge icon={Eye} data-testid="note-readonly-badge">
              閲覧のみ
            </EditorStatusBadge>
          ) : null}
          <SyncPill status={status} synced={synced} />
        </span>
      ),
      desktopExtra: (
        <>
          {provider ? (
            <span className="mr-1">
              <PresenceAvatars provider={provider} />
            </span>
          ) : null}
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => setShareOpen(true)}
            data-testid="note-share"
            className="gap-1.5 text-sm text-foreground/70 hover:text-foreground"
          >
            <Share2 className="size-4" aria-hidden />
            共有
          </Button>
          <NoteExportMenu exp={exp} synced={synced} />
        </>
      ),
      assistant: onToggleChat
        ? { active: chatOpen, onToggle: onToggleChat, testId: "note-ask-ai" }
        : undefined,
      menu: [
        {
          key: "share",
          label: "共有",
          icon: Share2,
          onSelect: () => setShareOpen(true),
          testId: "note-menu-share",
        },
        {
          key: "pdf",
          label: "PDF で書き出す（印刷）",
          icon: Printer,
          onSelect: exp.onPdf,
          disabled: exp.disabled,
          separatorBefore: true,
          testId: "note-menu-export-pdf",
        },
        {
          key: "md",
          label: "Markdown (.md) で書き出す",
          icon: FileText,
          onSelect: exp.onMd,
          disabled: exp.disabled,
          testId: "note-menu-export-md",
        },
        {
          key: "docx",
          label: "Word (.docx) で書き出す",
          icon: FileType,
          onSelect: () => void exp.onDocx(),
          disabled: exp.disabled,
          testId: "note-menu-export-docx",
        },
      ],
    };
  }, [state === null, name, editable, status, synced, chatOpen, onToggleChat, provider, exp, back]);

  if (!state) return null;
  return (
    <ShareDialog
      open={shareOpen}
      onOpenChange={setShareOpen}
      node={{ id: nodeId, name: state.name }}
      shareUrl={`/notes/${nodeId}`}
    />
  );
}
