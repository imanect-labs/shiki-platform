"use client";

/// ファイルのプレビュー画面のクローム（戻る・タイトル・⋯ のダウンロード）。
///
/// 形式ごとのプレビューが自分の操作（拡大縮小・折り返しなど）を足して登録する。クロームは
/// 1 画面に 1 つなので、プレビューを出している間はページ側では登録しない。
import * as React from "react";
import { Download } from "lucide-react";

import { nodeKindLabel } from "@/components/drive/primitives";
import { useEditorBack } from "@/components/shell/editor-loading";
import { usePageChrome, type ChromeMenuItem, type PageChromeSpec } from "@/components/shell/page-header-context";
import type { NodeResponse } from "@/lib/storage";

export type FileChromeExtra = Pick<PageChromeSpec, "subtitle" | "actions" | "primary"> & {
  /// ダウンロードの前に並べる項目。
  menu?: ChromeMenuItem[];
  /// 戻る前に確かめる（未保存の編集があるときなど）。`go` を呼ぶと戻る。
  guardBack?: (go: () => void) => void;
};

export function useFileChrome(
  node: NodeResponse | null,
  onDownload: () => void,
  extra: FileChromeExtra,
  deps: React.DependencyList,
): void {
  const editorBack = useEditorBack();
  const guardBack = extra.guardBack;
  const back = React.useMemo(
    () => (guardBack ? { ...editorBack, onClick: () => guardBack(editorBack.onClick) } : editorBack),
    [editorBack, guardBack],
  );
  usePageChrome(() => {
    if (!node) return { back };
    const menu: ChromeMenuItem[] = [
      ...(extra.menu ?? []),
      {
        key: "download",
        label: "ダウンロード",
        icon: Download,
        onSelect: onDownload,
        testId: "file-menu-download",
        separatorBefore: (extra.menu?.length ?? 0) > 0,
      },
    ];
    return {
      title: node.name,
      menuTitle: node.name,
      back,
      subtitle: extra.subtitle ?? nodeKindLabel(node),
      primary: extra.primary,
      actions: extra.actions,
      menu,
    };
  }, [back, node, onDownload, ...deps]);
}
