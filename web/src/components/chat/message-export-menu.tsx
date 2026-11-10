"use client";

/// 回答の書き出しメニュー（#1048）。フッターの 1 ボタンから、行き先を選ぶ。
///
/// - エクスポート（ドライブに置く）: ノートとして開く（下書きノートへ流し込み、保存は下書き画面で
///   決める）／Word としてドライブに保存（名前と保存先を選んで .docx を置く）。
/// - ダウンロード（手元に保存）: Markdown・Word・PDF（回答をその場で印刷する・[`ExportPrint`]）。
///
/// 操作の中身は会話全体の書き出しと共用する（[`useExportActions`]）。

import { FileDown, FileOutput, FileText, FileType, Loader2, NotebookPen, Printer } from "lucide-react";
import * as React from "react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { answerFileName, answerMarkdown, hasDocumentSources } from "@/lib/answer-export";
import type { Citation } from "@/lib/chat-api";
import { ExportPrint } from "./answer-print";
import { useMessageCitations } from "./citations/citation-context";
import { useExportActions } from "./export-actions";
import { answerImageSources, answerLinkSourcesOf, type TimelineSegment } from "./message-timeline";
import { footerButtonClass } from "./message-footer";

export function MessageExportMenu({
  threadId,
  threadTitle,
  segments,
  text,
  citations,
}: {
  threadId: string;
  threadTitle: string | null;
  /// 回答の区切り（印刷で、出典に現れた画像だけを描くのに使う）。
  segments: TimelineSegment[];
  text: string;
  citations: Citation[];
}) {
  const cited = useMessageCitations();
  const actions = useExportActions({
    threadId,
    what: "回答",
    // ファイル名・Markdown は押したときだけ求める（会話の全回答を描くたびに本文を解析しない）。
    fileName: () => answerFileName(text, threadTitle),
    // 出典の名前は、押した時点で解決できているものを使う（一覧の表示と同じ）。
    markdown: () => answerMarkdown(text, citations, cited?.nameOf ?? (() => "出典")),
    hasDocSources: hasDocumentSources(citations),
  });

  // PDF: その場で印刷する（会話を取り直さない）。押している間は層を出し、閉じたら外す。
  const [printing, setPrinting] = React.useState(false);
  const printItems = React.useMemo(
    () =>
      printing
        ? [{ key: "answer", question: null, text, citations, imageSources: answerImageSources(segments, citations), linkSources: answerLinkSourcesOf(segments, citations) }]
        : null,
    [printing, segments, text, citations],
  );

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          {/* Word の変換中も他の行き先は選べる（押せなくするのは Word の項目だけ・#1071）。 */}
          <button
            type="button"
            aria-label="書き出し"
            title="書き出し"
            data-testid="message-export"
            className={footerButtonClass()}
          >
            {actions.converting ? (
              <Loader2 className="size-3.5 animate-spin" aria-hidden />
            ) : (
              <FileOutput className="size-3.5" aria-hidden />
            )}
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-60 p-1.5">
          <DropdownMenuLabel>エクスポート</DropdownMenuLabel>
          <DropdownMenuItem
            className="gap-2.5 px-2.5 py-2"
            onSelect={() => void actions.openAsNote()}
            data-testid="message-export-note"
          >
            <NotebookPen className="text-muted-foreground" />
            ノートとして開く
          </DropdownMenuItem>
          <DropdownMenuItem
            className="gap-2.5 px-2.5 py-2"
            onSelect={() => void actions.openSaveDocx()}
            disabled={actions.saving}
            data-testid="message-export-docx-drive"
          >
            <FileType className="text-blue-600 dark:text-blue-400" />
            {actions.saving ? "ドライブに保存しています…" : "Word としてドライブに保存…"}
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuLabel>ダウンロード</DropdownMenuLabel>
          <DropdownMenuItem
            className="gap-2.5 px-2.5 py-2"
            onSelect={() => void actions.downloadMd()}
            data-testid="message-export-md"
          >
            <FileText className="text-muted-foreground" />
            Markdown (.md)
          </DropdownMenuItem>
          <DropdownMenuItem
            className="gap-2.5 px-2.5 py-2"
            onSelect={() => void actions.downloadDocx()}
            disabled={actions.converting}
            data-testid="message-export-docx"
          >
            <FileDown className="text-muted-foreground" />
            {actions.converting ? "Word に変換しています…" : "Word (.docx)"}
          </DropdownMenuItem>
          <DropdownMenuItem
            className="gap-2.5 px-2.5 py-2"
            onSelect={() => setPrinting(true)}
            data-testid="message-export-pdf"
          >
            <Printer className="text-muted-foreground" />
            PDF
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {actions.dialog}
      {printItems ? (
        <ExportPrint
          title={threadTitle}
          subtitle="Shiki の回答"
          items={printItems}
          onDone={() => setPrinting(false)}
        />
      ) : null}
    </>
  );
}
