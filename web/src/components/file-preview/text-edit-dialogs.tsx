"use client";

/// テキスト編集の確認（#827）: 保存の衝突と、未保存の編集の破棄。
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

/// 開いた後に、誰かが新しい版を保存していた。
export function ConflictDialog({
  open,
  onClose,
  onOverwrite,
  onReload,
}: {
  open: boolean;
  onClose: () => void;
  onOverwrite: () => void;
  onReload: () => void;
}) {
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent data-testid="text-conflict-dialog">
        <DialogHeader>
          <DialogTitle>ほかの保存で更新されています</DialogTitle>
          <DialogDescription>
            このファイルを開いた後に、新しい版が保存されました。このまま保存すると、その版の内容はあなたの内容で置き換わります（前の版は版の履歴に残ります）。
          </DialogDescription>
        </DialogHeader>
        <DialogFooter className="gap-2 sm:gap-2">
          <Button variant="outline" onClick={onReload} data-testid="text-conflict-reload">
            最新版を開く（編集を破棄）
          </Button>
          <Button onClick={onOverwrite} data-testid="text-conflict-overwrite">
            このまま保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// 未保存の編集を捨てて、編集を終える・画面を離れる。
export function DiscardDialog({
  open,
  onClose,
  onDiscard,
}: {
  open: boolean;
  onClose: () => void;
  onDiscard: () => void;
}) {
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent data-testid="text-discard-dialog">
        <DialogHeader>
          <DialogTitle>保存していない変更があります</DialogTitle>
          <DialogDescription>変更を破棄すると、元に戻せません。</DialogDescription>
        </DialogHeader>
        <DialogFooter className="gap-2 sm:gap-2">
          <Button variant="outline" onClick={onClose} data-testid="text-discard-cancel">
            編集を続ける
          </Button>
          <Button variant="destructive" onClick={onDiscard} data-testid="text-discard-confirm">
            変更を破棄
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
