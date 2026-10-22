"use client";

/// 「このファイルはプレビューできません」の案内（Google ドライブ相当・#775）。
///
/// 開いただけではダウンロードを始めず、ファイルの情報と「ダウンロード」ボタンを出す。
/// 形式は表示できるはずでも、ブラウザが扱えない・大きすぎる・中身がバイナリだった場合もここに来る。
import Link from "next/link";
import { Download, EyeOff, FolderOpen, Loader2 } from "lucide-react";

import { NodeIcon, nodeKindLabel } from "@/components/drive/primitives";
import { Button } from "@/components/ui/button";
import { formatBytes } from "@/lib/format";
import type { NodeResponse } from "@/lib/storage";

const dateFormatter = new Intl.DateTimeFormat("ja-JP", {
  year: "numeric",
  month: "numeric",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

export function NoPreview({
  node,
  onDownload,
  downloading,
  title = "このファイルはプレビューできません",
  description = "ダウンロードして、お使いのアプリで開いてください。",
}: {
  node: NodeResponse;
  onDownload: () => void;
  downloading: boolean;
  title?: string;
  description?: string;
}) {
  const folderHref = node.parent_id ? `/drive?folder=${node.parent_id}` : "/drive";
  const meta = [nodeKindLabel(node), formatBytes(node.size_bytes), `${dateFormatter.format(new Date(node.updated_at))} 更新`]
    .filter(Boolean)
    .join("・");

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex min-h-0 flex-1 items-center justify-center overflow-auto bg-muted/40 p-4 sm:p-6">
        <div
          className="flex w-full max-w-md flex-col items-center gap-5 rounded-2xl border border-border/60 bg-card px-6 py-10 text-center shadow-sm"
          data-testid="file-no-preview"
        >
          <div className="flex size-16 items-center justify-center rounded-2xl bg-muted">
            <NodeIcon kind="file" name={node.name} contentType={node.content_type} className="size-8" />
          </div>
          <div className="flex min-w-0 max-w-full flex-col gap-1">
            <h1 className="break-words text-base font-semibold leading-snug [overflow-wrap:anywhere]">{node.name}</h1>
            <p className="text-xs text-muted-foreground">{meta}</p>
          </div>
          <div className="flex flex-col items-center gap-1">
            <p className="inline-flex items-center gap-1.5 text-sm font-medium text-foreground">
              <EyeOff className="size-4 text-muted-foreground" aria-hidden />
              {title}
            </p>
            <p className="text-sm text-muted-foreground">{description}</p>
          </div>
          <div className="flex flex-wrap items-center justify-center gap-2">
            <Button onClick={onDownload} disabled={downloading} data-testid="file-download">
              {downloading ? <Loader2 className="animate-spin" aria-hidden /> : <Download aria-hidden />}
              ダウンロード
            </Button>
            <Button asChild variant="outline">
              <Link href={folderHref}>
                <FolderOpen aria-hidden />
                フォルダを開く
              </Link>
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
