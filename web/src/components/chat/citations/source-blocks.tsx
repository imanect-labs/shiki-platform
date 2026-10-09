"use client";

/// 出典パネルの原文（引用箇所の前後をブロック列で描く・#508）。
///
/// 引用はサーバが「版ごとの正規化ブロック列（doc_block）の中の範囲」で位置を返す。ここでは
/// その前後を窓で取り、見出し・段落・箇条書き・表をそのまま描いて、引用した範囲を塗る。
/// docx も md も PDF も同じ描き方になる（原本のバイト位置には戻さない）。
///
/// オフセットは UTF-16（JS の文字列の添字と同じ）。位置情報を持たない旧インデックスの引用は
/// 呼び出し側が抜粋表示へ落とす。
import * as React from "react";
import { ChevronDown, ChevronUp, Loader2 } from "lucide-react";

import { cn } from "@/lib/utils";
import type { Citation } from "@/lib/chat-api";
import { getVersionBlocks, type DocBlock } from "@/lib/storage";

/// 引用箇所の前に最初から見せるブロック数（見出しまで届くことが多い）。
const BEFORE = 6;
/// 引用箇所の後に最初から見せるブロック数。
const AFTER = 8;
/// 「前を表示」「続きを表示」で 1 回に足すブロック数。
const STEP = 20;

type Anchor = NonNullable<Citation["anchor"]>;

type Window = { blocks: DocBlock[]; from: number; nextFrom: number | null };

type State =
  | { status: "loading" }
  | { status: "error" }
  | { status: "empty" }
  | { status: "ready"; window: Window; more: "before" | "after" | null };

/// 位置情報を持つ引用か（持たなければ抜粋表示にする）。
export function hasBlockAnchor(c: Citation): c is Citation & { anchor: Anchor; version: number } {
  return c.anchor != null && typeof c.version === "number";
}

