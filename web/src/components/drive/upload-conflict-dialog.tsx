"use client";

import { CopyPlus, History, SkipForward, type LucideIcon } from "lucide-react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

import { NodeIcon, nodeMetaParts } from "./primitives";
import { numberedName, type ConflictResolution, type UploadConflict } from "./use-drive-uploads";

/// 同名アップロードの問い合わせ（スマホは下からのシート・PC は中央のダイアログ）。
/// 選んだ扱いを衝突した全件に当てる。閉じる（×・外側・Esc）は「スキップ」と同じ。
export function UploadConflictDialog({
  conflicts,
  onResolve,
}: {
  conflicts: UploadConflict[];
  onResolve: (resolution: ConflictResolution) => void;
}) {
  const open = conflicts.length > 0;
  const count = conflicts.length;
  // 新しいバージョンにできる（既存がファイルとして特定できた）ものがあるか。
  const versionable = conflicts.filter((c) => c.existing && c.existing.kind !== "folder").length;
  const sample = conflicts[0]?.file.name ?? "";

  return (
    <Dialog open={open} onOpenChange={(o) => (o ? null : onResolve("skip"))}>
      <DialogContent className="max-w-md gap-3" data-testid="upload-conflict">
        <DialogHeader>
          <DialogTitle className="leading-snug">
            同じ名前のファイルが {count} 件あります
          </DialogTitle>
          <DialogDescription>
            保存先のフォルダに同じ名前があります。どのように保存するか選んでください。
          </DialogDescription>
        </DialogHeader>

        <ul className="flex min-w-0 flex-col rounded-lg border border-border" aria-label="同じ名前のファイル">
          {conflicts.map((c) => (
            <li key={c.id} className="shiki-dash-bottom flex items-center gap-3 px-3 py-2.5 last:bg-none">
              <NodeIcon
                kind={c.existing?.kind ?? "file"}
                name={c.file.name}
                contentType={c.existing?.content_type ?? c.file.type}
                className="size-7 shrink-0"
              />
              <div className="min-w-0 flex-1">
                <p className="line-clamp-2 text-sm font-medium [overflow-wrap:anywhere]">{c.file.name}</p>
                <p className="truncate text-xs text-muted-foreground">
                  {c.existing
                    ? `既存: ${nodeMetaParts(c.existing).join("・")}`
                    : "既存のファイルを確認できませんでした"}
                </p>
              </div>
            </li>
          ))}
        </ul>

        <div className="flex flex-col gap-1.5" role="group" aria-label="保存のしかた">
          {versionable > 0 ? (
            <Choice
              icon={History}
              label="新しいバージョンとして保存"
              description={
                versionable === count
                  ? "既存のファイルを更新します。前の版は版履歴から戻せます。"
                  : "既存のファイルを更新します（フォルダと同名のものは名前を変えて保存）。前の版は版履歴から戻せます。"
              }
              onClick={() => onResolve("version")}
              primary
              testId="conflict-version"
            />
          ) : null}
          <Choice
            icon={CopyPlus}
            label="両方残す"
            description={`「${numberedName(sample, 1)}」のように名前を変えて保存します。`}
            onClick={() => onResolve("both")}
            testId="conflict-both"
          />
          <Choice
            icon={SkipForward}
            label="スキップ"
            description="これらのファイルはアップロードしません。"
            onClick={() => onResolve("skip")}
            testId="conflict-skip"
          />
        </div>
      </DialogContent>
    </Dialog>
  );
}

function Choice({
  icon: Icon,
  label,
  description,
  onClick,
  primary,
  testId,
}: {
  icon: LucideIcon;
  label: string;
  description: string;
  onClick: () => void;
  primary?: boolean;
  testId: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      data-testid={testId}
      className={cn(
        "flex min-h-[56px] w-full items-center gap-3 rounded-lg border px-3 py-2.5 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
        primary
          ? "border-transparent bg-accent hover:bg-accent/80 active:bg-accent/70"
          : "border-border hover:bg-accent/50 active:bg-accent",
      )}
    >
      <Icon className="size-5 shrink-0 text-foreground/75" aria-hidden />
      <span className="min-w-0 flex-1">
        <span className="block text-[15px] font-medium leading-tight">{label}</span>
        <span className="mt-0.5 block text-xs text-muted-foreground">{description}</span>
      </span>
    </button>
  );
}
