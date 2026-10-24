"use client";

/// 回答を**起きた順**に並べる（本文 → 操作・思考 → 本文 → …）。
///
/// 旧実装は思考とツールを回答の先頭にまとめ、本文を下に連結していた。本文とツールを往復する
/// 回答では、いま動いている操作の表示が一番上に残り、読んでいる位置（最新の本文の下）から
/// 離れる。ここでは本文と「操作のまとまり」（連続する思考・ツール呼び出し）を交互に並べ、
/// 生成中の表示が常に末尾に来るようにする。サーバの projection（message.content）も
/// イベント順なので、確定後の履歴も同じ並びで描ける（生成中と確定後で配置が変わらない）。

import * as React from "react";

import type { Citation, ContentBlock } from "@/lib/chat-api";
import { linkifyCitations } from "@/lib/citation";
import { Markdown } from "@/components/prompt-kit/markdown";
import { ChainOfThought } from "./chain-of-thought";
import type { ToolActivityItem } from "./tool-activity";

/// 描画用の 1 区切り。
export type TimelineSegment =
  | { kind: "text"; key: string; text: string }
  | { kind: "work"; key: string; thinking: string; tools: ToolActivityItem[] };

/// 生成中の区切り（ツールは `StreamState.tools` の key で参照し、結果の反映をそちらに任せる）。
export type StreamSegment =
  | { kind: "text"; text: string }
  | { kind: "work"; thinking: string; toolKeys: string[] };

/// 本文の差分を足す（直前が本文ならつなげる）。
export function appendStreamText(segments: StreamSegment[], text: string): StreamSegment[] {
  const last = segments[segments.length - 1];
  if (last?.kind === "text") {
    return [...segments.slice(0, -1), { kind: "text", text: last.text + text }];
  }
  return [...segments, { kind: "text", text }];
}

/// 思考の差分を足す（直前が**まだツールの無い**操作のまとまりならそこへ）。
///
/// ツールの後に来た思考は新しいまとまりにする。同じまとまりに入れると、思考の行（まとまりの
/// 先頭）がツールの上に差し込まれて、表示済みのツールが下へ押し出される。
export function appendStreamThinking(segments: StreamSegment[], text: string): StreamSegment[] {
  const last = segments[segments.length - 1];
  if (last?.kind === "work" && last.toolKeys.length === 0) {
    return [...segments.slice(0, -1), { ...last, thinking: last.thinking + text }];
  }
  return [...segments, { kind: "work", thinking: text, toolKeys: [] }];
}

/// ツール呼び出しを足す（直前が操作のまとまりならそこへ）。
export function appendStreamTool(segments: StreamSegment[], key: string): StreamSegment[] {
  const last = segments[segments.length - 1];
  if (last?.kind === "work") {
    return [...segments.slice(0, -1), { ...last, toolKeys: [...last.toolKeys, key] }];
  }
  return [...segments, { kind: "work", thinking: "", toolKeys: [key] }];
}

/// 生成中の区切りを描画用にする。
export function resolveStreamSegments(
  segments: StreamSegment[],
  tools: ToolActivityItem[],
): TimelineSegment[] {
  const byKey = new Map(tools.map((t) => [t.key, t]));
  return segments.map((s, i) =>
    s.kind === "text"
      ? { kind: "text", key: `s${i}`, text: s.text }
      : {
          kind: "work",
          key: `s${i}`,
          thinking: s.thinking,
          tools: s.toolKeys.flatMap((k) => byKey.get(k) ?? []),
        },
  );
}

