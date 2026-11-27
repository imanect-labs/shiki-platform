"use client";

/// ミニアプリの画面の「組み立てる」（#604 の続き・y-ui docs/editor.md §6）。
///
/// 画面（Yui の ```ui を含む Markdown）を段（文章・入力とボタン・表・そのままの図）の並びにして、
/// 段の並べ替え・追加・削除と、app の段の中身のドラッグ＆ドロップ編集（Yui の AppEditor）をする。
/// 表の段（```data_grid）は、束ねたビューから出すものを選ぶ（GAP-11）。
///
/// - 保存する形は今と同じ Markdown。直した app の段だけを正規形に書き直し、ほかの段は 1 文字も変えない。
/// - 記述に読めない行がある app（診断のあるもの）は開かず、書くタブで直してもらう（黙って行を落とさない）。
/// - ボタンの実行先は、束ねたワークフローの別名からだけ選べる。チェッカーにエラーがある間は保存させない
///   （親が `onValidity` を見る）。最後の判定は保存時のサーバーの検証（crates/gui の miniapp.rs）。

import * as React from "react";
import { ArrowDown, ArrowUp, GripVertical, Plus, Table2, Trash2, TriangleAlert } from "lucide-react";
import type { AppFigure } from "@imanect/yui";
import { checkApp, emptyApp, type WorkflowChoice } from "@imanect/yui/editor";
import { AppEditor, useDnd } from "@imanect/yui/editor/react";

import { Markdown } from "@/components/prompt-kit/markdown";
import { Button } from "@/components/ui/button";
import {
  joinScreen,
  newAppBlock,
  newGridBlock,
  newTextBlock,
  splitScreen,
  type ScreenBlock,
} from "@/lib/miniapp/screen-blocks";
import { cn } from "@/lib/utils";

export interface ScreenValidity {
  /// 保存を止めるエラーの数（app の段のチェッカーの error の合計）。
  errors: number;
}

