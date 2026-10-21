"use client";

/// 引用表示のギャラリー（デザイン確認・スクショ改善ループ用・issue #505）。
///
/// 認証・LLM 不要（middleware は /reference を除外）。実コンポーネントに固定フィクスチャを流し、
/// ファイル名などのノード情報は `NodeMetaOverride` で差し込む（getNode を叩かない）。
/// 先頭のデモは実際に操作できる（番号にホバー → 原文カード、クリック → 右に出典パネル）。
import * as React from "react";

import { Markdown } from "@/components/prompt-kit/markdown";
import {
  MessageCitationsProvider,
  NodeMetaOverride,
  SourcePanelProvider,
  useMessageCitations,
} from "@/components/chat/citations/citation-context";
import { CitationCardBody } from "@/components/chat/citations/citation-marker";
import { CitationSources } from "@/components/chat/citations/citation-sources";
import { EvidenceTable } from "@/components/chat/citations/evidence-table";
import { SourcePanelAside, SourcePanelView } from "@/components/chat/citations/source-panel";
import { linkifyCitations } from "@/lib/citation";
import type { Citation } from "@/lib/chat-api";
import { ANSWER, ANSWER_NO_MARKERS, CITATIONS, METAS } from "./fixtures";

function Cell({ id, title, children }: { id: string; title: string; children: React.ReactNode }) {
  return (
    <section data-testid={`citations-${id}`} className="min-w-0">
      <h2 className="mb-1.5 text-[12px] font-medium text-muted-foreground">{title}</h2>
      {children}
    </section>
  );
}

function Answer({
  messageKey,
  text,
  citations = CITATIONS,
  children,
}: {
  messageKey: string;
  text: string;
  citations?: Citation[];
  children?: React.ReactNode;
}) {
  return (
    <MessageCitationsProvider messageKey={messageKey} citations={citations} text={text}>
      <div className="rounded-xl border border-border bg-background px-6 py-5">
        <div className="text-[15px] leading-relaxed">
          <Markdown>{linkifyCitations(text, citations)}</Markdown>
        </div>
        {children}
      </div>
    </MessageCitationsProvider>
  );
}

/// 出典パネルを静的に描く（state を直接渡す）。
function StaticPanel({ n }: { n: number }) {
  return (
    <MessageCitationsProvider messageKey="static-panel" citations={CITATIONS} text={ANSWER}>
      <PanelFrame n={n} />
    </MessageCitationsProvider>
  );
}

function PanelFrame({ n }: { n: number }) {
  const message = useMessageCitations()!;
  return (
    <div className="h-[30rem] w-[26rem] max-w-full overflow-hidden rounded-xl border border-border bg-background">
      <SourcePanelView state={{ message, n }} onNavigate={() => {}} onClose={() => {}} />
    </div>
  );
}

function StaticCard({ n }: { n: number }) {
  return (
    <MessageCitationsProvider messageKey="static-card" citations={CITATIONS} text={ANSWER}>
      <CardFrame n={n} />
    </MessageCitationsProvider>
  );
}

function CardFrame({ n }: { n: number }) {
  const message = useMessageCitations()!;
  return (
    <div className="w-[25rem] max-w-full rounded-xl border border-border bg-popover p-3.5 shadow-lg shadow-black/[0.06]">
      <CitationCardBody n={n} message={message} onOpenPanel={() => {}} />
    </div>
  );
}

export default function CitationGalleryPage() {
  return (
    <NodeMetaOverride metas={METAS}>
      <SourcePanelProvider>
        <div className="flex min-h-screen">
          <main className="mx-auto min-w-0 max-w-3xl flex-1 space-y-8 p-6">
            <header>
              <h1 className="text-lg font-semibold tracking-tight">引用表示</h1>
              <p className="mt-1 text-[13px] text-muted-foreground">
                RAG の引用（issue #505）。回答直後は 1 行に畳み、開くと文書ごとの一覧。番号はホバーで原文、
                クリックで出典パネル。メッセージのアクションから「根拠を確認」。
              </p>
            </header>

            <Cell id="demo" title="操作できるデモ（番号にホバー・クリック、下の 1 行を開く）">
              <Answer messageKey="demo" text={ANSWER}>
                <CitationSources />
              </Answer>
            </Cell>

            <Cell id="expanded" title="出典一覧を開いた状態（使わなかった検索結果は畳む）">
              <Answer messageKey="expanded" text={ANSWER}>
                <CitationSources defaultOpen />
              </Answer>
            </Cell>

            <Cell id="card" title="原文カード（番号 5 にホバーしたとき）">
              <StaticCard n={5} />
            </Cell>

            <Cell id="panel" title="出典パネル（番号 6 を開いたとき）">
              <StaticPanel n={6} />
            </Cell>

            <Cell id="evidence" title="根拠を確認（メッセージのアクションから開く）">
              <Answer messageKey="evidence" text={ANSWER}>
                <EvidenceTable />
              </Answer>
            </Cell>

            <Cell id="no-markers" title="本文に番号が無い回答（全件を「参照した文書」として出す）">
              <Answer messageKey="no-markers" text={ANSWER_NO_MARKERS}>
                <CitationSources defaultOpen />
              </Answer>
            </Cell>

            <Cell id="unresolved" title="ファイル名が解決できない（読み込み中・権限なし）">
              <NodeMetaOverride metas={{}}>
                <Answer messageKey="unresolved" text={ANSWER}>
                  <CitationSources defaultOpen />
                </Answer>
              </NodeMetaOverride>
            </Cell>
          </main>
          <div className="sticky top-0 h-screen">
            <SourcePanelAside />
          </div>
        </div>
      </SourcePanelProvider>
    </NodeMetaOverride>
  );
}
