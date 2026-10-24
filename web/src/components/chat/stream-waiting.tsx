"use client";

/// 生成中の「待たされている」表示。
///
/// モデルが大きなツール入力（Office 文書の中身など）を書き出している間や、ステップの合間は
/// イベントが数十秒届かない。何も動かないと固まったように見えるので、最後のイベントからの
/// 経過で文言を進め、回転アイコンと光の流れる文字で「動いている」ことを伝える。

import * as React from "react";
import { Loader2 } from "lucide-react";

import type { ToolActivityItem } from "./tool-activity";
import { WAITING_STEPS, waitingPhrase, type WaitingContext } from "@/lib/waiting-phrases";

/// 本文の途中で止まってから、末尾に待ち表示を出すまでの秒数（トークンの小休止では出さない）。
const SHOW_AFTER_SECS = 2;
/// 経過秒を添え始める秒数。
const ELAPSED_AFTER_SECS = 10;

/// `signal` が最後に変わってからの秒数を、`steps` の刻みに丸めて返す（`active` の間だけ進める）。
///
/// 毎秒の再描画はしない。この値を持つのは本文の Markdown ごと描き直す行なので、刻みを
/// 跨いだとき（文言が変わる・末尾の待ち表示を出す）だけ描き直す。経過秒の表示は
/// [`StreamWaitingLine`] が自分の中で数える。
function useIdleStep(signal: string, active: boolean, steps: readonly number[]): number {
  const [secs, setSecs] = React.useState(0);
  React.useEffect(() => {
    // 0 のままなら再描画しない（トークンごとに行を余計に描き直さない）。
    setSecs(0);
    if (!active) return;
    const timers = steps
      .filter((s) => s > 0)
      .map((s) => window.setTimeout(() => setSecs(s), s * 1000));
    return () => timers.forEach((t) => window.clearTimeout(t));
  }, [signal, active, steps]);
  return secs;
}

/// 末尾の待ち表示を出すまでの秒数も刻みに含める。
const IDLE_STEPS = [...new Set([...WAITING_STEPS, SHOW_AFTER_SECS])].sort((a, b) => a - b);
/// 思考が止まったとみなす刻み。
const THINKING_STEPS = [SHOW_AFTER_SECS];

export type StreamWaiting = {
  /// 末尾の操作のまとまりの見出し・本文前の見出しに出す文言（待っていなければ null）。
  label: string | null;
  /// 本文の末尾に待ち表示を出すか（本文の途中で止まったとき）。
  atBottom: boolean;
};

/// 回答の末尾が何か（生成中の表示をどこに出すかを決める）。
export type StreamTail = { kind: "text" } | { kind: "work"; hasTools: boolean } | null;

/// 生成中のストリームから、待ち表示の出し分けを決める。
///
/// - 末尾が操作のまとまり（ステップの合間・思考の後）: その見出しに文言を出す。
/// - 末尾が本文: 止まってから [`SHOW_AFTER_SECS`] 秒で本文の末尾に待ち表示を出す。
/// - まだ何も無い: 本文前の見出し（「準備しています」）に出す。
///
/// 思考（reasoning）が流れている間は「考えています」とし、止まってから待ちの文言を進める
/// （思考の後にツールの入力を長く書き出す間も、見出しが「考えています」で止まらない）。
export function useStreamWaiting({
  text,
  thinking,
  tools,
  tail,
  awaitingUser,
}: {
  text: string;
  thinking: string;
  tools: ToolActivityItem[];
  tail: StreamTail;
  /// 承認待ちなど、ユーザーの操作を待っている（こちらが待たせているのではない）。
  awaitingUser: boolean;
}): StreamWaiting {
  const toolsRunning = tools.some((t) => t.running);
  const done = tools.filter((t) => !t.running).length;
  const signal = `${text.length}:${thinking.length}:${tools.length}:${done}`;
  const waiting = !toolsRunning && !awaitingUser;
  const idleSecs = useIdleStep(signal, waiting, IDLE_STEPS);
  // 思考が最後に伸びてからの秒数（ツールの実行で途切れないよう常に数える）。
  const thinkingIdle = useIdleStep(String(thinking.length), true, THINKING_STEPS);
  const thinkingFlowing = thinking.trim().length > 0 && thinkingIdle < SHOW_AFTER_SECS;

  if (!waiting) return { label: null, atBottom: false };
  if (tail?.kind === "text") return { label: null, atBottom: idleSecs >= SHOW_AFTER_SECS };
  const context: WaitingContext =
    tail === null ? "start" : tail.hasTools ? "after_tool" : "after_thinking";
  return {
    label: thinkingFlowing ? "考えています" : waitingPhrase(context, idleSecs),
    atBottom: false,
  };
}

/// 本文の末尾に出す待ち表示（本文が途中で止まったとき）。止まってから
/// [`SHOW_AFTER_SECS`] 秒で出るので、そこから自分で数える（親の行を毎秒描き直さない）。
export function StreamWaitingLine() {
  const [secs, setSecs] = React.useState(SHOW_AFTER_SECS);
  React.useEffect(() => {
    const mounted = Date.now();
    const timer = window.setInterval(
      () => setSecs(SHOW_AFTER_SECS + Math.floor((Date.now() - mounted) / 1000)),
      1000,
    );
    return () => window.clearInterval(timer);
  }, []);
  const label = waitingPhrase("after_text", secs);
  return (
    <div
      role="status"
      data-testid="stream-waiting"
      className="flex items-center gap-2 py-1 text-[13px] animate-in fade-in-0 duration-300"
    >
      <Loader2 className="size-3.5 shrink-0 animate-spin text-muted-foreground" aria-hidden />
      {/* 入れ替えのアニメーションは外側に置く（shimmer と同じ要素だと光が止まる）。 */}
      <span key={label} className="flex min-w-0 animate-in fade-in-0 duration-300">
        <span className="shiki-text-shimmer min-w-0 truncate font-medium">{label}…</span>
      </span>
      {secs >= ELAPSED_AFTER_SECS ? (
        <span className="shrink-0 tabular-nums text-muted-foreground" aria-hidden>
          ・ {secs}秒
        </span>
      ) : null}
    </div>
  );
}