export function ScreenBuilder({
  screen,
  onScreenChange,
  workflows,
  views,
  appendGrid,
  onValidity,
  onOpenSource,
}: {
  screen: string;
  onScreenChange: (screen: string) => void;
  workflows: readonly WorkflowChoice[];
  /// 束ねたビューの名前（表の段が出せるもの・GAP-11）。
  views: readonly string[];
  /// 表の段を最後に足す頼み（ビューを追加したとき）。`n` が変わるたびに 1 回だけ足す（開いたときの頼みは済んだものとする）。
  appendGrid?: { alias: string; n: number } | null;
  onValidity: (v: ScreenValidity) => void;
  /// 書くタブへ移る（直せない段を記述で直すため）。
  onOpenSource: () => void;
}) {
  // 段は開いたときの画面から作り、以後はここで持つ（打つたびに分け直すと段の id が変わるため）。
  const [blocks, setBlocks] = React.useState<ScreenBlock[]>(() => splitScreen(screen));
  const [normalizedNotice, setNormalizedNotice] = React.useState(false);
  const root = React.useRef<HTMLDivElement | null>(null);

  const commit = React.useCallback(
    (next: ScreenBlock[]) => {
      setBlocks(next);
      onScreenChange(joinScreen(next));
    },
    [onScreenChange],
  );

  // ビューを追加したときの表の段（親の頼み）。段は手元の並びの最後に足す。
  const blocksRef = React.useRef(blocks);
  blocksRef.current = blocks;
  const doneRequest = React.useRef(appendGrid?.n ?? 0);
  React.useEffect(() => {
    if (!appendGrid || appendGrid.n === doneRequest.current) return;
    doneRequest.current = appendGrid.n;
    commit([...blocksRef.current, newGridBlock(appendGrid.alias)]);
  }, [appendGrid, commit]);

  // 保存を止めるのは、組み立てで直した app の段のエラーだけ（直していない段は元の記述のまま保存され、サーバーが確かめる）
  const errors = React.useMemo(
    () => blocks.reduce((n, b) => (b.type === "app" && b.touched ? n + checkApp(b.figure, { workflows, allowChat: false }).filter((p) => p.severity === "error").length : n), 0),
    [blocks, workflows],
  );
  React.useEffect(() => onValidity({ errors }), [errors, onValidity]);

  // 元の画面で空行だけだった段（図と文章の間の区切り）は見せない。つなぎ直しのために並びには残す
  const visible = blocks.flatMap((b, i) => (b.type === "text" && b.spacer ? [] : [i]));
  const update = (id: string, fn: (b: ScreenBlock) => ScreenBlock) => commit(blocks.map((b) => (b.id === id ? fn(b) : b)));
  /// 見えている段の並びで、from 番目を to 番目の前へ動かす（to は動かす前の位置。末尾は visible.length）。
  const move = (from: number, to: number) => {
    if (to < 0 || to > visible.length || from === to || from + 1 === to) return;
    const src = visible[from]!;
    const dest = to < visible.length ? visible[to]! : blocks.length;
    const next = blocks.slice();
    const [b] = next.splice(src, 1);
    next.splice(src < dest ? dest - 1 : dest, 0, b!);
    commit(next);
  };
  const remove = (id: string) => commit(blocks.filter((b) => b.id !== id));
  const add = (b: ScreenBlock) => commit([...blocks, b]);

  const { drag, start, line } = useDnd<{ index: number; label: string }>(root, {
    accepts: (_, list) => list === "screen-blocks",
    onDrop: (item, spot) => move(item.index, spot.index),
  });

  const firstWorkflow = workflows[0]?.name;
  const newApp = (): AppFigure => {
    const f = emptyApp("");
    if (!firstWorkflow) return f;
    return { ...f, sections: [{ fields: [], actions: [{ id: "a1", label: "実行する", target: { kind: "workflow", name: firstWorkflow }, confirm: true }] }] };
  };

  return (
    <div ref={root} className="space-y-3" data-testid="screen-builder">
      {normalizedNotice ? (
        <p className="rounded-lg border border-border/60 bg-secondary/50 px-3 py-2 text-xs leading-relaxed text-muted-foreground">
          組み立てで直した「入力とボタン」は、書くタブで標準の書き方（例: <code className="font-mono">必須</code>・
          <code className="font-mono">as</code> の位置）にそろいます。動きは変わりません。
        </p>
      ) : null}
      <div className="space-y-3" data-yui-list="screen-blocks" data-yui-axis="y">
        {visible.map((bi) => blocks[bi]!).map((b, i) => (
          <section
            key={b.id}
            data-yui-item=""
            className={cn(
              "rounded-xl border border-border/60 bg-card/40 transition-opacity",
              drag?.item.index === i && "opacity-40",
            )}
          >
            <header className="flex items-center gap-1 border-b border-border/40 px-2 py-1.5">
              <span
                role="button"
                tabIndex={-1}
                aria-hidden
                title="ドラッグで段を動かす"
                onPointerDown={(e) => start(e, { index: i, label: blockLabel(b) }, blockLabel(b), true)}
                className="flex size-8 cursor-grab touch-none items-center justify-center rounded-md text-muted-foreground/70 hover:bg-secondary hover:text-foreground pointer-coarse:size-10"
              >
                <GripVertical className="size-4" />
              </span>
              <span className="min-w-0 flex-1 truncate text-xs font-medium text-foreground/70">{blockLabel(b)}</span>
              <Button type="button" variant="ghost" size="icon" className="size-8" aria-label="前へ" disabled={i === 0} onClick={() => move(i, i - 1)}>
                <ArrowUp className="size-4" />
              </Button>
              <Button type="button" variant="ghost" size="icon" className="size-8" aria-label="後へ" disabled={i === visible.length - 1} onClick={() => move(i, i + 2)}>
                <ArrowDown className="size-4" />
              </Button>
              <Button type="button" variant="ghost" size="icon" className="size-8 text-muted-foreground hover:text-destructive" aria-label={`${blockLabel(b)}を消す`} onClick={() => remove(b.id)}>
                <Trash2 className="size-4" />
              </Button>
            </header>
            <div className="p-3">
              {b.type === "text" ? (
                <textarea
                  aria-label="文章"
                  value={b.body}
                  // 前後の空行（図との区切り）は段が別に持つので、ここでは中身だけを直す
                  onChange={(e) => {
                    const v = e.currentTarget.value;
                    update(b.id, (x) => (x.type === "text" ? { ...x, body: v } : x));
                  }}
                  rows={Math.min(8, Math.max(2, b.body.split("\n").length))}
                  placeholder="説明の文（Markdown）"
                  className="block w-full resize-y rounded-lg border border-input bg-background px-3 py-2 text-sm leading-relaxed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                />
              ) : b.type === "grid" ? (
                <GridBlock
                  block={b}
                  views={views}
                  onChange={(alias) => update(b.id, (x) => (x.type === "grid" ? { ...x, alias, touched: true } : x))}
                />
              ) : b.type === "app" ? (
                <AppEditor
                  figure={b.figure}
                  workflows={workflows}
                  allowChat={false}
                  onChange={(figure) => {
                    if (!normalizedNotice && b.raw) setNormalizedNotice(true);
                    update(b.id, (x) => (x.type === "app" ? { ...x, figure, touched: true } : x));
                  }}
                />
              ) : (
                <RawBlock block={b} onOpenSource={onOpenSource} />
              )}
            </div>
          </section>
        ))}
      </div>
      <div className="flex flex-wrap gap-2">
        <Button type="button" variant="outline" size="sm" onClick={() => add(newAppBlock(newApp()))}>
          <Plus className="size-4" aria-hidden />
          入力とボタン
        </Button>
        <Button type="button" variant="outline" size="sm" onClick={() => add(newTextBlock())}>
          <Plus className="size-4" aria-hidden />
          文章
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={views.length === 0}
          aria-describedby={views.length === 0 ? "screen-builder-grid-note" : undefined}
          onClick={() => add(newGridBlock(views[0]!))}
        >
          <Plus className="size-4" aria-hidden />
          表
        </Button>
      </div>
      {views.length === 0 ? (
        // 押せない理由はボタンの近くに文字で出す（付録 C の P15）。
        <p id="screen-builder-grid-note" className="text-xs text-muted-foreground">
          表を置くには、先に下の「表に出すビュー」でビューを追加してください。
        </p>
      ) : null}
      {drag ? (
        // 位置は useDnd が style を直接動かす（指を動かすたびに描き直さない）
        <div ref={(el) => void (line.current = el)} aria-hidden className="pointer-events-none fixed left-0 top-0 z-50 rounded bg-primary" style={{ display: "none" }} />
      ) : null}
    </div>
  );
}

