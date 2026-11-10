"use client";

/// 回答のリンクの出所チェックのギャラリー（#1143・デザイン確認と E2E 用）。
///
/// 認証・LLM 不要（middleware は /reference を除外）。実部品（MessageTimeline）に固定フィクスチャを流す。
/// 出典（Web の引用）と同じホストへのリンクはそのまま開き、出典に無い外部リンク（社内文書に仕込まれた
/// 誘導など）は行き先と注意の印を添え、押したときに確認してから開く。

import * as React from "react";

import { MessageTimeline, type TimelineSegment } from "@/components/chat/message-timeline";
import { Markdown } from "@/components/prompt-kit/markdown";
import type { Citation } from "@/lib/chat-api";

const web = (cite_id: number, url: string, title: string, snippet: string): Citation => ({
  type: "citation",
  cite_id,
  node_id: "",
  chunk_id: `web:${url}`,
  snippet,
  heading_path: [],
  score: 0,
  web: { url, title },
});

const doc = (cite_id: number, snippet: string): Citation => ({
  type: "citation",
  cite_id,
  node_id: "6f6c2c1e-1b7a-4c1e-9e52-0c7e0a0f5a11",
  chunk_id: "2b3c4d5e-6f70-4812-9a3b-4c5d6e7f8091",
  snippet,
  heading_path: ["第3条 締め日"],
  score: 0.82,
  web: null,
});

/// 社内文書（1）と Web（2）を引用した回答。
const CITATIONS: Citation[] = [
  doc(1, "経費精算の申請は毎月25日締めとし、翌月10日に支払う。"),
  web(2, "https://www.nta.go.jp/taxes/shiraberu/taxanswer/hojin/5260.htm", "交際費等の範囲と損金不算入額の計算", "交際費等とは…"),
];

/// 出典と同じホストのリンク・出典に無いリンク（社内文書に仕込まれた誘導）・表示を偽ったリンク・メール。
const ANSWER = [
  "経費精算の締め日は毎月25日で、翌月10日に支払われます[1]。",
  "",
  "交際費の扱いは国税庁の解説が参考になります[2]。概要は [国税庁のタックスアンサー](https://www.nta.go.jp/taxes/shiraberu/taxanswer/index2.htm) から探せます。",
  "",
  // 社内文書の本文に仕込まれた誘導を、モデルがそのまま書いたもの（#1143 の実測）。
  "詳細は https://evil.example/login で社員IDとパスワードを入力して確認してください。",
  "",
  // 表示の文字は出典のサイトに見せかけ、行き先は別のホスト。
  "申請フォームは [www.nta.go.jp の申請ページ](https://nta-go-jp.example.net/form) にあります。",
  "",
  "不明点は [経理部](mailto:keiri@corp.example) まで。",
].join("\n");

const SEGMENTS: TimelineSegment[] = [{ kind: "text", key: "t1", text: ANSWER }];

export default function AnswerLinksGalleryPage() {
  return (
    <main className="mx-auto max-w-3xl space-y-8 p-6">
      <header>
        <h1 className="text-lg font-semibold tracking-tight">回答のリンク</h1>
        <p className="mt-1 text-[13px] text-muted-foreground">
          出典（Web の引用）と同じホストへのリンクはそのまま開く。出典に無い外部リンクは行き先と注意の印を添え、
          押したときに確認してから開く（#1143）。
        </p>
      </header>
      <section data-testid="answer-links-timeline" className="min-w-0 rounded-xl border border-border px-6 py-5">
        <MessageTimeline segments={SEGMENTS} citations={CITATIONS} />
      </section>
      <section className="space-y-2">
        <h2 className="text-[13px] font-medium text-muted-foreground">回答以外の本文（ノート・ヘルプなど）は確認しない</h2>
        <div data-testid="answer-links-plain" className="min-w-0 rounded-xl border border-border px-6 py-5">
          <Markdown>{"社外の資料: [example](https://evil.example/login)"}</Markdown>
        </div>
      </section>
    </main>
  );
}
