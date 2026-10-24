"use client";

/// ドライブのスマホ用クローム（右上の浮いたボタン）。
///
/// 検索アイコンはドライブ内検索欄の開閉、戻るはフォルダの中で親フォルダへ戻る。
/// ドライブの区画（ホーム／最近使った／お気に入り／共有済み／ゴミ箱）の切替は本文上端の
/// タブ（DriveTabs）が担う。PC はサイドバーに子メニューがあり、上部バーもルート由来の
/// 既定表示のままにしたいので登録しない。

import * as React from "react";
import { Search } from "lucide-react";

import {
  ChromeIconButton,
  usePageChrome,
  type PageChromeSpec,
} from "@/components/shell/page-header-context";
import { useMediaQuery } from "@/hooks/use-media-query";

import { MOBILE_QUERY } from "./node-menu";

type Props = {
  /// ドライブ内検索欄の開閉。
  search: { open: boolean; onToggle: () => void };
  back?: PageChromeSpec["back"];
};

export function DriveChrome(props: Props) {
  const mobile = useMediaQuery(MOBILE_QUERY);
  return mobile ? <DriveChromeSlot {...props} /> : null;
}

function DriveChromeSlot({ search, back }: Props) {
  const { open: searchOpen, onToggle } = search;
  usePageChrome(
    () => ({
      title: "ドライブ",
      back,
      actions: (
        <ChromeIconButton
          label={searchOpen ? "検索を閉じる" : "ドライブ内を検索"}
          pressed={searchOpen}
          onClick={onToggle}
          data-testid="drive-search-toggle"
        >
          <Search aria-hidden />
        </ChromeIconButton>
      ),
    }),
    [searchOpen, onToggle, back?.href, back?.onClick, back?.label],
  );
  return null;
}
