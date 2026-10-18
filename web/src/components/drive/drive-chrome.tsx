"use client";

/// ドライブのスマホ用クローム（右上の浮いたボタン）。
///
/// スマホのドロワにはドライブの子メニュー（ホーム／最近使った／お気に入り／共有済み／ゴミ箱）を
/// 出さないため、⋯ がそれらへの導線になる。検索アイコンはドライブ内検索欄の開閉。
/// PC はサイドバーに子メニューがあり、上部バーもルート由来の既定表示のままにしたいので登録しない。

import * as React from "react";
import { Search } from "lucide-react";

import {
  ChromeIconButton,
  usePageChrome,
  type PageChromeSpec,
} from "@/components/shell/page-header-context";
import { useMediaQuery } from "@/hooks/use-media-query";
import { DRIVE_CHILDREN } from "@/lib/nav-config";

import { MOBILE_QUERY } from "./node-menu";

export type DriveSection = (typeof DRIVE_CHILDREN)[number]["key"];

type Props = {
  /// いま表示している区画（⋯ で「表示中」と示す）。
  current: DriveSection;
  /// ドライブ内検索欄の開閉（ホームのみ）。
  search?: { open: boolean; onToggle: () => void };
  back?: PageChromeSpec["back"];
};

export function DriveChrome(props: Props) {
  const mobile = useMediaQuery(MOBILE_QUERY);
  return mobile ? <DriveChromeSlot {...props} /> : null;
}

function DriveChromeSlot({ current, search, back }: Props) {
  const searchOpen = search?.open ?? false;
  const onToggle = search?.onToggle;
  usePageChrome(
    () => ({
      title: DRIVE_CHILDREN.find((c) => c.key === current)?.label ?? "ドライブ",
      back,
      actions: onToggle ? (
        <ChromeIconButton
          label={searchOpen ? "検索を閉じる" : "ドライブ内を検索"}
          pressed={searchOpen}
          onClick={onToggle}
          data-testid="drive-search-toggle"
        >
          <Search aria-hidden />
        </ChromeIconButton>
      ) : undefined,
      menuTitle: "ドライブを切り替え",
      menu: DRIVE_CHILDREN.map((c) => ({
        key: c.key,
        label: c.key === current ? `${c.label}（表示中）` : c.label,
        icon: c.icon,
        href: c.href,
        testId: `drive-switch-${c.key}`,
      })),
    }),
    [current, searchOpen, onToggle, back?.href, back?.onClick, back?.label],
  );
  return null;
}