/// 確定メッセージのブロック列（イベント順）を区切りにする。
///
/// ツール結果は tool_call_id で引く（#358/#386）。**同じ id が複数回現れ得る**ため
/// （ループで再利用される呼び出し ID）、id ごとに出現順のキューとして持ち、
/// 呼び出しへ順番に対応付ける（最後の結果を全行へ適用しない）。
export function segmentsFromBlocks(blocks: ContentBlock[]): TimelineSegment[] {
  const results = new Map<string, Extract<ContentBlock, { type: "tool_result" }>[]>();
  for (const b of blocks) {
    if (b.type !== "tool_result") continue;
    const q = results.get(b.tool_call_id);
    if (q) q.push(b);
    else results.set(b.tool_call_id, [b]);
  }
  const out: TimelineSegment[] = [];
  let toolIndex = 0;
  // `afterTools` なら、ツールを含むまとまりには足さない（思考はツールの後なら新しいまとまり・
  // 生成中の appendStreamThinking と同じ区切り方）。
  const work = (afterTools = false) => {
    const last = out[out.length - 1];
    if (last?.kind === "work" && !(afterTools && last.tools.length > 0)) return last;
    const seg: TimelineSegment = { kind: "work", key: `s${out.length}`, thinking: "", tools: [] };
    out.push(seg);
    return seg;
  };
  for (const b of blocks) {
    if (b.type === "text") {
      // 先頭は空行だけを落とす（字下げは残す）。```ui のボタンの id はサーバーが保存済みの各ブロックを
      // そのまま読んで求めるので、フェンスの字下げを変えると id が合わなくなる（#604）。
      const text = b.text.replace(/^(?:[ \t]*\r?\n)+/, "").trimEnd();
      if (!text) continue;
      const last = out[out.length - 1];
      if (last?.kind === "text") last.text = `${last.text}\n\n${text}`;
      else out.push({ kind: "text", key: `s${out.length}`, text });
    } else if (b.type === "thinking") {
      work(true).thinking += b.text;
    } else if (b.type === "tool_call") {
      const res = results.get(b.id)?.shift();
      work().tools.push({
        // 確定メッセージ内での出現位置は安定（再レンダーで並びが変わらない）。
        key: `${b.id}-${toolIndex++}`,
        id: b.id,
        name: b.name,
        running: false,
        input: b.input,
        // 生成型は Option<u32> を `number | null` にする。UI は「不明」を undefined で扱う。
        step: b.step ?? undefined,
        ok: res?.ok ?? undefined,
        result: res?.content,
      });
    }
  }
  return out;
}

/// 区切りを順に描く。生成中は末尾の区切りだけが「動いている」。
///
/// 参照ドキュメントのチップは最初の操作のまとまりに付ける（操作が無ければ先頭に単独で出す）。
export function MessageTimeline({
  segments,
  citations,
  streaming = false,
  phase = null,
  waitingLabel = null,
}: {
  segments: TimelineSegment[];
  citations: Citation[];
  streaming?: boolean;
  /// 計画の進行中サブタスク（自律 run）。生成中の操作のまとまりの見出しに優先して出す。
  phase?: string | null;
  /// 次のイベントを待っている間の文言（末尾の操作のまとまり・本文前の見出しに出す）。
  waitingLabel?: string | null;
}) {
  const firstWork = segments.findIndex((s) => s.kind === "work");
  const lastText = segments.findLastIndex((s) => s.kind === "text");
  const leading =
    // 本文も操作もまだ無い（生成の開始直後）、または操作が無く引用だけある。
    (streaming && segments.length === 0) || (firstWork < 0 && citations.length > 0);
  return (
    <>
      {leading ? (
        <ChainOfThought
          thinking=""
          tools={[]}
          citations={citations}
          streaming={streaming}
          waitingLabel={streaming ? waitingLabel : null}
        />
      ) : null}
      {segments.map((s, i) => {
        const live = streaming && i === segments.length - 1;
        if (s.kind === "work") {
          return (
            <ChainOfThought
              key={s.key}
              thinking={s.thinking}
              tools={s.tools}
              citations={i === firstWork ? citations : []}
              streaming={live}
              phase={live ? phase : null}
              waitingLabel={live ? waitingLabel : null}
            />
          );
        }
        return (
          <div key={s.key} className="text-[15px] leading-relaxed">
            <Markdown streaming={streaming && i === lastText}>
              {linkifyCitations(s.text, citations)}
            </Markdown>
          </div>
        );
      })}
    </>
  );
}
