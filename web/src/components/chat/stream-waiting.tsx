"use client";

/// 生成中の「待たされている」表示。
///
/// モデルが大きなツール入力（Office 文書の中身など）を書き出している間や、ステップの合間は
/// イベントが数十秒届かない。何も動かないと固まったように見えるので、最後のイベントからの
/// 経過で文言を進め、回転アイコンと光の流れる文字で「動いている」ことを伝える。

import * as React from "react";
import { Loader2 } from "lucide-react";

import type { ToolActivityItem } from "./tool-activity";
import { waitingPhrase, type WaitingContext } from "@/lib/waiting-phrases";

/// 本文がこれだけ流れたら回答に入ったとみなし、ツールのローリングを畳む。ツールの前置き
/// （「〜を調べます。」）程度で畳むと、次のツールでまた開いてパカパカする。
const ANSWER_MIN_CHARS = 80;
/// 本文の途中で止まってから、末尾に待ち表示を出すまでの秒数（トークンの小休止では出さない）。
const SHOW_AFTER_SECS = 2;
/// 経過秒を添え始める秒数。
const ELAPSED_AFTER_SECS = 10;

/// `signal` が最後に変わってからの秒数（`active` の間だけ進める）。
function useIdleSeconds(signal: string, active: boolean): number {
  const [idle, setIdle] = React.useState(0);
  React.useEffect(() => {
    setIdle(0);
    if (!active) return;
    const since = Date.now();
    const timer = window.setInterval(() => setIdle((Date.now() - since) / 1000), 500);
    return () => window.clearInterval(timer);
  }, [signal, active]);
  return idle;
}

export type StreamWaiting = {
  /// 最後のツールの後に回答の本文が流れ始めた（ツールのローリングを畳む）。
  answering: boolean;
  /// ステップの合間・思考見出しに出す文言（待っていなければ null）。
  label: string | null;
  /// 本文の末尾に待ち表示を出すか（ツールの見出しや思考の見出しが受け持たない局面）。
  atBottom: boolean;
  /// 最後のイベントからの秒数。
  idleSecs: number;
};

/// 生成中のストリームから、待ち表示の出し分けを決める。
///
/// 思考（reasoning）の差分は待ちを解かない。思考が流れている間も本文やツールは止まって
/// いるので、そこで文言をリセットし続けると「結果を確認しています」から進まなくなる。
export function useStreamWaiting({
  text,
  textAtLastTool,
  tools,
  awaitingUser,
}: {
  text: string;
  /// 最後のツール呼び出し時点の本文の長さ（それ以降が「ツールの後の本文」）。
  textAtLastTool: number;
  tools: ToolActivityItem[];
  /// 承認待ちなど、ユーザーの操作を待っている（こちらが待たせているのではない）。
  awaitingUser: boolean;
}): StreamWaiting {
  const toolsRunning = tools.some((t) => t.running);
  const done = tools.filter((t) => !t.running).length;
  const signal = `${text.length}:${tools.length}:${done}`;
  const waiting = !toolsRunning && !awaitingUser;
  const idleSecs = useIdleSeconds(signal, waiting);

  const sinceTool = text.slice(textAtLastTool).trim();
  const hasTools = tools.length > 0;
  const answering = !toolsRunning && sinceTool.length >= ANSWER_MIN_CHARS;
  const context: WaitingContext =
    hasTools && sinceTool.length === 0 ? "after_tool" : text.trim() ? "after_text" : "start";
  // 本文が流れている最中は「待ち」ではない（止まってから待ちの文言を進める）。
  const flowing = context === "after_text" && idleSecs < SHOW_AFTER_SECS;
  const label = waiting ? (flowing ? "回答を書いています" : waitingPhrase(context, idleSecs)) : null;
  // ステップの合間はツールの見出しが、本文が出る前は思考の見出しが文言を受け持つ。
  const between = hasTools && !answering;
  const atBottom = waiting && !between && text.trim().length > 0;
  return {
    answering,
    label: atBottom ? null : label,
    atBottom: atBottom && idleSecs >= SHOW_AFTER_SECS,
    idleSecs,
  };
}

/// 本文の末尾に出す待ち表示（本文が途中で止まったとき）。
export function StreamWaitingLine({ idleSecs }: { idleSecs: number }) {
  const secs = Math.floor(idleSecs);
  const label = waitingPhrase("after_text", idleSecs);
  return (
    <div
      role="status"
      data-testid="stream-waiting"
      className="flex items-center gap-2 py-1 text-[13px] animate-in fade-in-0 duration-300"
    >
      <Loader2 className="size-3.5 shrink-0 animate-spin text-muted-foreground" aria-hidden />
      <span
        key={label}
        className="shiki-text-shimmer min-w-0 truncate font-medium animate-in fade-in-0 duration-300"
      >
        {label}…
      </span>
      {secs >= ELAPSED_AFTER_SECS ? (
        <span className="shrink-0 tabular-nums text-muted-foreground" aria-hidden>
          ・ {secs}秒
        </span>
      ) : null}
    </div>
  );
}
