"use client";

/// pptx エクスポートのダイアログ（Task 11.4・PIT-42 の変換レポート可視化）。
///
/// 実行 → 変換レポート（何要素をネイティブ変換・何要素を画像化したか）を表示し、
/// ダウンロード or ドライブ保存（既存アップロード API → Collabora で再編集可能）を選ぶ。

import { Download, FileUp, Loader2 } from "lucide-react";
import * as React from "react";
import type * as Y from "yjs";

import type { ExportReport } from "@/components/slides/editor-bridge";
import {
  downloadBlob,
  exportDeckToPptx,
  readSlidesForExport,
} from "@/components/slides/slide-export";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast } from "@/components/ui/use-toast";
import { uploadFile } from "@/lib/storage";

type Phase =
  | { kind: "running" }
  | { kind: "done"; blob: Blob; report: ExportReport }
  | { kind: "error"; message: string };

export function ExportDialog({
  doc,
  name,
  open,
  onClose,
}: {
  doc: Y.Doc;
  /// ファイル名（拡張子なし・pptx 名の既定に使う）。
  name: string;
  open: boolean;
  onClose: () => void;
}) {
  const [phase, setPhase] = React.useState<Phase>({ kind: "running" });
  const [saving, setSaving] = React.useState(false);

  React.useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setPhase({ kind: "running" });
    const slides = readSlidesForExport(doc);
    exportDeckToPptx(slides, name)
      .then(({ blob, report }) => {
        if (!cancelled) setPhase({ kind: "done", blob, report });
      })
      .catch((e: unknown) => {
        if (!cancelled)
          setPhase({ kind: "error", message: e instanceof Error ? e.message : String(e) });
      });
    return () => {
      cancelled = true;
    };
  }, [open, doc, name]);

  const fileName = `${name}.pptx`;

  const saveToDrive = async (blob: Blob) => {
    setSaving(true);
    try {
      const file = new File([blob], fileName, {
        type: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
      });
      await uploadFile({ file });
      toast({ title: "ドライブに保存しました", description: fileName });
      onClose();
    } catch (e) {
      toast({
        variant: "destructive",
        title: "ドライブ保存に失敗しました",
        description: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-md" data-testid="slide-export-dialog">
        <DialogHeader>
          <DialogTitle>PowerPoint へエクスポート</DialogTitle>
          <DialogDescription>
            テキスト・表・図形は編集可能なネイティブ要素として変換されます。
          </DialogDescription>
        </DialogHeader>

        {phase.kind === "running" ? (
          <div className="flex items-center gap-2 py-6 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" aria-hidden />
            変換しています…（スライドの計測と組み立て）
          </div>
        ) : null}

        {phase.kind === "error" ? (
          <p className="py-4 text-sm text-destructive">{phase.message}</p>
        ) : null}

        {phase.kind === "done" ? (
          <div className="space-y-2 py-2 text-sm" data-testid="slide-export-report">
            <p>
              {phase.report.slides} 枚のスライドを変換しました（テキスト {phase.report.texts}・表{" "}
              {phase.report.tables}・画像 {phase.report.images}・図形 {phase.report.shapes}）。
            </p>
            {phase.report.rasterized > 0 ? (
              <p className="text-muted-foreground">
                {phase.report.rasterized}{" "}
                個の要素はネイティブ変換できず画像として埋め込みました（パワポでの再編集不可）。
              </p>
            ) : (
              <p className="text-muted-foreground">
                すべての要素が編集可能な形式で変換されました。
              </p>
            )}
          </div>
        ) : null}

        <DialogFooter className="gap-2 sm:gap-2">
          {phase.kind === "done" ? (
            <>
              <Button
                type="button"
                variant="outline"
                disabled={saving}
                onClick={() => void saveToDrive(phase.blob)}
                data-testid="slide-export-save-drive"
              >
                {saving ? (
                  <Loader2 className="mr-1.5 size-4 animate-spin" aria-hidden />
                ) : (
                  <FileUp className="mr-1.5 size-4" aria-hidden />
                )}
                ドライブに保存
              </Button>
              <Button
                type="button"
                onClick={() => downloadBlob(phase.blob, fileName)}
                data-testid="slide-export-download"
              >
                <Download className="mr-1.5 size-4" aria-hidden />
                ダウンロード
              </Button>
            </>
          ) : (
            <Button type="button" variant="outline" onClick={onClose}>
              閉じる
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
