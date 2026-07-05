import {
  Bot,
  FileSearch,
  FileText,
  FolderOpen,
  LayoutGrid,
  type LucideIcon,
  MessageSquareText,
  Share2,
  Sparkles,
  Star,
} from "lucide-react";

/// ホーム下部の機能ショートカット（画像1 の Genspark ワークスペース風）。
/// ロードマップ（docs/requirements.md FR-2〜11）準拠の枠として配置し、backend が
/// 未実装の機能は `ready: false` として「準備中」を明示する（フェイク遷移を作らない）。

export type HomeShortcut = {
  key: string;
  label: string;
  icon: LucideIcon;
  /// 実装済みで遷移できる項目のみ href を持つ。
  href?: string;
  /// false の項目は未実装（押下不可・「準備中」表示）。
  ready: boolean;
};

export type HomeShortcutCategory = {
  key: string;
  label: string;
  items: HomeShortcut[];
};

export const HOME_SHORTCUT_CATEGORIES: HomeShortcutCategory[] = [
  {
    key: "assistant",
    label: "アシスタント",
    items: [
      // FR-4 LLMチャット（本 issue でプレビュー動作）。
      { key: "chat", label: "AIチャット", icon: MessageSquareText, href: "/", ready: true },
      // FR-3 permission-aware RAG（Phase 2 で実装済み）。
      { key: "rag", label: "文書検索", icon: FileSearch, href: "/search", ready: true },
      // FR-5 サンドボックス＆AIエージェント。
      { key: "agent", label: "エージェント", icon: Bot, ready: false },
    ],
  },
  {
    key: "knowledge",
    label: "ナレッジ",
    items: [
      // FR-2 ストレージ（Drive UI は #20、ここはホームへの導線）。
      { key: "drive", label: "ドライブ", icon: FolderOpen, href: "/drive", ready: true },
      { key: "shared", label: "共有済み", icon: Share2, href: "/drive/shared", ready: true },
      { key: "favorites", label: "お気に入り", icon: Star, ready: false },
    ],
  },
  {
    key: "apps",
    label: "ミニアプリ",
    items: [
      // FR-6 / FR-11 generative UI ＆ 業務ミニアプリ基盤。
      { key: "miniapps", label: "ミニアプリ", icon: LayoutGrid, ready: false },
      // FR-7 prompt template（gem/GPTs 相当）。
      { key: "prompts", label: "プロンプト", icon: Sparkles, ready: false },
      // FR-8 資料作成。
      { key: "docs", label: "資料作成", icon: FileText, ready: false },
    ],
  },
];
