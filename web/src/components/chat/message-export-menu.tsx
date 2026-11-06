"use client";

/// 回答の書き出しメニュー（#1048）。フッターの 1 ボタンから、行き先を選ぶ。
///
/// - エクスポート（ドライブに置く）: ノートとして開く（下書きノートへ流し込み、保存は下書き画面で
///   決める）／Word としてドライブに保存（名前と保存先を選んで .docx を置く）。
/// - ダウンロード（手元に保存）: Markdown・Word・PDF（回答をその場で印刷する・[`AnswerPrint`]）。
///
/// 出典付きの回答には、閲覧権限のある人だけが読める文書の抜粋が入りうる。ドライブに置くと
/// 共有で出典を読めない人にも見えるので、保存ダイアログで注意を出す（human 決定: UI の注意に留める）。

import { FileDown, FileOutput, FileText, FileType, Loader2, NotebookPen, Printer } from "lucide-react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import * as React from "react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { toast } from "@/components/ui/use-toast";
import {
  SaveDraftDialog,
  SOURCES_NOTICE,
  saveFailedMessage,
  type SaveTarget,
} from "@/components/notes/save-draft-dialog";
import { answerFileName, answerMarkdown, docxMarkdown, noteMarkdown } from "@/lib/answer-export";
import type { Citation } from "@/lib/chat-api";
import { isWebCitation } from "@/lib/citation";
import { markdownToDocx } from "@/lib/documents-api";
import { downloadBlob } from "@/lib/download";
import { useDefaultSaveFolder } from "@/lib/drafts/save-folder";
import { draftHref } from "@/lib/notes/draft-nav";
import { getDraft, upsertDraft } from "@/lib/notes/draft-store";
import { uploadNewFile } from "@/lib/storage";
import { AnswerPrint } from "./answer-print";
import { useMessageCitations } from "./citations/citation-context";
import { answerImageSources, type TimelineSegment } from "./message-timeline";
import { footerButtonClass } from "./message-footer";

const DOCX_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

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
  const router = useRouter();
  const cited = useMessageCitations();
  const [busy, setBusy] = React.useState(false);
  const [saveOpen, setSaveOpen] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const defaultFolder = useDefaultSaveFolder(threadId, undefined, saveOpen);

  const name = React.useMemo(() => answerFileName(text, threadTitle), [text, threadTitle]);
  // 出典の名前は、押した時点で解決できているものを使う（一覧の表示と同じ）。
  const markdown = () => answerMarkdown(text, citations, cited?.nameOf ?? (() => "出典"));
  // 共有の注意は社内文書を出典に持つときだけ（Web ページは誰でも読めるので注意しない）。閲覧できない
  // 出典（withheld）は一覧には出ないが、本文はその文書を元に書かれているので注意の対象に含める。
  const hasDocSources = citations.some((c) => c.withheld || !isWebCitation(c));

  const openAsNote = () => {
    // 同じ会話で同じ名前の下書きがあれば、上書きせず連番の名前で開く。
    let draftName = name;
    for (let i = 2; getDraft(threadId, draftName); i++) draftName = `${name} (${i})`;
    upsertDraft(threadId, draftName, noteMarkdown(markdown()), "user", undefined, {
      sourcesNotice: hasDocSources,
    });
    router.push(draftHref(threadId, draftName));
  };

  const downloadMd = () => {
    downloadBlob(new Blob([markdown()], { type: "text/markdown;charset=utf-8" }), `${name}.md`);
  };

  const toDocx = (docName: string) => markdownToDocx({ name: docName, markdown: docxMarkdown(markdown()) });

  const downloadDocx = async () => {
    if (busy) return;
    setBusy(true);
    try {
      downloadBlob(await toDocx(name), `${name}.docx`);
    } catch (e) {
      toast({ variant: "destructive", description: errorText(e, "Word への変換に失敗しました。") });
    } finally {
      setBusy(false);
    }
  };

  const saveDocx = async (target: SaveTarget) => {
    if (saving) return;
    setSaving(true);
    const fileName = /\.docx$/i.test(target.name) ? target.name : `${target.name}.docx`;
    let blob: Blob;
    try {
      blob = await toDocx(fileName.replace(/\.docx$/i, ""));
    } catch (e) {
      toast({ variant: "destructive", description: errorText(e, "Word への変換に失敗しました。") });
      setSaving(false);
      return;
    }
    try {
      // 同じ名前があれば連番の名前で置く（既存のファイルを上書きしない）。
      const node = await uploadNewFile({
        file: new File([blob], fileName, { type: DOCX_TYPE }),
        parentId: target.parentId ?? undefined,
      });
      setSaveOpen(false);
      toast({
        description: (
          <span>
            「{node.name}」をドライブに保存しました。
            <Link href={`/office/${node.id}`} className="ml-1 font-medium underline underline-offset-2">
              開く
            </Link>
          </span>
        ),
      });
    } catch (e) {
      toast({ variant: "destructive", description: saveFailedMessage(e, target.parentId) });
    } finally {
      setSaving(false);
    }
  };

  // PDF: その場で印刷する（会話を取り直さない）。押している間は層を出し、閉じたら外す。
  const [printing, setPrinting] = React.useState(false);
  const imageSources = React.useMemo(
    () => (printing ? answerImageSources(segments, citations) : null),
    [printing, segments, citations],
  );

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            aria-label="書き出し"
            title="書き出し"
            disabled={busy}
            data-testid="message-export"
            className={footerButtonClass()}
          >
            {busy ? (
              <Loader2 className="size-3.5 animate-spin" aria-hidden />
            ) : (
              <FileOutput className="size-3.5" aria-hidden />
            )}
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-60 p-1.5">
          <DropdownMenuLabel>エクスポート</DropdownMenuLabel>
          <DropdownMenuItem className="gap-2.5 px-2.5 py-2" onSelect={openAsNote} data-testid="message-export-note">
            <NotebookPen className="text-muted-foreground" />
            ノートとして開く
          </DropdownMenuItem>
          <DropdownMenuItem
            className="gap-2.5 px-2.5 py-2"
            onSelect={() => setSaveOpen(true)}
            data-testid="message-export-docx-drive"
          >
            <FileType className="text-blue-600 dark:text-blue-400" />
            Word としてドライブに保存…
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuLabel>ダウンロード</DropdownMenuLabel>
          <DropdownMenuItem className="gap-2.5 px-2.5 py-2" onSelect={downloadMd} data-testid="message-export-md">
            <FileText className="text-muted-foreground" />
            Markdown (.md)
          </DropdownMenuItem>
          <DropdownMenuItem
            className="gap-2.5 px-2.5 py-2"
            onSelect={() => void downloadDocx()}
            data-testid="message-export-docx"
          >
            <FileDown className="text-muted-foreground" />
            Word (.docx)
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
      <SaveDraftDialog
        open={saveOpen}
        onOpenChange={(v) => !saving && setSaveOpen(v)}
        defaultName={name}
        saving={saving}
        onConfirm={(t) => void saveDocx(t)}
        entityLabel="ファイル"
        description="回答を Word 文書（.docx）にしてドライブに保存します。"
        defaultFolder={defaultFolder}
        notice={hasDocSources ? SOURCES_NOTICE : undefined}
      />
      {printing && imageSources ? (
        <AnswerPrint
          text={text}
          citations={citations}
          imageSources={imageSources}
          title={threadTitle}
          onDone={() => setPrinting(false)}
        />
      ) : null}
    </>
  );
}

function errorText(e: unknown, fallback: string): string {
  return e instanceof Error && e.message ? e.message : fallback;
}
