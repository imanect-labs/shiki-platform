"use client";

import * as React from "react";
import {
  Download,
  Eye,
  FilePlus2,
  FolderInput,
  FolderOpen,
  History,
  MoreVertical,
  Pencil,
  Share2,
  Trash2,
  type LucideIcon,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import { useMediaQuery } from "@/hooks/use-media-query";
import type { NodeResponse } from "@/lib/storage";
import { cn } from "@/lib/utils";

import { NodeIcon, nodeMetaParts } from "./primitives";

export type NodeAction =
  | "open"
  | "download"
  | "newversion"
  | "rename"
  | "move"
  | "share"
  | "versions"
  | "delete";

/// シェルの浮いたクロームと同じ切替幅（md 未満をスマホとして扱う）。
export const MOBILE_QUERY = "(max-width: 767.98px)";

type ActionDef = { action: NodeAction; label: string; icon: LucideIcon };

/// ノードに出せる操作を、スマホのアクションシートの段組み（大きなタイル 4 つ／行／破壊的操作）
/// に分けて返す。PC のドロップダウンも同じ定義から並べる（項目の増減を 1 箇所で済ませる）。
function actionGroups(node: NodeResponse): { tiles: ActionDef[]; rows: ActionDef[] } {
  const isFolder = node.kind === "folder";
  if (isFolder) {
    return {
      tiles: [
        { action: "open", label: "開く", icon: FolderOpen },
        { action: "share", label: "共有", icon: Share2 },
        { action: "rename", label: "名前を変更", icon: Pencil },
        { action: "move", label: "移動", icon: FolderInput },
      ],
      rows: [],
    };
  }
  return {
    tiles: [
      { action: "open", label: "開く", icon: Eye },
      { action: "share", label: "共有", icon: Share2 },
      { action: "download", label: "ダウンロード", icon: Download },
      { action: "rename", label: "名前を変更", icon: Pencil },
    ],
    rows: [
      { action: "move", label: "移動", icon: FolderInput },
      { action: "newversion", label: "新しいバージョン", icon: FilePlus2 },
      { action: "versions", label: "版履歴", icon: History },
    ],
  };
}

/// ノードの操作メニュー（行・カードで共有）。トリガは … ボタン。
///
/// - PC: 従来どおりのドロップダウン（表示位置は align で調整）。
/// - スマホ: 下からのアクションシート。先頭に対象の名前とメタ情報、よく使う 4 操作を大きな
///   タイル、その下に行、区切って最下段に「ゴミ箱へ移動」（確認は呼び出し側のダイアログ）。
///
/// `open`/`onOpenChange` を渡すと開閉を外から制御できる（行の長押しで同じシートを開く）。
export function NodeActionsMenu({
  node,
  onAction,
  align = "end",
  triggerClassName,
  open: openProp,
  onOpenChange,
}: {
  node: NodeResponse;
  onAction: (action: NodeAction, node: NodeResponse) => void;
  align?: "start" | "center" | "end";
  triggerClassName?: string;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const mobile = useMediaQuery(MOBILE_QUERY);
  const [openState, setOpenState] = React.useState(false);
  const open = openProp ?? openState;
  const setOpen = (v: boolean) => {
    setOpenState(v);
    onOpenChange?.(v);
  };
  const { tiles, rows } = actionGroups(node);

  const trigger = (
    <Button
      variant="ghost"
      size="icon"
      className={cn("size-8 pointer-coarse:size-10", triggerClassName)}
      aria-label={`「${node.name}」の操作`}
      // スマホのシートでは Radix のトリガ属性が付かないため、開いている状態は自前で示す。
      data-state={open ? "open" : "closed"}
      aria-haspopup={mobile ? "dialog" : undefined}
      onClick={mobile ? () => setOpen(true) : undefined}
    >
      <MoreVertical className="size-4" aria-hidden />
    </Button>
  );

  if (mobile) {
    // 選んだ操作は同期で呼ぶ（「新しいバージョン」のファイル選択はユーザー操作の文脈が
    // 切れると iOS で開かないため、setTimeout 等で遅らせない）。
    const run = (action: NodeAction) => {
      setOpen(false);
      onAction(action, node);
    };
    return (
      <>
        {trigger}
        <Sheet open={open} onOpenChange={setOpen}>
          <SheetContent
            side="bottom"
            className="gap-0 px-4 pb-[max(1rem,env(safe-area-inset-bottom))] pt-5.5"
            data-testid="node-action-sheet"
          >
            <div className="flex items-center gap-3 pb-3 pr-10">
              <NodeIcon
                kind={node.kind}
                name={node.name}
                contentType={node.content_type}
                className="size-9 shrink-0"
              />
              <div className="min-w-0 flex-1">
                <SheetTitle className="line-clamp-2 break-all text-[15px] leading-snug">
                  {node.name}
                </SheetTitle>
                <SheetDescription className="mt-1 truncate text-[12.5px]">
                  {nodeMetaParts(node).join("・")}
                </SheetDescription>
              </div>
            </div>

            <div className="grid grid-cols-4 gap-2">
              {tiles.map((t) => (
                <button
                  key={t.action}
                  type="button"
                  onClick={() => run(t.action)}
                  className="flex min-h-[76px] flex-col items-center justify-center gap-1.5 rounded-xl bg-muted/60 px-1 text-[12px] font-medium text-foreground outline-none transition-colors active:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <t.icon className="size-[22px] text-foreground/75" aria-hidden />
                  <span className="leading-tight">{t.label}</span>
                </button>
              ))}
            </div>

            {rows.length > 0 ? (
              <div className="mt-2 flex flex-col">
                {rows.map((r) => (
                  <SheetRow key={r.action} icon={r.icon} label={r.label} onClick={() => run(r.action)} />
                ))}
              </div>
            ) : null}

            <div className="shiki-dash-x my-1.5" />
            <SheetRow
              icon={Trash2}
              label="ゴミ箱へ移動"
              destructive
              onClick={() => run("delete")}
            />
          </SheetContent>
        </Sheet>
      </>
    );
  }

  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
      <DropdownMenuContent align={align}>
        {[...tiles, ...rows]
          // PC は従来の並び（開く・ダウンロード・名前を変更・移動・共有・新しいバージョン・版履歴）。
          .sort((a, b) => DESKTOP_ORDER.indexOf(a.action) - DESKTOP_ORDER.indexOf(b.action))
          // フォルダの「開く」は行クリックと同じなので PC のメニューには出さない（従来どおり）。
          .filter((d) => !(node.kind === "folder" && d.action === "open"))
          .map((d) => (
            <DropdownMenuItem key={d.action} onSelect={() => onAction(d.action, node)}>
              <d.icon aria-hidden />
              {d.label}
            </DropdownMenuItem>
          ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem variant="destructive" onSelect={() => onAction("delete", node)}>
          <Trash2 aria-hidden />
          ゴミ箱へ移動
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

const DESKTOP_ORDER: NodeAction[] = [
  "open",
  "download",
  "rename",
  "move",
  "share",
  "newversion",
  "versions",
];

function SheetRow({
  icon: Icon,
  label,
  destructive,
  onClick,
}: {
  icon: LucideIcon;
  label: string;
  destructive?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "flex min-h-[52px] w-full items-center gap-3.5 rounded-lg px-2 text-left text-[15px] outline-none transition-colors active:bg-accent focus-visible:bg-accent",
        destructive ? "text-destructive" : "text-foreground",
      )}
    >
      <Icon
        className={cn("size-5 shrink-0", destructive ? "text-destructive" : "text-foreground/70")}
        aria-hidden
      />
      {label}
    </button>
  );
}

/// タッチの長押し（約 0.5 秒）で `onLongPress` を呼ぶハンドラ群。指が動いたら（スクロール）
/// 取り消し、発火した直後のクリック（行を開く）は握りつぶす。マウス・ペンでは何もしない
/// （PC は ⋯ と右クリックの既定動作のまま）。
///
/// React の合成イベントはポータル（スマホの操作シート等）からも React ツリー上の祖先へ伝わる。
/// シート内の項目の長押しで行の長押しが発火したり、行の onClickCapture がシート内の click を
/// 握りつぶしたりしないよう、DOM 上で要素の外から来たイベントは無視する。
export function useLongPress(onLongPress: () => void) {
  const timer = React.useRef<ReturnType<typeof setTimeout> | null>(null);
  const start = React.useRef<{ x: number; y: number } | null>(null);
  const fired = React.useRef(false);
  const cbRef = React.useRef(onLongPress);
  cbRef.current = onLongPress;

  const cancel = React.useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    start.current = null;
  }, []);

  React.useEffect(() => cancel, [cancel]);

  const fromOutside = (e: React.SyntheticEvent) =>
    !(e.currentTarget as Node).contains(e.target as Node);

  return {
    onPointerDown: (e: React.PointerEvent) => {
      if (e.pointerType !== "touch" || fromOutside(e)) return;
      fired.current = false;
      start.current = { x: e.clientX, y: e.clientY };
      timer.current = setTimeout(() => {
        fired.current = true;
        timer.current = null;
        cbRef.current();
      }, 500);
    },
    onPointerMove: (e: React.PointerEvent) => {
      if (!start.current || fromOutside(e)) return;
      if (Math.hypot(e.clientX - start.current.x, e.clientY - start.current.y) > 10) cancel();
    },
    onPointerUp: cancel,
    onPointerCancel: cancel,
    // Android は長押しで contextmenu を出す。こちらのシートと二重にならないよう止める。
    onContextMenu: (e: React.MouseEvent) => {
      if (fromOutside(e)) return;
      if (fired.current || timer.current) e.preventDefault();
    },
    onClickCapture: (e: React.MouseEvent) => {
      if (fromOutside(e)) return;
      if (fired.current) {
        fired.current = false;
        e.preventDefault();
        e.stopPropagation();
      }
    },
  };
}
