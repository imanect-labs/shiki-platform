import { Home, type LucideIcon, ScrollText, Sparkles, Users } from "lucide-react";

/// 管理コンソールのナビの単一定義（レール・プロダクトメニュー・パンくず）。

export type AdminMenuItem = {
  label: string;
  href: string;
  /// API が未実装のモック画面（バッジを出す）。
  mock?: boolean;
};

export type AdminMenuGroup = { label: string; items: AdminMenuItem[] };

export type AdminSection = {
  key: string;
  label: string;
  icon: LucideIcon;
  /// レールから飛ぶ先。
  href: string;
  /// このセクションに属するパスの接頭辞。
  prefix: string;
  /// 2 列目のプロダクトメニュー（無いセクションは 1 画面だけ）。
  menu?: AdminMenuGroup[];
  mock?: boolean;
};

export const ADMIN_SECTIONS: AdminSection[] = [
  { key: "home", label: "ホーム", icon: Home, href: "/admin", prefix: "/admin" },
  {
    key: "access",
    label: "アクセス管理",
    icon: Users,
    href: "/admin/members",
    prefix: "/admin/(members|roles)",
    menu: [
      {
        label: "メンバー",
        items: [
          { label: "メンバー", href: "/admin/members", mock: true },
          { label: "ロール", href: "/admin/roles", mock: true },
        ],
      },
    ],
  },
  {
    key: "ai",
    label: "AI",
    icon: Sparkles,
    href: "/admin/ai/usage",
    prefix: "/admin/ai",
    menu: [
      { label: "分析", items: [{ label: "利用量", href: "/admin/ai/usage" }] },
      {
        label: "設定",
        items: [
          { label: "モデル", href: "/admin/ai/models" },
          { label: "プロバイダ", href: "/admin/ai/providers" },
        ],
      },
    ],
  },
  {
    key: "audit",
    label: "監査ログ",
    icon: ScrollText,
    href: "/admin/audit",
    prefix: "/admin/audit",
    mock: true,
  },
];

/// パスが属するセクション（最長一致・ホームは完全一致のみ）。
export function sectionFor(pathname: string): AdminSection {
  const hit = ADMIN_SECTIONS.find(
    (s) => s.key !== "home" && new RegExp(`^${s.prefix}(/|$)`).test(pathname),
  );
  return hit ?? ADMIN_SECTIONS[0];
}

/// パスに対応するメニュー項目（パンくずの末尾）。
export function menuItemFor(pathname: string): AdminMenuItem | undefined {
  return sectionFor(pathname)
    .menu?.flatMap((g) => g.items)
    .find((i) => pathname === i.href || pathname.startsWith(`${i.href}/`));
}
