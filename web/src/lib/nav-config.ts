import {
  Clock,
  FolderOpen,
  Home,
  LayoutGrid,
  type LucideIcon,
  MessageSquareText,
  Settings,
  Share2,
  Sparkles,
  Star,
  Trash2,
  Workflow,
} from "lucide-react";

/// ナビゲーションの単一定義（項目・ルート・アイコン）。
/// サイドバーと現在地判定（aria-current）はここを正本に組み立てる。

export type NavLeaf = {
  key: string;
  label: string;
  href: string;
  icon: LucideIcon;
};

/// Drive アコーディオン配下の子。`backend` が false の項目は本 issue 時点で
/// API が無く、ページは「作り込んだ空状態」を表示する（フェイクデータを置かない）。
export type DriveChild = NavLeaf & { backend: boolean };

/// Drive のルート（親）。クリックでホームへ遷移し、アコーディオンを開く。
export const DRIVE_ROOT = "/drive";

export const DRIVE_CHILDREN: DriveChild[] = [
  { key: "home", label: "ホーム", href: "/drive", icon: Home, backend: true },
  { key: "recent", label: "最近使った", href: "/drive/recent", icon: Clock, backend: false },
  { key: "favorites", label: "お気に入り", href: "/drive/favorites", icon: Star, backend: false },
  { key: "shared", label: "共有済み", href: "/drive/shared", icon: Share2, backend: true },
  { key: "trash", label: "ゴミ箱", href: "/drive/trash", icon: Trash2, backend: true },
];

export const DRIVE_ICON: LucideIcon = FolderOpen;

/// スキル / ミニアプリ（Phase 6）。サイドバーのトップレベル項目。
export const SKILLS_NAV: NavLeaf = { key: "skills", label: "スキル", href: "/skills", icon: Sparkles };
export const APPS_NAV: NavLeaf = { key: "apps", label: "アプリ", href: "/apps", icon: LayoutGrid };

/// ワークフロー（Phase 10）。dnd エディタと実行履歴の入口。
export const WORKFLOWS_NAV: NavLeaf = {
  key: "workflows",
  label: "ワークフロー",
  href: "/workflows",
  icon: Workflow,
};

/// あるパスがナビ項目のアクティブ対象かを判定する。
/// 完全一致、またはルート（"/" を除く）配下のサブパスを active とみなす。
export function isActivePath(href: string, pathname: string): boolean {
  if (href === "/") return pathname === "/";
  return pathname === href || pathname.startsWith(`${href}/`);
}

/// パスからヘッダのページタイトルを解決する（現在地表示）。
export function resolvePageTitle(pathname: string): string {
  if (pathname === "/") return "ホーム";
  if (pathname.startsWith("/c/")) return "チャット";
  if (pathname === "/settings") return "設定";
  if (isActivePath(SKILLS_NAV.href, pathname)) return "スキル";
  if (isActivePath(APPS_NAV.href, pathname)) return "アプリ";
  if (isActivePath(WORKFLOWS_NAV.href, pathname)) return "ワークフロー";
  const child = DRIVE_CHILDREN.find((c) => c.href === pathname);
  // ドライブのルート（home child）は単に「ドライブ」。下位ページのみ親付きで示す。
  if (child) return child.key === "home" ? "ドライブ" : `ドライブ・${child.label}`;
  if (pathname.startsWith(DRIVE_ROOT)) return "ドライブ";
  return "Shiki";
}

/// パスからヘッダのアイコンを解決する。
export function resolvePageIcon(pathname: string): LucideIcon {
  if (pathname === "/") return MessageSquareText;
  if (pathname.startsWith("/c/")) return MessageSquareText;
  if (pathname === "/settings") return Settings;
  if (isActivePath(SKILLS_NAV.href, pathname)) return SKILLS_NAV.icon;
  if (isActivePath(APPS_NAV.href, pathname)) return APPS_NAV.icon;
  if (isActivePath(WORKFLOWS_NAV.href, pathname)) return WORKFLOWS_NAV.icon;
  const child = DRIVE_CHILDREN.find((c) => c.href === pathname);
  if (child) return child.icon;
  return DRIVE_ICON;
}