export function SourceBlocks({
  citation,
  fallback,
  className,
}: {
  citation: Citation & { anchor: Anchor; version: number };
  /// ブロック列が取れない（解析前の版・読み込み失敗）ときに出すもの（抜粋表示）。
  fallback: React.ReactNode;
  className?: string;
}) {
  const { node_id: nodeId, version, anchor } = citation;
  const [state, setState] = React.useState<State>({ status: "loading" });
  const markRef = React.useRef<HTMLDivElement>(null);
  const scrollRef = React.useRef<HTMLDivElement>(null);
  // 「前を表示」「続きを表示」の結果を、閉じた後・別の引用へ移った後に反映しない。
  const current = React.useRef(0);
  const scrolledFor = React.useRef<string | null>(null);
  const key = `${nodeId}:${version}:${anchor.block_start}:${anchor.off_start}`;

  React.useEffect(() => {
    let active = true;
    const gen = current;
    gen.current++;
    const from = Math.max(0, anchor.block_start - BEFORE);
    const limit = Math.min(200, anchor.block_end - from + 1 + AFTER);
    setState({ status: "loading" });
    getVersionBlocks(nodeId, version, { from, limit })
      .then((page) => {
        if (!active) return;
        setState(
          page.blocks.length === 0
            ? { status: "empty" }
            : {
                status: "ready",
                window: { blocks: page.blocks, from: page.blocks[0].ordinal, nextFrom: page.next_from ?? null },
                more: null,
              },
        );
      })
      .catch(() => active && setState({ status: "error" }));
    return () => {
      active = false;
      // 閉じた・別の引用へ移ったら世代を進め、読み込み中の「前/続き」の結果を捨てさせる。
      gen.current++;
    };
  }, [nodeId, version, anchor.block_start, anchor.block_end]);

  // 開いた・別の引用へ送ったときに、引用箇所を原文の枠の上 1/3 あたりへ寄せる（窓を広げたときは
  // 動かさない）。scrollIntoView はパネル全体まで動かし、回答の記述や PDF の縮小表示を押し出すので、
  // この枠の中だけをスクロールする。
  React.useEffect(() => {
    if (state.status !== "ready" || scrolledFor.current === key) return;
    scrolledFor.current = key;
    const box = scrollRef.current;
    const mark = markRef.current;
    // 枠は position: relative なので、mark.offsetTop は枠の先頭からの距離になる。
    if (box && mark) box.scrollTop = Math.max(0, mark.offsetTop - box.clientHeight / 4);
  }, [state, key]);

  const loadBefore = () => {
    // 読み込み中はもう一方も押せない（古い窓を閉じ込めた 2 つの結果が上書きし合うため）。
    if (state.status !== "ready" || state.more || state.window.from === 0) return;
    const cur = state.window;
    const from = Math.max(0, cur.from - STEP);
    const token = current.current;
    setState({ ...state, more: "before" });
    getVersionBlocks(nodeId, version, { from, limit: cur.from - from })
      .then((page) => {
        if (current.current !== token) return;
        setState({
          status: "ready",
          window: { ...cur, blocks: [...page.blocks, ...cur.blocks], from },
          more: null,
        });
      })
      .catch(() => current.current === token && setState({ status: "ready", window: cur, more: null }));
  };

  const loadAfter = () => {
    if (state.status !== "ready" || state.more || state.window.nextFrom == null) return;
    const cur = state.window;
    const token = current.current;
    setState({ ...state, more: "after" });
    getVersionBlocks(nodeId, version, { from: cur.nextFrom!, limit: STEP })
      .then((page) => {
        if (current.current !== token) return;
        setState({
          status: "ready",
          window: { ...cur, blocks: [...cur.blocks, ...page.blocks], nextFrom: page.next_from ?? null },
          more: null,
        });
      })
      .catch(() => current.current === token && setState({ status: "ready", window: cur, more: null }));
  };

  if (state.status === "loading") {
    return (
      <div className={cn("flex items-center gap-2 px-1 py-6 text-[12.5px] text-muted-foreground", className)}>
        <Loader2 className="size-3.5 animate-spin" aria-hidden />
        原文を読み込んでいます…
      </div>
    );
  }
  if (state.status !== "ready") return <>{fallback}</>;

  const { blocks, from, nextFrom } = state.window;
  let firstMarked = true;
  return (
    <div
      ref={scrollRef}
      className={cn("relative max-h-[26rem] overflow-y-auto scrollbar-subtle", className)}
      data-testid="source-blocks"
    >
      {from > 0 ? (
        <MoreButton
          onClick={loadBefore}
          busy={state.more === "before"}
          disabled={state.more != null}
          direction="before"
        />
      ) : null}
      <div className="space-y-2.5 text-[13.5px] leading-[1.85] text-foreground/85">
        {blocks.map((b) => {
          const range = markRange(b, anchor);
          const ref = range && firstMarked ? markRef : undefined;
          if (range) firstMarked = false;
          return (
            <div key={b.ordinal} ref={ref} data-ordinal={b.ordinal} data-cited={range ? "" : undefined}>
              <BlockView block={b} range={range} />
            </div>
          );
        })}
      </div>
      {nextFrom != null ? (
        <MoreButton
          onClick={loadAfter}
          busy={state.more === "after"}
          disabled={state.more != null}
          direction="after"
        />
      ) : null}
    </div>
  );
}

/// ブロック内の引用範囲（UTF-16）。範囲外なら null。
function markRange(b: DocBlock, a: Anchor): [number, number] | null {
  if (b.ordinal < a.block_start || b.ordinal > a.block_end) return null;
  const start = b.ordinal === a.block_start ? a.off_start : 0;
  const end = b.ordinal === a.block_end ? a.off_end : b.text.length;
  return [Math.max(0, start), Math.min(b.text.length, Math.max(start, end))];
}

const MARK =
  "rounded-[3px] bg-[var(--doc,var(--season-autumn))]/20 px-0.5 text-foreground [box-decoration-break:clone]";

