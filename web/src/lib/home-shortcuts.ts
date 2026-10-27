import {
  Database,
  FileSearch,
  FolderOpen,
  LayoutGrid,
  type LucideIcon,
  Share2,
  Sparkles,
  Workflow,
} from "lucide-react";

/// ホーム下部の機能ショートカット。実在する画面への入口だけを置く（「準備中」の枠は作らない・#964）。
/// チャット・エージェント・作成は入力欄の上のモードで選ぶので、ここには並べない。
/// お気に入りは星を付ける機能がまだ無い（/drive/favorites は空の案内だけ）ため載せない。

export type HomeShortcut = {
  key: string;
  label: string;
  icon: LucideIcon;
  href: string;
};

export type HomeShortcutCategory = {
  key: string;
  label: string;
  items: HomeShortcut[];
};

export const HOME_SHORTCUT_CATEGORIES: HomeShortcutCategory[] = [
  {
    key: "knowledge",
    label: "ナレッジ",
    items: [
      { key: "drive", label: "ドライブ", icon: FolderOpen, href: "/drive" },
      { key: "shared", label: "共有済み", icon: Share2, href: "/drive/shared" },
      { key: "rag", label: "文書検索", icon: FileSearch, href: "/search" },
    ],
  },
  {
    key: "apps",
    label: "アプリ",
    items: [
      { key: "miniapps", label: "ミニアプリ", icon: LayoutGrid, href: "/apps" },
      { key: "skills", label: "スキル", icon: Sparkles, href: "/skills" },
      { key: "workflows", label: "ワークフロー", icon: Workflow, href: "/workflows" },
      { key: "data", label: "データ", icon: Database, href: "/data" },
    ],
  },
];
