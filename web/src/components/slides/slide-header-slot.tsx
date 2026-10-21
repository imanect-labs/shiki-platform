"use client";

/// スライド詳細のクローム（ノートの useNoteChrome と同型・Task 11.1）。
///
/// - 左: 戻る。右: AI に依頼（✦）→ ⋯（エクスポート・プレゼン）。
/// - PC のバー: 名前＋状態（閲覧のみ・同期中/オフライン）、補助にプレゼンスとエクスポート/プレゼン。
/// - スマホ: 名前と状態は本文先頭（ページ側の EditorMobileHeading）に出す。
///
/// 読み込み中・notfound でも戻るを出すため、ページ最上位（早期 return より前）で呼ぶ。

import { Eye, FileDown, Presentation } from "lucide-react";
import * as React from "react";

import { PresenceAvatars } from "@/components/notes/presence";
import { SyncPill } from "@/components/notes/note-status";
import { EditorStatusBadge, useEditorBack } from "@/components/shell/editor-loading";
import { usePageChrome, type ChromeMenuItem } from "@/components/shell/page-header-context";
import { Button } from "@/components/ui/button";
import type { CollabProvider, CollabStatus } from "@/lib/collab";

/// エクスポート・プレゼンの開始（ワークスペースの状態を親が持つ）。
export type SlideActions = { onExport: () => void; onPresent: () => void };

/// ⋯ メニューのエクスポート・プレゼン項目（詳細・下書きで共用）。
export function slideMenuItems({ onExport, onPresent }: SlideActions): ChromeMenuItem[] {
  return [
    {
      key: "present",
      label: "プレゼンする",
      icon: Presentation,
      onSelect: onPresent,
      testId: "slide-menu-present",
    },
    {
      key: "export",
      label: "PowerPoint へエクスポート",
      icon: FileDown,
      onSelect: onExport,
      testId: "slide-menu-export",
    },
  ];
}

/// PC のバーに出すエクスポート・プレゼン（文字ボタン・幅に余裕があるため外に出す）。
export function SlideDesktopActions({ onExport, onPresent }: SlideActions) {
  return (
    <>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        onClick={onExport}
        data-testid="slide-export"
        className="gap-1.5 text-sm text-foreground/70 hover:text-foreground"
      >
        <FileDown className="size-4" aria-hidden />
        エクスポート
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        onClick={onPresent}
        data-testid="slide-present"
        className="gap-1.5 text-sm text-foreground/70 hover:text-foreground"
      >
        <Presentation className="size-4" aria-hidden />
        プレゼン
      </Button>
    </>
  );
}

export type SlideChromeState = SlideActions & {
  name: string;
  editable: boolean;
  status: CollabStatus;
  synced: boolean;
  provider: CollabProvider | null;
  chatOpen: boolean;
  onToggleChat: () => void;
};

/// クロームを登録する。`state` が null の間（読み込み中・開けない）は戻るだけを出す。
export function useSlideChrome(state: SlideChromeState | null): void {
  const back = useEditorBack();
  const name = state?.name;
  const editable = state?.editable ?? false;
  const status = state?.status;
  const synced = state?.synced ?? false;
  const provider = state?.provider ?? null;
  const chatOpen = state?.chatOpen ?? false;
  const onToggleChat = state?.onToggleChat;
  const onExport = state?.onExport;
  const onPresent = state?.onPresent;

  usePageChrome(() => {
    if (!status || !onToggleChat || !onExport || !onPresent) return { back };
    return {
      title: name,
      menuTitle: name,
      back,
      subtitle: (
        <span className="inline-flex items-center gap-1.5">
          {!editable ? (
            <EditorStatusBadge icon={Eye} data-testid="slide-readonly-badge">
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
          <SlideDesktopActions onExport={onExport} onPresent={onPresent} />
        </>
      ),
      assistant: { active: chatOpen, onToggle: onToggleChat, testId: "slide-ask-ai" },
      menu: slideMenuItems({ onExport, onPresent }),
    };
  }, [back, name, editable, status, synced, provider, chatOpen, onToggleChat, onExport, onPresent]);
}
