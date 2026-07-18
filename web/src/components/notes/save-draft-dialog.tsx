"use client";

/// 下書きノートを「ドライブに保存」するダイアログ（issue #282）。
///
/// 名前と保存先フォルダ（既定＝ルート／マイドライブ）を選び、POST /notes で実体化する。
/// 保存先は human 決定「保存先ピッカー（既定=ルート）」に従い、既定はルート直下・任意で
/// フォルダを選べる（後で移動も可）。保存自体は呼び出し側（onConfirm）が行う。

import { Folder, FolderInput } from "lucide-react";
import * as React from "react";

import { FolderPicker } from "@/components/artifacts/folder-picker";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";

export type SaveTarget = { name: string; parentId: string | null };

export function SaveDraftDialog({
  open,
  onOpenChange,
  defaultName,
  saving,
  onConfirm,
  entityLabel = "ノート",
  description = "下書きをノートとして保存します。保存後はバージョン管理・共有・共同編集ができます。",
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  defaultName: string;
  saving: boolean;
  onConfirm: (target: SaveTarget) => void;
  /// 保存対象の呼称（ノート/スライド/CSV・名前欄のラベルに使う）。
  entityLabel?: string;
  /// ダイアログの説明文（対象種別ごとに差し替え）。
  description?: string;
}) {
  const [name, setName] = React.useState(defaultName);
  const [folder, setFolder] = React.useState<{ id: string; name: string } | null>(null);
  const [pickerOpen, setPickerOpen] = React.useState(false);

  // ダイアログを**開いた瞬間だけ**既定名へ戻す（open が false→true の遷移時のみ）。開いたまま
  // defaultName（呼び出し元の activeName）が AI 流し込みで変わっても、ユーザーの入力中の名前/保存先を
  // 黙ってリセットしない。
  const prevOpen = React.useRef(false);
  React.useEffect(() => {
    if (open && !prevOpen.current) {
      setName(defaultName);
      setFolder(null);
    }
    prevOpen.current = open;
  }, [open, defaultName]);

  const canSave = name.trim().length > 0 && !saving;

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>ドライブに保存</DialogTitle>
            <DialogDescription>{description}</DialogDescription>
          </DialogHeader>

          <div className="space-y-3">
            <label className="block space-y-1.5">
              <span className="text-sm font-medium">{entityLabel}名</span>
              <Input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={`${entityLabel}名`}
                data-testid="save-draft-name"
                autoFocus
              />
            </label>

            <div className="space-y-1.5">
              <span className="text-sm font-medium">保存先</span>
              <button
                type="button"
                onClick={() => setPickerOpen(true)}
                data-testid="save-draft-location"
                className="flex w-full items-center gap-2 rounded-md border border-border bg-card px-3 py-2 text-left text-sm transition-colors hover:border-primary/40 hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <Folder className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                <span className="min-w-0 flex-1 truncate">
                  {folder ? folder.name : "マイドライブ（ルート）"}
                </span>
                <FolderInput className="size-4 shrink-0 text-muted-foreground" aria-hidden />
              </button>
              {folder ? (
                <button
                  type="button"
                  onClick={() => setFolder(null)}
                  className="text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
                >
                  ルートに戻す
                </button>
              ) : null}
            </div>
          </div>

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={saving}>
              キャンセル
            </Button>
            <Button
              type="button"
              disabled={!canSave}
              data-testid="save-draft-confirm"
              onClick={() => onConfirm({ name: name.trim(), parentId: folder?.id ?? null })}
            >
              {saving ? "保存中…" : "保存"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <FolderPicker
        open={pickerOpen}
        onOpenChange={setPickerOpen}
        purpose="scope"
        onSelect={(f) => setFolder({ id: f.id, name: f.name })}
      />
    </>
  );
}
