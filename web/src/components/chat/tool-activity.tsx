"use client";

import {
  Loader2,
  Search,
  Check,
  Terminal,
  Globe,
  FileDown,
  FileText,
  FilePlus,
  FilePen,
  Trash2,
  Files,
  ListChecks,
  Sparkles,
} from "lucide-react";

import { cn } from "@/lib/utils";

export type ToolActivityItem = {
  id: string;
  name: string;
  running: boolean;
  /// ツール入力（doc_search なら `{ query }`）。CoT で検索クエリを見せるのに使う。
  input?: unknown;
};

const TOOL_LABEL: Record<string, { label: string; icon: typeof Search }> = {
  doc_search: { label: "社内文書を検索", icon: Search },
  code_interpreter: { label: "コードを実行", icon: Terminal },
  web_search: { label: "web を検索", icon: Globe },
  web_fetch: { label: "ページを取得", icon: FileDown },
  // 自律エージェントのフルツール（Task 5.4）。
  fs_list: { label: "ファイル一覧", icon: Files },
  fs_read: { label: "ファイルを読む", icon: FileText },
  fs_write: { label: "ファイルを書く", icon: FilePlus },
  fs_edit: { label: "ファイルを編集", icon: FilePen },
  fs_delete: { label: "ファイルを削除", icon: Trash2 },
  grep: { label: "ファイルを検索", icon: Search },
  shell: { label: "コマンドを実行", icon: Terminal },
  plan: { label: "計画を更新", icon: ListChecks },
  skill: { label: "スキルを読み込み", icon: Sparkles },
};

/// ツール入力から表示に添える対象名を取り出す（skill ならスキル名・#344）。
function itemDetail(it: ToolActivityItem): string | null {
  if (it.name !== "skill") return null;
  const name = (it.input as { name?: unknown } | undefined)?.name;
  // バックエンド（SkillTool::call）は trim して解決するため、表示も同じ正規化を通す。
  return typeof name === "string" ? name.trim() || null : null;
}

/// エージェントのツール実行（Chain of Thought）を可視化する。検索中はスピナー、完了はチェック。
export function ToolActivity({ items }: { items: ToolActivityItem[] }) {
  if (items.length === 0) return null;
  return (
    <div className="mb-2 flex flex-col gap-1.5">
      {items.map((it) => {
        const meta = TOOL_LABEL[it.name] ?? { label: it.name, icon: Search };
        const Icon = meta.icon;
        const detail = itemDetail(it);
        return (
          <div
            key={it.id}
            className={cn(
              "inline-flex w-fit items-center gap-2 rounded-full border border-border bg-card px-3 py-1 text-[13px]",
              it.running ? "text-foreground" : "text-muted-foreground",
            )}
          >
            <Icon className="size-3.5 text-primary" aria-hidden />
            <span>
              {meta.label}
              {detail ? (
                <span className="ml-1 font-medium text-foreground">「{detail}」</span>
              ) : null}
            </span>
            {it.running ? (
              <Loader2 className="size-3.5 animate-spin text-muted-foreground" aria-hidden />
            ) : (
              <Check className="size-3.5 text-primary" aria-hidden />
            )}
          </div>
        );
      })}
    </div>
  );
}
