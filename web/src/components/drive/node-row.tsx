"use client";

import * as React from "react";
import { Lock, Users } from "lucide-react";

import type { NodeResponse } from "@/lib/storage";
import { formatBytes, formatDateTime } from "@/lib/format";
import { cn } from "@/lib/utils";

import { NodeActionsMenu, useLongPress, type NodeAction } from "./node-menu";
import { LIST_GRID, NodeIcon, isShared, nodeMetaParts } from "./primitives";

export type { NodeAction };

/// 更新者の表示名。自分は「自分」、他者はディレクトリ解決名、未解決（AI 等）は subject を出す。
export function editorLabel(node: NodeResponse, meId: string | undefined): string {
  if (meId && node.updated_by === meId) return "自分";
  return node.updated_by_name ?? node.updated_by;
}

/// ⋯ の出し方（行・カード共通）。マウスの端末だけホバーまで隠し、タッチでは常に見せる
/// （Tailwind v4 の hover はタッチで発火しないため、隠すと一度も見えない）。
export const ROW_MENU_TRIGGER =
  "transition-opacity pointer-fine:opacity-0 pointer-fine:group-hover:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100";

/// 一覧の 1 行。フォルダは開く、ファイルはビューアで開く。… メニュー（スマホは長押しでも）から各操作。
/// 狭い幅（一覧が `@xl` 未満）では列を出さず、名前の下に種類・更新日・サイズ・共有中を 1 行で出す。
export function NodeRow({
  node,
  onAction,
  meId,
}: {
  node: NodeResponse;
  onAction: (action: NodeAction, node: NodeResponse) => void;
  /// 現在ユーザー id（更新者列の「自分」判定用・親で 1 回だけ解決して渡す）。
  meId?: string;
}) {
  const isFolder = node.kind === "folder";
  const shared = isShared(node);
  const [menuOpen, setMenuOpen] = React.useState(false);
  const longPress = useLongPress(() => setMenuOpen(true));
  // フォルダは開く、ファイルはビューアで開く（ダウンロードはメニューから）。
  const primary = () => onAction("open", node);

  return (
    <div
      className={cn(
        "group shiki-dash-bottom px-3 py-2.5 transition-colors last:bg-none hover:bg-accent",
        // 狭い幅は 2 行（名前＋メタ）で 68px。長押しで iOS の文字選択・吹き出しを出さない。
        "@max-xl:min-h-[68px] pointer-coarse:select-none pointer-coarse:[-webkit-touch-callout:none]",
        menuOpen && "bg-accent",
        LIST_GRID,
      )}
      {...longPress}
    >
      <button
        type="button"
        onClick={primary}
        className="flex min-w-0 items-center gap-3.5 text-left outline-none focus-visible:underline"
      >
        <NodeIcon
          kind={node.kind}
          name={node.name}
          contentType={node.content_type}
          className="size-7 shrink-0 @max-xl:size-8"
        />
        <span className="flex min-w-0 flex-col gap-0.5">
          {/* 広い幅は 1 行で省略、狭い幅は 2 行まで折り返す（長い名前の判別のため）。 */}
          <span className="truncate text-[15px] font-medium leading-snug @max-xl:line-clamp-2 @max-xl:whitespace-normal @max-xl:[overflow-wrap:anywhere]">
            {node.name}
          </span>
          <span className="truncate text-[12.5px] text-muted-foreground @xl:hidden">
            {nodeMetaParts(node).join("・")}
          </span>
        </span>
      </button>

      <span className="hidden truncate text-[13px] text-muted-foreground @xl:block">
        {formatDateTime(node.updated_at)}
      </span>
      <span className="hidden truncate text-[13px] text-muted-foreground @4xl:block">
        {/* 最終更新者（updated_by・11P.10）。自分は「自分」、他者は表示名、AI は主体名義。 */}
        {editorLabel(node, meId)}
      </span>
      <span className="hidden truncate text-[13px] text-muted-foreground @xl:block">
        {isFolder ? "—" : formatBytes(node.size_bytes)}
      </span>
      <span className="hidden items-center gap-1.5 truncate text-[13px] @4xl:flex">
        {/* shared フラグも後続 PR で提供。未提供時は「プライベート」表示に degrade する。 */}
        {shared ? (
          <>
            <Users className="size-3.5 shrink-0 text-foreground/70" aria-hidden />
            <span className="truncate text-foreground/80">共有</span>
          </>
        ) : (
          <>
            <Lock className="size-3.5 shrink-0 text-muted-foreground/55" aria-hidden />
            <span className="truncate text-muted-foreground">プライベート</span>
          </>
        )}
      </span>

      <NodeActionsMenu
        node={node}
        onAction={onAction}
        open={menuOpen}
        onOpenChange={setMenuOpen}
        triggerClassName={ROW_MENU_TRIGGER}
      />
    </div>
  );
}
