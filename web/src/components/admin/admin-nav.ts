import { Home, type LucideIcon, ScrollText, Sparkles, Users } from "lucide-react";

import type { MeResponse } from "@/lib/api";

/// 管理コンソールのナビの単一定義（レール・プロダクトメニュー・パンくず）。

export type AdminMenuItem = {
  label: string;
  href: string;
  /// API が未実装のモック画面（バッジを出す）。
  mock?: boolean;
  /// 配下のパスでは選択中にしない（下位の画面が別の項目として並ぶとき）。
  exact?: boolean;
};

/// パスがこの項目（またはその配下）か。
export function isMenuItemActive(item: AdminMenuItem, pathname: string): boolean {
  return pathname === item.href || (!item.exact && pathname.startsWith(`${item.href}/`));
}

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
  /// 誰が開けるか。`admin` は組織管理者（`organization#admin`）、`audit` は監査閲覧者
  /// （`organization#audit_viewer`・#634）。両者は独立で、互いに含意しない。
  access: AdminAccess;
};

export type AdminAccess = "admin" | "audit";

/// 管理コンソールの入口に必要な権限（`/me` の値）。
export type AdminPermissions = Pick<MeResponse, "is_admin" | "can_view_audit">;

export function canOpen(access: AdminAccess, me: AdminPermissions | null | undefined): boolean {
  if (!me) return false;
  return access === "audit" ? me.can_view_audit : me.is_admin;
}

export const ADMIN_SECTIONS: AdminSection[] = [
  { key: "home", label: "ホーム", icon: Home, href: "/admin", prefix: "/admin", access: "admin" },
  {
    key: "access",
    label: "アクセス管理",
    icon: Users,
    href: "/admin/members",
    prefix: "/admin/(members|roles)",
    access: "admin",
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
    access: "admin",
    menu: [
      {
        label: "分析",
        items: [
          { label: "活用状況", href: "/admin/ai/insights", exact: true },
          { label: "利用者と部署", href: "/admin/ai/insights/adoption" },
          { label: "用途", href: "/admin/ai/insights/use-cases" },
          { label: "効果", href: "/admin/ai/insights/impact" },
          { label: "利用量", href: "/admin/ai/usage" },
        ],
      },
      {
        label: "設定",
        items: [
          { label: "モデル", href: "/admin/ai/models" },
          { label: "プロバイダ", href: "/admin/ai/providers" },
          { label: "分析の設定", href: "/admin/ai/insights/settings" },
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
    access: "audit",
  },
];

/// その人が開けるセクション（レール・モバイルのメニューに出すもの）。
export function visibleSections(me: AdminPermissions | null | undefined): AdminSection[] {
  return ADMIN_SECTIONS.filter((s) => canOpen(s.access, me));
}

/// 管理コンソールの入口（管理者はホーム、監査閲覧者だけの人は監査ログ）。どちらでもなければ null。
export function adminEntry(me: AdminPermissions | null | undefined): string | null {
  if (me?.is_admin) return "/admin";
  if (me?.can_view_audit) return "/admin/audit";
  return null;
}

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
    .find((i) => isMenuItemActive(i, pathname));
}
