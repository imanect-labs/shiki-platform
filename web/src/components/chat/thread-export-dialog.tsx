"use client";

/// 会話全体の書き出し（右上の ⋯ →「会話を書き出し…」）。行き先は回答 1 件の書き出しと同じ
/// （ノートとして開く・Word としてドライブに保存・Markdown・Word・PDF）で、操作の中身も共用する
/// （[`useExportActions`]）。
///
/// やり取りを順に（質問 → 回答 → その回答の出典）並べる。「質問も含める」をオフにすると回答だけ。
/// 思考・ツールの実行・添付は含めない。会話画面が持つメッセージから組み立て、会話を取り直さない。

import { FileDown, FileText, FileType, Loader2, NotebookPen, Printer } from "lucide-react";
import * as React from "react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Switch } from "@/components/ui/switch";
import {
  hasDocumentSources,
  threadFileName,
  threadMarkdown,
  type SourceName,
  type ThreadTurn,
} from "@/lib/answer-export";
import type { Citation, ContentBlock, Message } from "@/lib/chat-api";
import { isWebCitation } from "@/lib/citation";
import { isUnresolvable, useNodeMetas } from "@/lib/node-name-cache";
import { cn } from "@/lib/utils";
import { ExportPrint, type PrintItem } from "./answer-print";
import { useExportActions } from "./export-actions";
import { answerImageSources, answerLinkSourcesOf, segmentsFromBlocks, type TimelineSegment } from "./message-timeline";

type Turn = ThreadTurn & { key: string; segments: TimelineSegment[] };

/// 会話のメッセージを、やり取り（質問と、それへの回答）の並びにする。回答の無い質問（生成中など）は含めない。
export function threadTurns(messages: readonly Message[]): Turn[] {
  const turns: Turn[] = [];
  let question: string | null = null;
  for (const m of messages) {
    if (m.role === "user") {
      question = textOf(m.content) || null;
      continue;
    }
    if (m.role !== "assistant") continue;
    const segments = segmentsFromBlocks(m.content);
    const answer = segments.flatMap((s) => (s.kind === "text" ? [s.text] : [])).join("\n\n");
    if (!answer.trim()) continue;
    const citations = m.content.filter((b): b is Citation => b.type === "citation");
    turns.push({ key: m.id, question, answer, citations, segments });
    question = null;
  }
  return turns;
}

function textOf(blocks: readonly ContentBlock[]): string {
  return blocks
    .flatMap((b) => (b.type === "text" ? [b.text] : []))
    .join("\n\n")
    .trim();
}