function Marked({ text, range }: { text: string; range: [number, number] | null }) {
  if (!range) return <>{text}</>;
  const [s, e] = range;
  return (
    <>
      {text.slice(0, s)}
      <mark className={MARK}>{text.slice(s, e)}</mark>
      {text.slice(e)}
    </>
  );
}

function BlockView({ block, range }: { block: DocBlock; range: [number, number] | null }) {
  switch (block.type) {
    case "heading": {
      const level = Math.min(Math.max(block.level ?? 2, 1), 4);
      return (
        <p
          className={cn(
            "font-semibold text-foreground",
            level <= 1 ? "pt-1 text-[15px]" : level === 2 ? "pt-1 text-[14px]" : "text-[13.5px]",
          )}
        >
          <Marked text={block.text} range={range} />
        </p>
      );
    }
    case "list_item":
      return (
        <p className="flex gap-2 pl-1">
          <span className="shrink-0 tabular-nums text-muted-foreground">{block.list_marker || "•"}</span>
          <span className="min-w-0 whitespace-pre-wrap">
            <Marked text={block.text} range={range} />
          </span>
        </p>
      );
    case "caption":
      return (
        <p className="text-[12px] italic text-muted-foreground">
          <Marked text={block.text} range={range} />
        </p>
      );
    case "table":
      // 表は worker が Markdown の表にして寄越す。**Markdown として描かない**（アップロードされた
      // 文書のセルに画像やリンクの記法があると、開いた人のブラウザが外部へリクエストを出すため）。
      // 行と列に割って文字として描き、引用範囲に入っていれば表ごと縁取る。
      return (
        <div
          className={cn(
            "overflow-x-auto rounded-md",
            range && "bg-[var(--doc,var(--season-autumn))]/10 ring-1 ring-[var(--doc,var(--season-autumn))]/40",
          )}
        >
          <PlainTable text={block.text} />
        </div>
      );
    default:
      return (
        <p className="whitespace-pre-wrap">
          <Marked text={block.text} range={range} />
        </p>
      );
  }
}

/// Markdown の表（`| a | b |`）を行と列に割る。区切り行（`|---|`）は捨てる。
export function parseMarkdownTable(text: string): string[][] {
  return text
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.startsWith("|"))
    .filter((l) => !/^\|[\s:|-]+\|?$/.test(l))
    .map((l) =>
      l
        .replace(/^\|/, "")
        .replace(/\|$/, "")
        .split(/(?<!\\)\|/)
        .map((c) => c.replace(/\\\|/g, "|").trim()),
    );
}

function PlainTable({ text }: { text: string }) {
  const rows = parseMarkdownTable(text);
  if (rows.length === 0) return <p className="whitespace-pre-wrap text-[12.5px]">{text}</p>;
  const [head, ...body] = rows;
  return (
    <table className="w-full border-collapse text-[12.5px] leading-relaxed">
      <thead>
        <tr>
          {head.map((c, i) => (
            <th key={i} className="border-b border-border px-2 py-1 text-left font-medium text-foreground/80">
              {c}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {body.map((r, i) => (
          <tr key={i}>
            {r.map((c, j) => (
              <td key={j} className="border-b border-border/60 px-2 py-1 align-top">
                {c}
              </td>
            ))}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function MoreButton({
  onClick,
  busy,
  disabled,
  direction,
}: {
  onClick: () => void;
  busy: boolean;
  disabled: boolean;
  direction: "before" | "after";
}) {
  const Icon = direction === "before" ? ChevronUp : ChevronDown;
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "flex w-full items-center justify-center gap-1 rounded-md py-1.5 text-[12px] text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-60",
        direction === "before" ? "mb-2" : "mt-2",
      )}
    >
      {busy ? <Loader2 className="size-3.5 animate-spin" aria-hidden /> : <Icon className="size-3.5" aria-hidden />}
      {direction === "before" ? "前を表示" : "続きを表示"}
    </button>
  );
}
