"use client";

/// ```ui（Yui）の図のギャラリー（デザイン確認・スマホ幅の崩れの回帰確認用）。
///
/// 認証・LLM 不要（middleware は /reference を除外）。会話の回答と同じ列（max-w-3xl・左右 16px）と
/// 同じ Markdown の描画で、全種類の図と狭い画面で崩れやすい形（長いラベル・多い行や列・
/// deep research の質問と計画）を並べる。ボタンは押せる状態にし、押すと「送信しません」と返す（何も送らない）。

import * as React from "react";

import { Markdown } from "@/components/prompt-kit/markdown";
import { Message } from "@/components/prompt-kit/message";
import { YuiActionContext, type YuiActionHost } from "@/components/prompt-kit/yui-actions";
import { BASIC, DEEP_RESEARCH, STRESS, type YuiSample } from "./fixtures";

const HOST: YuiActionHost = {
  invoke: async () => {
    throw new Error("ギャラリーでは送信しません");
  },
  isBound: () => true,
  invokedActions: [],
  uiRuns: [],
  onChanged: () => {},
};

function Answer({ sample }: { sample: YuiSample }) {
  return (
    <section data-testid={`yui-${sample.id}`} className="min-w-0">
      <h2 className="mb-1.5 text-[12px] font-medium text-muted-foreground">{sample.title}</h2>
      <Message className="group justify-start">
        <div className="w-full min-w-0">
          <YuiActionContext.Provider value={HOST}>
            <Markdown>{sample.text}</Markdown>
          </YuiActionContext.Provider>
        </div>
      </Message>
    </section>
  );
}

export default function YuiGalleryPage() {
  return (
    <main className="mx-auto flex w-full min-w-0 max-w-3xl flex-col gap-6 px-4 py-8">
      <header>
        <h1 className="text-lg font-semibold tracking-tight">図（```ui）</h1>
        <p className="mt-1 text-[13px] text-muted-foreground">
          会話の回答と同じ幅・同じ描画で、全種類の図と崩れやすい形を並べる。スマホ幅（390px）でも横にはみ出さないこと。
        </p>
      </header>
      {[...DEEP_RESEARCH, ...BASIC, ...STRESS].map((s) => (
        <Answer key={s.id} sample={s} />
      ))}
    </main>
  );
}
