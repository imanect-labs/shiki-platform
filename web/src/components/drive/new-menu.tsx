"use client";

import * as React from "react";
import {
  FileSpreadsheet,
  FileText,
  FolderPlus,
  NotebookPen,
  Plus,
  Presentation,
  Upload,
  type LucideIcon,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import { cn } from "@/lib/utils";

/// 新規作成（エディタへ遷移する種類）。チャット「+」と共用の useCreateContent に渡す。
export type CreateHandlers = {
  note: () => void;
  document: () => void;
  sheet: () => void;
  slide: () => void;
  csv: () => void;
};

type CreateItem = {
  key: keyof CreateHandlers;
  label: string;
  icon: LucideIcon;
  color: string;
  testId: string;
};

const CREATE_ITEMS: CreateItem[] = [
  { key: "note", label: "ノート", icon: NotebookPen, color: "text-primary", testId: "new-note" },
  { key: "document", label: "ドキュメント", icon: FileText, color: "text-blue-600", testId: "new-document" },
  {
    key: "sheet",
    label: "スプレッドシート（Excel）",
    icon: FileSpreadsheet,
    color: "text-emerald-600",
    testId: "new-sheet",
  },
  { key: "slide", label: "スライド", icon: Presentation, color: "text-orange-500", testId: "new-slide" },
  { key: "csv", label: "CSV（表・SQL 分析）", icon: FileSpreadsheet, color: "text-green-600", testId: "new-csv" },
];

/// PC のツールバーの丸い「＋」（ノート/ドキュメント/スライド/CSV の新規作成）。
export function NewMenuDesktop({ create }: { create: CreateHandlers }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button size="icon" className="size-9 shrink-0 rounded-full" aria-label="新規作成">
          <Plus className="size-5" aria-hidden />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-52">
        <DropdownMenuLabel>新規作成</DropdownMenuLabel>
        {CREATE_ITEMS.map((item) => (
          <DropdownMenuItem key={item.key} onSelect={() => create[item.key]()} data-testid={item.testId}>
            <item.icon className={item.color} aria-hidden />
            {item.label}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/// スマホの右下の「＋ 新規」と、タップで出る下からのシート（アップロード／新規フォルダ／
/// 各種の新規作成）。PC はツールバーのボタンのまま（md 以上では出さない）。
export function NewFab({
  create,
  onUpload,
  onNewFolder,
}: {
  create: CreateHandlers;
  onUpload: () => void;
  onNewFolder: () => void;
}) {
  const [open, setOpen] = React.useState(false);
  // ファイル選択はユーザー操作の文脈の中で呼ぶ（iOS は遅らせると開かない）。
  const run = (fn: () => void) => {
    setOpen(false);
    fn();
  };
  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        aria-haspopup="dialog"
        aria-expanded={open}
        data-testid="drive-fab"
        className={cn(
          "fixed bottom-[calc(1rem+env(safe-area-inset-bottom))] right-4 z-30 inline-flex h-[54px] items-center gap-2 rounded-full bg-primary pl-4 pr-5 text-[15px] font-semibold text-primary-foreground md:hidden",
          "shadow-[0_8px_24px_-8px_rgb(0_0_0/0.45)] outline-none transition-transform active:scale-[0.96] focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        )}
      >
        <Plus className="size-[22px]" aria-hidden />
        新規
      </button>
      <Sheet open={open} onOpenChange={setOpen}>
        <SheetContent side="bottom" className="gap-0 px-4 pb-[max(1rem,env(safe-area-inset-bottom))] pt-2">
          <div aria-hidden className="mx-auto mb-2 h-1.5 w-9 rounded-full bg-muted-foreground/25" />
          <SheetTitle className="px-1 pb-3 pt-1 text-[17px]">新規</SheetTitle>
          <SheetDescription className="sr-only">
            ファイルのアップロード、フォルダや文書の新規作成ができます。
          </SheetDescription>
          <div className="grid grid-cols-2 gap-2">
            <Tile icon={Upload} label="アップロード" onClick={() => run(onUpload)} />
            <Tile icon={FolderPlus} label="新規フォルダ" onClick={() => run(onNewFolder)} />
          </div>
          <p className="px-1 pb-1 pt-4 text-xs font-medium text-muted-foreground">作成して開く</p>
          <div className="flex flex-col">
            {CREATE_ITEMS.map((item) => (
              <button
                key={item.key}
                type="button"
                data-testid={`${item.testId}-sheet`}
                onClick={() => run(create[item.key])}
                className="flex min-h-[52px] w-full items-center gap-3.5 rounded-lg px-2 text-left text-[15px] outline-none transition-colors active:bg-accent focus-visible:bg-accent"
              >
                <item.icon className={cn("size-5 shrink-0", item.color)} aria-hidden />
                {item.label}
              </button>
            ))}
          </div>
        </SheetContent>
      </Sheet>
    </>
  );
}

function Tile({ icon: Icon, label, onClick }: { icon: LucideIcon; label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex min-h-[76px] flex-col items-center justify-center gap-1.5 rounded-xl bg-muted/60 text-[13px] font-medium outline-none transition-colors active:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
    >
      <Icon className="size-[22px] text-foreground/75" aria-hidden />
      {label}
    </button>
  );
}
