"use client";

/// ノートのエクスポートメニュー（#334）。md / docx / pdf の 3 形式を提供する。
///
/// - md/docx はエディタ state から生成しダウンロード（docx はサーバ変換＋シキ静的化）。
/// - pdf は専用プリントビュー（/notes/{id}/print）を新規タブで開き、ブラウザ印刷で PDF 化する。
///   （human 決定: ブラウザ印刷ベース。チャートは SVG のままベクタ印刷され、iframe は
///   プレースホルダへ差し替わる。）

import { Download, FileType, FileText, Loader2, Printer } from "lucide-react";
import * as React from "react";
import type { Editor } from "@tiptap/react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { toast } from "@/components/ui/use-toast";
import { exportNoteDocx, exportNoteMarkdown } from "@/lib/notes/export-note";

export function NoteExportMenu({
  editor,
  nodeId,
  name,
}: {
  /// エクスポート対象のエディタ（未生成なら無効化）。
  editor: Editor | null;
  nodeId: string;
  /// 拡張子なしの表示名（ダウンロードファイル名の素）。
  name: string;
}) {
  const [busy, setBusy] = React.useState(false);

  const onMd = () => {
    if (!editor) return;
    exportNoteMarkdown(editor, name);
  };

  const onDocx = async () => {
    if (!editor || busy) return;
    setBusy(true);
    try {
      await exportNoteDocx(editor, name);
    } catch (e) {
      toast({
        variant: "destructive",
        description: e instanceof Error ? e.message : "docx のエクスポートに失敗しました。",
      });
    } finally {
      setBusy(false);
    }
  };

  const onPdf = () => {
    // 印刷専用ビューを開く（同期完了後に自動で印刷ダイアログを出す）。
    window.open(`/notes/${nodeId}/print`, "_blank", "noopener");
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          disabled={!editor || busy}
          aria-label="エクスポート"
          data-testid="note-export"
          className="inline-flex h-8 items-center gap-1.5 rounded-md px-2.5 text-sm font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
        >
          {busy ? (
            <Loader2 className="size-4 animate-spin" aria-hidden />
          ) : (
            <Download className="size-4" aria-hidden />
          )}
          エクスポート
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-48 p-1.5">
        <DropdownMenuLabel className="uppercase tracking-wide">エクスポート</DropdownMenuLabel>
        <DropdownMenuItem className="gap-2.5 px-2.5 py-2" onSelect={onPdf} data-testid="note-export-pdf">
          <Printer className="text-muted-foreground" />
          PDF
        </DropdownMenuItem>
        <DropdownMenuItem className="gap-2.5 px-2.5 py-2" onSelect={onMd} data-testid="note-export-md">
          <FileText className="text-muted-foreground" />
          Markdown (.md)
        </DropdownMenuItem>
        <DropdownMenuItem
          className="gap-2.5 px-2.5 py-2"
          onSelect={() => void onDocx()}
          data-testid="note-export-docx"
        >
          <FileType className="text-blue-600" />
          Word (.docx)
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
