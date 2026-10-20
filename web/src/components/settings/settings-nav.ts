import { Settings } from "lucide-react";

import type { AdminMenuItem, AdminSection } from "@/components/admin/admin-nav";

/// ユーザー設定のナビの単一定義（メニュー・パンくず）。管理コンソールと同じ型で持ち、
/// 2 列目のメニュー（ProductMenu）をそのまま使う。`mock` は API が未実装の画面。
export const SETTINGS_SECTION: AdminSection = {
  key: "settings",
  label: "設定",
  icon: Settings,
  href: "/settings/profile",
  prefix: "/settings",
  menu: [
    {
      label: "アカウント",
      items: [{ label: "プロフィール", href: "/settings/profile" }],
    },
    {
      label: "セキュリティ",
      items: [
        { label: "ログイン方法", href: "/settings/security" },
        { label: "端末とセッション", href: "/settings/sessions" },
      ],
    },
    {
      label: "環境設定",
      items: [
        { label: "表示", href: "/settings/appearance" },
        { label: "通知", href: "/settings/notifications", mock: true },
      ],
    },
    {
      label: "AI",
      items: [{ label: "パーソナライズ", href: "/settings/ai", mock: true }],
    },
    {
      label: "連携",
      items: [{ label: "接続済みアプリ", href: "/settings/connections", mock: true }],
    },
  ],
};

/// パスに対応するメニュー項目とそのグループ名（パンくず用）。
export function settingsItemFor(
  pathname: string,
): { group: string; item: AdminMenuItem } | undefined {
  for (const group of SETTINGS_SECTION.menu ?? []) {
    const item = group.items.find((i) => pathname === i.href || pathname.startsWith(`${i.href}/`));
    if (item) return { group: group.label, item };
  }
  return undefined;
}
