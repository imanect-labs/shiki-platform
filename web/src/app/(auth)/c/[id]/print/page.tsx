"use client";

/// 回答の印刷ビュー（#1048・PDF の書き出し）。`/c/{thread}/print?m={message}` で回答 1 件を
/// 全幅で描き、出典の名前が揃ってから window.print() を呼ぶ（ブラウザの「PDF に保存」で PDF 化する）。
///
/// ノートの PDF（#334）と同じくブラウザ印刷ベース。```ui の図は SVG のまま印刷され、
/// 図の操作ボタンは印刷しない（globals.css の .answer-print）。会話の読み取りは通常の
/// GET /threads/{id}/messages なので、読めない会話・回答は「見つかりません」になる。

import { useSearchParams } from "next/navigation";
import * as React from "react";

import { MessageCitationsProvider, useMessageCitations } from "@/components/chat/citations/citation-context";
import { segmentsFromBlocks } from "@/components/chat/message-timeline";
import { Markdown } from "@/components/prompt-kit/markdown";
import { EditorLoading } from "@/components/shell/editor-loading";
import { EmptyState } from "@/components/ui/empty-state";
import { sourcesMarkdown } from "@/lib/answer-export";
import { getThread, getThreadMessages, type Citation } from "@/lib/chat-api";
import { linkifyCitations } from "@/lib/citation";
import { formatDateTime } from "@/lib/format";

type Answer = { text: string; citations: Citation[]; createdAt: string; title: string | null };

/// 出典の名前が解決するのを待つ上限（読めない文書があっても印刷は出す）。
const NAME_WAIT_MS = 3000;

export default function AnswerPrintPage({ params }: { params: Promise<{ id: string }> }) {
  const { id: threadId } = React.use(params);
  return (
    <React.Suspense fallback={<EditorLoading kind="doc" message="印刷プレビューを準備しています…" />}>
      <AnswerPrint threadId={threadId} />
    </React.Suspense>
  );
}

function AnswerPrint({ threadId }: { threadId: string }) {
  const messageId = useSearchParams().get("m") ?? "";
  const [answer, setAnswer] = React.useState<Answer | "loading" | "notfound">("loading");

  React.useEffect(() => {
    let cancelled = false;
    setAnswer("loading");
    Promise.all([getThreadMessages(threadId), getThread(threadId).catch(() => null)])
      .then(([{ messages }, thread]) => {
        if (cancelled) return;
        const m = messages.find((x) => x.id === messageId && x.role === "assistant");
        if (!m) return setAnswer("notfound");
        // 本文はチャットの表示・コピーと同じ組み立て（本文の区切りを段落として繋ぐ）。
        const text = segmentsFromBlocks(m.content)
          .flatMap((s) => (s.kind === "text" ? [s.text] : []))
          .join("\n\n");
        const citations = m.content.filter((b): b is Citation => b.type === "citation");
        setAnswer({ text, citations, createdAt: m.createdAt, title: thread?.title.trim() || null });
      })
      .catch(() => !cancelled && setAnswer("notfound"));
    return () => {
      cancelled = true;
    };
  }, [threadId, messageId]);

  if (answer === "loading") {
    return <EditorLoading kind="doc" message="印刷プレビューを準備しています…" />;
  }
  if (answer === "notfound" || !answer.text) {
    return <EmptyState title="回答が見つかりません" description="削除されたか、アクセス権がありません。" />;
  }

  return (
    <div className="answer-print mx-auto max-w-3xl px-6 py-8" data-testid="answer-print">
      <p className="mb-6 border-b pb-3 text-xs text-muted-foreground">
        {answer.title ? <span className="font-medium text-foreground/80">{answer.title}</span> : null}
        {answer.title ? " ・ " : null}
        Shiki の回答 ・ {formatDateTime(answer.createdAt)}
      </p>
      <MessageCitationsProvider messageKey={`print:${messageId}`} citations={answer.citations} text={answer.text}>
        <Markdown>{linkifyCitations(answer.text, answer.citations)}</Markdown>
        <PrintSources text={answer.text} citations={answer.citations} />
      </MessageCitationsProvider>
    </div>
  );
}

/// 末尾の「出典」一覧（書き出しの md と同じ文面）。名前が揃うか待ちの上限で、一度だけ印刷を出す。
function PrintSources({ text, citations }: { text: string; citations: Citation[] }) {
  const cited = useMessageCitations();
  const md = cited ? sourcesMarkdown(text, citations, cited.nameOf) : null;
  // 文書の名前がまだ引けていない出所があるか（引けないと確定したものは「開けないファイル」になる）。
  const loading =
    !!cited &&
    cited.groups.docs.some((d) => !d.web && cited.nameOf(d.passages[0].citation) === "読み込み中…");
  const printTimer = React.useRef<number | null>(null);
  const [timedOut, setTimedOut] = React.useState(false);

  React.useEffect(() => {
    const t = window.setTimeout(() => setTimedOut(true), NAME_WAIT_MS);
    return () => {
      window.clearTimeout(t);
      if (printTimer.current !== null) window.clearTimeout(printTimer.current);
    };
  }, []);
  React.useEffect(() => {
    if (printTimer.current !== null || (loading && !timedOut)) return;
    // 図・数式の描画が落ち着くのを少し待つ（ノートの印刷ビューと同じ間合い）。
    printTimer.current = window.setTimeout(() => window.print(), 800);
  }, [loading, timedOut]);

  if (!md) return null;
  return (
    <div className="mt-8 border-t pt-4 text-sm" data-testid="answer-print-sources">
      <Markdown className="text-sm">{md}</Markdown>
    </div>
  );
}
