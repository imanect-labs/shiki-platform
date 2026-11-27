import * as React from "react";

import { cn } from "@/lib/utils";
import { seasonAccentStyle } from "@/lib/season";

/// 共有ダイアログの色の意味づけ（シキの四季の差し色を、意味が一つに決まる所にだけ使う）。
/// - 権限: 閲覧＝冬（雪空の青）、編集＝秋（紅葉の橙）。選択・候補・リンクで同じ色にする。
/// - 部署・ロール（複数人に届く）＝夏（新緑）の淡い面。人はこれまでどおり無彩色。
/// 色は面と枠にごく薄く敷くだけにし、色の点や飾りのアイコンは置かない（意味は文字で伝える）。
export type ShareRoleKey = "viewer" | "editor";

export const ROLE_LABEL: Record<ShareRoleKey, string> = { viewer: "閲覧", editor: "編集" };
/// seasonVar のインデックス（春0/夏1/秋2/冬3）。
export const ROLE_SEASON: Record<ShareRoleKey, number> = { viewer: 3, editor: 2 };
export const GROUP_SEASON = 1;

/// 権限の小さな札（「閲覧で共有中」「編集できる」など）。色の面はごく薄く、文字は本文色のまま。
export function RoleTag({
  role,
  children,
  className,
}: {
  role: ShareRoleKey;
  children?: React.ReactNode;
  className?: string;
}) {
  return (
    <span
      style={seasonAccentStyle(ROLE_SEASON[role])}
      className={cn(
        "inline-flex shrink-0 items-center rounded-full bg-[var(--season)]/[0.16] px-2 py-px text-[11px] font-medium leading-4 text-foreground/85",
        className,
      )}
    >
      {children ?? ROLE_LABEL[role]}
    </span>
  );
}
