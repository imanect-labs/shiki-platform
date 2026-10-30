"use client";

/// 実行履歴の「AI で判定」（ai.decide）の注意: AI が自信の低い答えを返したこと（BUG-124・D-114）。
///
/// AI が自分の答えに、どの答えも同じくらいと見たときの値（はい／いいえなら 50%）より低い自信を付けると、判定は
/// AI の自信をその値まで引き上げて進む（D-78 (3)）。取り違えや迷いの合図が値から消えるので、引き上げた質問を作る人に
/// 見せる。形はカタログの output_fields（raised_questions・answers）と同じで、読めない項目は無いものとして扱う。
/// 質問の文は、実行に使ったバージョンの設定（params.questions）から引く。

import { TriangleAlert } from "lucide-react";

type Question = { key?: unknown; instructions?: unknown };
type Output = { raised_questions?: unknown; answers?: Record<string, { confidence?: unknown } | undefined> };

/// 出力の引き上げた質問のキー（無い・読めないなら空）。
export function raisedQuestionsOf(output: unknown): string[] {
  if (typeof output !== "object" || output === null) return [];
  const raised = (output as Output).raised_questions;
  return Array.isArray(raised) ? raised.filter((k): k is string => typeof k === "string") : [];
}

/// 0〜1 の値を % で（整数ならそのまま、そうでなければ小数 1 桁）。
function percent(x: number): string {
  const p = Math.round(x * 1000) / 10;
  return `${Number.isInteger(p) ? p.toFixed(0) : p.toFixed(1)}%`;
}

export function DecideRaised({ output, params }: { output: unknown; params: unknown }) {
  const raised = raisedQuestionsOf(output);
  if (raised.length === 0) return null;
  const questions = (
    Array.isArray((params as { questions?: unknown } | null)?.questions)
      ? (params as { questions: Question[] }).questions
      : []
  ) as Question[];
  const answers = (output as Output).answers ?? {};
  return (
    <div
      role="note"
      className="space-y-1.5 rounded-md border p-2.5 text-[11px] leading-relaxed"
      style={{
        borderColor: "color-mix(in oklab, var(--season-autumn) 45%, transparent)",
        backgroundColor: "color-mix(in oklab, var(--season-autumn) 8%, transparent)",
      }}
      data-testid="decide-raised"
    >
      <p className="flex items-start gap-1.5 font-medium">
        <TriangleAlert className="mt-0.5 size-3.5 shrink-0 text-[var(--season-autumn)]" aria-hidden />
        <span>AI が自信の低い答えを返しました（引き上げた質問: {raised.length} 件）</span>
      </p>
      <ul className="space-y-0.5 pl-5 [overflow-wrap:anywhere]">
        {raised.map((key) => {
          const ask = questions.find((q) => q.key === key)?.instructions;
          const c = answers[key]?.confidence;
          return (
            <li key={key} className="list-disc">
              {typeof ask === "string" && ask.trim() !== "" ? `「${ask}」（${key}）` : key}
              {typeof c === "number" && Number.isFinite(c)
                ? `: ${percent(c)} 未満を ${c.toFixed(2)} に引き上げ`
                : null}
            </li>
          );
        })}
      </ul>
      <p>
        AI が質問か判定の材料を取り違えているかもしれません。「AI に尋ねること」の書き方と判定の材料を見直してください。
      </p>
      <p className="text-muted-foreground">
        AI の自信は、どの答えも同じくらいと見たときの値より低くしません。AI の自信（較正なし）は、正解の確率ではありません。
      </p>
    </div>
  );
}