function blockLabel(b: ScreenBlock): string {
  if (b.type === "text") return "文章";
  if (b.type === "grid") return b.alias ? `表「${b.alias}」` : "表";
  if (b.type === "app") return b.figure.title ? `入力とボタン「${b.figure.title}」` : "入力とボタン";
  return b.reason.kind === "figure" ? "図" : "入力とボタン（組み立てでは直せません）";
}

/// 表の段: 束ねたビューから、表に出すものを選ぶ。行は、アプリを使う人の権限で読み込む（ここでは描かない）。
function GridBlock({
  block,
  views,
  onChange,
}: {
  block: Extract<ScreenBlock, { type: "grid" }>;
  views: readonly string[];
  onChange: (alias: string) => void;
}) {
  const bundled = views.includes(block.alias);
  const id = React.useId();
  return (
    <div className="space-y-2" data-testid="screen-builder-grid">
      <label htmlFor={id} className="flex items-center gap-1.5 text-xs font-medium text-foreground/70">
        <Table2 className="size-3.5" aria-hidden />
        表に出すビュー
      </label>
      <select
        id={id}
        value={block.alias}
        onChange={(e) => onChange(e.currentTarget.value)}
        aria-describedby={bundled ? undefined : `${id}-missing`}
        className="h-9 w-full rounded-lg border border-input bg-background px-3 text-base md:text-sm pointer-coarse:h-11"
      >
        {bundled ? null : <option value={block.alias}>{block.alias ? `「${block.alias}」（追加していないビュー）` : "選んでいません"}</option>}
        {views.map((v) => (
          <option key={v} value={v}>
            {v}
          </option>
        ))}
      </select>
      {bundled ? (
        <p className="text-xs text-muted-foreground">
          レコードは、アプリを使う人の権限で読み込みます。保存して開くと、実際の表を試せます。
        </p>
      ) : (
        <p id={`${id}-missing`} role="note" className="text-xs text-amber-700 dark:text-amber-400">
          {block.alias
            ? `「${block.alias}」は、このアプリに追加したビューにありません。下の「表に出すビュー」で追加するか、ほかのビューを選んでください。`
            : "表に出すビューを選んでください。"}
        </p>
      )}
    </div>
  );
}

function RawBlock({ block, onOpenSource }: { block: Extract<ScreenBlock, { type: "raw" }>; onOpenSource: () => void }) {
  if (block.reason.kind === "figure") {
    return (
      <div className="px-3">
        <Markdown>{block.raw}</Markdown>
      </div>
    );
  }
  const lines = block.reason.kind === "diagnostics" ? block.reason.diagnostics : [];
  return (
    <div className="space-y-2" role="note">
      <p className="flex items-start gap-2 text-sm text-foreground">
        <TriangleAlert className="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-400" aria-hidden />
        {block.reason.kind === "unclosed"
          ? "この図は閉じの ``` がありません。書くタブで直してください。"
          : "この図には読めない記述があるため、組み立てでは開きません（開くと読めない行が消えてしまうため）。書くタブで直してください。"}
      </p>
      {lines.length ? (
        <ul className="space-y-0.5 pl-6 text-xs text-muted-foreground">
          {lines.slice(0, 5).map((d, k) => (
            <li key={k}>
              {d.line ? `${d.line} 行目: ` : ""}
              {d.message}
            </li>
          ))}
        </ul>
      ) : null}
      <pre className="max-h-40 overflow-auto rounded-lg bg-secondary/50 px-3 py-2 font-mono text-xs leading-relaxed">{block.source}</pre>
      <Button type="button" size="sm" variant="outline" onClick={onOpenSource}>
        書くタブで直す
      </Button>
    </div>
  );
}
