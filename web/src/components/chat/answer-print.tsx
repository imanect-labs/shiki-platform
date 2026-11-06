"use client";

/// 回答・会話をその場で印刷する（#1048・PDF の書き出し）。ブラウザの「PDF に保存」で PDF 化する。
///
/// 会話画面は回答の本文・出典を既に持っているので、取り直さずに印刷用の層（body 直下のポータル）へ
/// 描き、印刷の間だけアプリの画面を隠す（globals.css の `body.printing-answer`）。新しいタブで会話を
/// 丸ごと読み直すと、長い会話ほど無関係な履歴の取得が増えるため。
///
/// 描画はチャットと同じ Markdown（```ui の図は SVG のまま印刷・出典に現れた画像だけ読み込む・PIT-77）。
/// 回答ごとに出典の文脈（[`MessageCitationsProvider`]）を張り直し、引用番号と出典の一覧を回答ごとに解く
/// （番号は回答ごとに 1 から振られる）。

import * as React from "react";
import { createPortal } from "react-dom";

import { Markdown } from "@/components/prompt-kit/markdown";
import { sourcesMarkdown } from "@/lib/answer-export";
import type { ImageSources } from "@/lib/answer-images";
import type { Citation } from "@/lib/chat-api";
import { linkifyCitations } from "@/lib/citation";
import { formatDateTime } from "@/lib/format";
import { MessageCitationsProvider, useMessageCitations } from "./citations/citation-context";

/// 画像の読み込みを待つ上限（届かない画像があっても印刷は出す）。
const IMAGE_WAIT_MS = 3000;
const BODY_CLASS = "printing-answer";

/// 印刷する 1 組（質問は会話全体の書き出しで「質問も含める」ときだけ）。
export type PrintItem = {
  key: string;
  question: string | null;
  text: string;
  citations: readonly Citation[];
  imageSources: ImageSources;
};

export function ExportPrint({
  title,
  subtitle,
  items,
  onDone,
}: {
  title: string | null;
  /// 題名の横の説明（「Shiki の回答」「Shiki との会話」）。
  subtitle: string;
  items: readonly PrintItem[];
  /// 印刷ダイアログを閉じた（または出せなかった）。呼び出し側はこの層を外す。
  onDone: () => void;
}) {
  const root = React.useRef<HTMLDivElement>(null);
  const done = React.useRef(onDone);
  done.current = onDone;
  const [printedAt] = React.useState(() => new Date().toISOString());

  React.useEffect(() => {
    let cancelled = false;
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      document.body.classList.remove(BODY_CLASS);
      window.removeEventListener("afterprint", finish);
      done.current();
    };
    document.body.classList.add(BODY_CLASS);
    window.addEventListener("afterprint", finish);
    void (async () => {
      // 遅延読み込みの画像は印刷層では読み込まれないことがあるので、すぐ読ませて描画を待つ。
      const images = Array.from(root.current?.querySelectorAll("img") ?? []);
      for (const img of images) img.loading = "eager";
      await Promise.race([
        Promise.allSettled(images.map((img) => img.decode())),
        new Promise((r) => window.setTimeout(r, IMAGE_WAIT_MS)),
      ]);
      // 図・数式の描画が落ち着くのを 1 フレーム待つ。
      await new Promise((r) => requestAnimationFrame(() => r(null)));
      if (cancelled) return;
      window.print();
      // print() が戻る＝ダイアログを閉じた（afterprint を出さないブラウザもあるので、ここでも閉じる）。
      finish();
    })();
    return () => {
      cancelled = true;
      document.body.classList.remove(BODY_CLASS);
      window.removeEventListener("afterprint", finish);
    };
  }, []);

  return createPortal(
    <div ref={root} className="answer-print-root" data-testid="answer-print" aria-hidden>
      <p className="mb-6 border-b pb-3 text-xs text-muted-foreground">
        {title ? <span className="font-medium text-foreground/80">{title}</span> : null}
        {title ? " ・ " : null}
        {subtitle} ・ {formatDateTime(printedAt)} に書き出し
      </p>
      <div className="space-y-8">
        {items.map((item) => (
          <MessageCitationsProvider
            key={item.key}
            messageKey={`print:${item.key}`}
            citations={item.citations}
            text={item.text}
          >
            <PrintTurn item={item} />
          </MessageCitationsProvider>
        ))}
      </div>
    </div>,
    document.body,
  );
}

function PrintTurn({ item }: { item: PrintItem }) {
  const cited = useMessageCitations();
  const sources = cited ? sourcesMarkdown(item.text, item.citations, cited.nameOf) : null;
  return (
    <section className="answer-print-turn" data-testid="answer-print-turn">
      {item.question ? (
        <div className="mb-4 rounded-md border-l-2 border-border pl-3" data-testid="answer-print-question">
          <p className="mb-1 text-xs font-medium text-muted-foreground">質問</p>
          {/* 利用者が書いた文（チャットの表示と同じく、図・画像にせず打ったとおりに描く）。 */}
          <Markdown untrusted breaks>
            {item.question}
          </Markdown>
        </div>
      ) : null}
      <Markdown imageSources={item.imageSources}>{linkifyCitations(item.text, item.citations)}</Markdown>
      {sources ? (
        <div className="mt-6 border-t pt-4 text-sm" data-testid="answer-print-sources">
          <Markdown className="text-sm">{sources}</Markdown>
        </div>
      ) : null}
    </section>
  );
}