export function ThreadExportDialog({
  open,
  onOpenChange,
  threadId,
  title,
  messages,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  threadId: string;
  title: string | null;
  messages: readonly Message[];
}) {
  const [includeQuestions, setIncludeQuestions] = React.useState(true);
  // 開閉に関わらず持つ（Word の保存は、ダイアログを閉じた後の保存先の確定で組み立てるため）。
  // メッセージは生成の確定時にだけ変わる（生成中のトークンでは変わらない）。
  const turns = React.useMemo(() => threadTurns(messages), [messages]);
  const citations = React.useMemo(() => turns.flatMap((t) => t.citations), [turns]);

  // 出典の文書名（回答の出典一覧と同じキャッシュから引く。書き出しは押した時点で引けている名前を使う）。
  const docIds = React.useMemo(
    () => Array.from(new Set(citations.filter((c) => !c.withheld && !isWebCitation(c)).map((c) => c.node_id))),
    [citations],
  );
  const metas = useNodeMetas(docIds);
  const nameOf: SourceName = (c) => {
    if (isWebCitation(c)) return c.web.title || c.web.url;
    return metas[c.node_id]?.name ?? (isUnresolvable(c.node_id) ? "開けないファイル" : "文書");
  };

  const actions = useExportActions({
    threadId,
    what: "会話",
    fileName: () => threadFileName(title),
    markdown: () => threadMarkdown(turns, { title, includeQuestions, nameOf }),
    hasDocSources: hasDocumentSources(citations),
  });

  // PDF: その場で印刷する。ダイアログを閉じてから印刷層を出す（ダイアログを紙面に載せない）。
  const [printItems, setPrintItems] = React.useState<PrintItem[] | null>(null);
  const print = () => {
    setPrintItems(
      turns.map((t) => ({
        key: t.key,
        question: includeQuestions ? t.question : null,
        text: t.answer,
        citations: t.citations,
        imageSources: answerImageSources(t.segments, [...t.citations]),
        linkSources: answerLinkSourcesOf(t.segments, [...t.citations]),
      })),
    );
  };

  /// 押したらダイアログを閉じて操作を始める（Word の変換・保存は閉じた後も続き、結果はトーストで知らせる）。
  const run = (action: () => void | Promise<void>) => () => {
    onOpenChange(false);
    void action();
  };

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="max-w-md" data-testid="thread-export-dialog">
          <DialogHeader>
            <DialogTitle>会話を書き出し</DialogTitle>
            <DialogDescription>
              {turns.length > 0
                ? `会話全体（${turns.length} 件のやり取り）を書き出します。思考やツールの実行は含めません。`
                : "書き出せる回答がまだありません。"}
            </DialogDescription>
          </DialogHeader>

          <label className="flex items-center justify-between gap-3 rounded-md border border-border px-3 py-2.5">
            <span className="text-sm">
              質問も含める
              <span className="mt-0.5 block text-xs text-muted-foreground">オフにすると回答だけを並べます</span>
            </span>
            <Switch
              checked={includeQuestions}
              onCheckedChange={setIncludeQuestions}
              aria-label="質問も含める"
              data-testid="thread-export-include-questions"
            />
          </label>

          <div className="space-y-3" role="group" aria-label="書き出し先">
            <Section label="エクスポート">
              <Choice
                icon={<NotebookPen className="text-muted-foreground" />}
                label="ノートとして開く"
                onClick={run(actions.openAsNote)}
                disabled={turns.length === 0}
                testId="thread-export-note"
              />
              <Choice
                icon={<FileType className="text-blue-600 dark:text-blue-400" />}
                label={actions.saving ? "ドライブに保存しています…" : "Word としてドライブに保存…"}
                onClick={run(actions.openSaveDocx)}
                disabled={turns.length === 0 || actions.saving}
                testId="thread-export-docx-drive"
              />
            </Section>
            <Section label="ダウンロード">
              <Choice
                icon={<FileText className="text-muted-foreground" />}
                label="Markdown (.md)"
                onClick={run(actions.downloadMd)}
                disabled={turns.length === 0}
                testId="thread-export-md"
              />
              <Choice
                icon={
                  actions.converting ? (
                    <Loader2 className="animate-spin text-muted-foreground" />
                  ) : (
                    <FileDown className="text-muted-foreground" />
                  )
                }
                label={actions.converting ? "Word に変換しています…" : "Word (.docx)"}
                onClick={run(actions.downloadDocx)}
                disabled={turns.length === 0 || actions.converting}
                testId="thread-export-docx"
              />
              <Choice
                icon={<Printer className="text-muted-foreground" />}
                label="PDF"
                onClick={run(print)}
                disabled={turns.length === 0}
                testId="thread-export-pdf"
              />
            </Section>
          </div>
        </DialogContent>
      </Dialog>
      {actions.dialog}
      {printItems ? (
        <ExportPrint
          title={title}
          subtitle="Shiki との会話"
          items={printItems}
          onDone={() => setPrintItems(null)}
        />
      ) : null}
    </>
  );
}

function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <p className="mb-1 px-1 text-xs font-medium text-muted-foreground">{label}</p>
      <div className="space-y-0.5">{children}</div>
    </div>
  );
}

function Choice({
  icon,
  label,
  onClick,
  disabled,
  testId,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  testId: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      data-testid={testId}
      className={cn(
        "flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-left text-sm transition-colors pointer-coarse:min-h-11",
        "hover:bg-accent hover:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
        "disabled:pointer-events-none disabled:opacity-50 [&_svg]:size-4 [&_svg]:shrink-0",
      )}
    >
      {icon}
      {label}
    </button>
  );
}
