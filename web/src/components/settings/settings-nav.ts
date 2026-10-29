import {
  Bell,
  KeyRound,
  MessageSquare,
  MonitorSmartphone,
  Palette,
  Plug,
  Sparkles,
  UserRound,
  type LucideIcon,
} from "lucide-react";

export type SettingsNavItem = {
  label: string;
  href: string;
  icon: LucideIcon;
  /// API が未実装の画面（メニューに「準備中」を添える）。
  mock?: boolean;
};

export type SettingsNavGroup = { label: string; items: SettingsNavItem[] };

/// ユーザー設定のナビの単一定義（メニュー・現在地）。
export const SETTINGS_NAV: SettingsNavGroup[] = [
  {
    label: "アカウント",
    items: [{ label: "プロフィール", href: "/settings/profile", icon: UserRound }],
  },
  {
    label: "セキュリティ",
    items: [
      { label: "ログイン方法", href: "/settings/security", icon: KeyRound },
      { label: "端末とセッション", href: "/settings/sessions", icon: MonitorSmartphone },
    ],
  },
  {
    label: "環境設定",
    items: [
      { label: "表示", href: "/settings/appearance", icon: Palette },
      { label: "チャット", href: "/settings/chat", icon: MessageSquare },
      { label: "通知", href: "/settings/notifications", icon: Bell, mock: true },
    ],
  },
  {
    label: "AI",
    items: [{ label: "パーソナライズ", href: "/settings/ai", icon: Sparkles }],
  },
  {
    label: "連携",
    items: [{ label: "接続済みアプリ", href: "/settings/connections", icon: Plug, mock: true }],
  },
];

/// パスに対応するメニュー項目とそのグループ名・通し番号（四季の差し色の巡回に使う）。
export function settingsItemFor(
  pathname: string,
): { group: string; item: SettingsNavItem; index: number } | undefined {
  let index = 0;
  for (const group of SETTINGS_NAV) {
    for (const item of group.items) {
      if (pathname === item.href || pathname.startsWith(`${item.href}/`)) {
        return { group: group.label, item, index };
      }
      index += 1;
    }
  }
  return undefined;
}
