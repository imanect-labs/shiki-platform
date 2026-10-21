"use client";

/// スマホ（md 未満）のフロー表示: キャンバスの代わりに「きっかけ → 各ステップ」を縦のカードで並べる。
///
/// - カードの間には常時表示の＋（32px）を置き、その位置にブロックを差し込む（つながりの間・
///   出口の末尾・きっかけ直後の 3 通り）。
/// - 分岐（はい／いいえ・振り分けの各出口・エラー時など）は出口名の行と字下げで簡易に表す。
///   合流（2 本目以降の入エッジ）は「〜へ合流」の行で示し、同じカードを二重に描かない。
/// - 完全なグラフ編集（任意の接続・座標）はキャンバスで行う（「キャンバスで開く」）。

import * as React from "react";
import { AlertCircle, ChevronRight, CornerDownRight, GitMerge, Plus } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { Node as IrNode, Trigger, ValidationError, WorkflowIr } from "@/generated/workflow-ir";
import { categoryVar } from "../category-accent";
import type { AddTarget } from "./add-target";
import { nodeIcon } from "./icons";
import type { EditorAction, Selection } from "./ir-state";
import { outputPorts, portLabel } from "./node-card";
import { triggerSeasonIndex, triggerView } from "./trigger-node";
import { mapErrors } from "./validation";

type Props = {
  ir: WorkflowIr;
  selection: Selection;
  serverErrors: ValidationError[];
  dispatch: React.Dispatch<EditorAction>;
  /// ＋が押された（追加先を親へ渡し、親がブロック選択のシートを開く）。
  onAdd: (target: AddTarget, contextLabel: string) => void;
};

/// 字下げの上限（深い入れ子でもカードが潰れないようにする）。
const MAX_DEPTH = 3;

function nodeName(node: IrNode): string {
  return node.label || NODE_CATALOG.find((c) => c.type === node.type)?.label_ja || node.id;
}

function Indent({ depth, children }: { depth: number; children: React.ReactNode }) {
  const d = Math.min(depth, MAX_DEPTH);
  return (
    <div
      className={cn(d > 0 && "border-l border-border")}
      style={d > 0 ? { marginLeft: `${(d - 1) * 14 + 6}px`, paddingLeft: "12px" } : undefined}
    >
      {children}
    </div>
  );
}

/// カード間の＋（縦の細い線の中央に置く）。
function AddSlot({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <div className="relative flex h-12 items-center justify-center">
      <span aria-hidden className="absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-border" />
      <button
        type="button"
        onClick={onClick}
        aria-label={label}
        className={cn(
          "relative flex size-8 items-center justify-center rounded-full border bg-background text-muted-foreground shadow-sm outline-none",
          "transition-colors duration-fast hover:border-primary hover:text-primary active:scale-95",
          "focus-visible:ring-2 focus-visible:ring-ring",
          // 見た目は 32px のまま、指の当たり判定だけ 44px に広げる。
          "after:absolute after:-inset-1.5 after:content-['']",
        )}
      >
        <Plus className="size-4" aria-hidden />
      </button>
    </div>
  );
}

function StepCard({
  icon: Icon,
  accent,
  eyebrow,
  name,
  detail,
  selected,
  errorCount = 0,
  onClick,
}: {
  icon: React.ElementType;
  accent: string;
  eyebrow: string;
  name: string;
  detail?: string;
  selected: boolean;
  errorCount?: number;
  onClick: () => void;
}) {
  const hasError = errorCount > 0;
  return (
    <button
      type="button"
      onClick={onClick}
      aria-current={selected ? "true" : undefined}
      className={cn(
        "flex min-h-[64px] w-full items-center gap-3 rounded-xl border bg-card px-3.5 py-3 text-left outline-none",
        "transition-colors duration-fast active:bg-accent focus-visible:ring-2 focus-visible:ring-ring",
        selected && "bg-accent",
        hasError && "border-destructive/60",
      )}
    >
      <span
        className={cn(
          "flex size-10 shrink-0 items-center justify-center rounded-[10px]",
          hasError && "bg-destructive/10 text-destructive",
        )}
        style={
          hasError
            ? undefined
            : { backgroundColor: `color-mix(in oklab, ${accent} 14%, transparent)`, color: accent }
        }
      >
        <Icon className="size-[18px]" aria-hidden />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[11.5px] text-muted-foreground">{eyebrow}</span>
        <span className="block truncate text-[15px] font-medium leading-snug">{name}</span>
        {detail ? (
          <span className="block truncate text-xs text-muted-foreground">{detail}</span>
        ) : null}
      </span>
      {hasError ? (
        <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-destructive/10 px-2 py-0.5 text-[11px] font-medium text-destructive">
          <AlertCircle className="size-3" aria-hidden />
          要修正 {errorCount}
        </span>
      ) : null}
      <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden />
    </button>
  );
}

function PortRow({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-1.5 pb-1 pt-2 text-xs font-medium text-muted-foreground">
      <CornerDownRight className="size-3.5" aria-hidden />
      {label}
    </div>
  );
}

function MergeRow({ name }: { name: string }) {
  return (
    <div className="flex items-center gap-1.5 rounded-lg border border-dashed px-3 py-2 text-xs text-muted-foreground">
      <GitMerge className="size-3.5" aria-hidden />
      「{name}」へ合流
    </div>
  );
}

export function MobileFlow({ ir, selection, serverErrors, dispatch, onAdd }: Props) {
  const errors = React.useMemo(() => mapErrors(serverErrors), [serverErrors]);
  const byId = React.useMemo(() => new Map(ir.nodes.map((n) => [n.id, n])), [ir.nodes]);

  const select = (next: Selection) => dispatch({ type: "select", selection: next });

  const card = (node: IrNode) => {
    const entry = NODE_CATALOG.find((c) => c.type === node.type);
    return (
      <StepCard
        icon={nodeIcon(node.type)}
        accent={categoryVar(entry?.category)}
        eyebrow={entry?.category_label_ja ?? node.type}
        name={nodeName(node)}
        selected={selection.kind === "node" && selection.id === node.id}
        errorCount={errors.byNode.get(node.id)?.length ?? 0}
        onClick={() => select({ kind: "node", id: node.id })}
      />
    );
  };

  // 入口（入エッジ 0 本）から辿って縦に並べる。訪問済みは合流行にする（循環・join の二重描画防止）。
  const visited = new Set<string>();
  const items: React.ReactNode[] = [];
  let key = 0;
  const push = (depth: number, node: React.ReactNode) =>
    items.push(
      <Indent key={key++} depth={depth}>
        {node}
      </Indent>,
    );

  const walk = (startId: string, depth: number) => {
    let currentId: string | null = startId;
    while (currentId) {
      const node = byId.get(currentId);
      if (!node) return;
      if (visited.has(node.id)) {
        push(depth, <MergeRow name={nodeName(node)} />);
        return;
      }
      visited.add(node.id);
      push(depth, card(node));

      const ports = outputPorts(node);
      const outs = ir.edges.filter((e) => e.from === node.id);
      const name = nodeName(node);
      if (ports.length === 1) {
        const port = ports[0];
        const edges = outs.filter((e) => (e.from_port ?? "out") === port);
        if (edges.length === 0) {
          push(
            depth,
            <AddSlot
              label={`「${name}」の後ろにブロックを追加`}
              onClick={() => onAdd({ kind: "after", from: node.id, port }, `「${name}」の後ろに追加`)}
            />,
          );
          return;
        }
        if (edges.length === 1) {
          const edge = edges[0];
          push(
            depth,
            <AddSlot
              label={`「${name}」の後ろにブロックを挿入`}
              onClick={() => onAdd({ kind: "edge", edge }, `「${name}」の後ろに挿入`)}
            />,
          );
          currentId = edge.to;
          continue;
        }
        // 同じ出口から複数へ（並行）。各行き先を字下げで並べる。
        edges.forEach((edge, i) => {
          push(depth + 1, <PortRow label={`並行 ${i + 1}`} />);
          push(
            depth + 1,
            <AddSlot
              label={`「${name}」の後ろにブロックを挿入`}
              onClick={() => onAdd({ kind: "edge", edge }, `「${name}」の後ろに挿入`)}
            />,
          );
          walk(edge.to, depth + 1);
        });
        return;
      }
      // 複数の出口（分岐・振り分け・エラー時）。
      for (const port of ports) {
        const label = portLabel(port) || port;
        push(depth + 1, <PortRow label={label} />);
        const edges = outs.filter((e) => (e.from_port ?? "out") === port);
        if (edges.length === 0) {
          push(
            depth + 1,
            <AddSlot
              label={`「${name}」の「${label}」にブロックを追加`}
              onClick={() =>
                onAdd({ kind: "after", from: node.id, port }, `「${name}」の「${label}」に追加`)
              }
            />,
          );
          continue;
        }
        for (const edge of edges) {
          push(
            depth + 1,
            <AddSlot
              label={`「${name}」の「${label}」にブロックを挿入`}
              onClick={() => onAdd({ kind: "edge", edge }, `「${name}」の「${label}」に挿入`)}
            />,
          );
          walk(edge.to, depth + 1);
        }
      }
      return;
    }
  };

  const incoming = new Set(ir.edges.map((e) => e.to));
  const entries = ir.nodes.filter((n) => !incoming.has(n.id));
  if (ir.nodes.length === 0) {
    push(0, <AddSlot label="最初のブロックを追加" onClick={() => onAdd({ kind: "first" }, "最初のブロックを追加")} />);
  }
  entries.forEach((entry, i) => {
    if (i > 0) push(0, <PortRow label="別の入口" />);
    push(
      0,
      <AddSlot
        label={`「${nodeName(entry)}」の前にブロックを挿入`}
        onClick={() => onAdd({ kind: "before", before: entry.id }, `「${nodeName(entry)}」の前に挿入`)}
      />,
    );
    walk(entry.id, 0);
  });
  const orphans = ir.nodes.filter((n) => !visited.has(n.id));

  return (
    <div className="flex flex-col px-4 pb-6" data-testid="workflow-mobile-flow">
      <ol className="flex flex-col gap-2">
        {(ir.triggers as Trigger[]).map((t, i) => {
          const view = triggerView(t);
          return (
            <li key={i}>
              <StepCard
                icon={view.icon}
                accent={seasonVar(triggerSeasonIndex(t.kind))}
                eyebrow="きっかけ"
                name={view.label}
                detail={view.detail}
                selected={selection.kind === "trigger" && selection.index === i}
                onClick={() => select({ kind: "trigger", index: i })}
              />
            </li>
          );
        })}
      </ol>
      {items}
      {orphans.length > 0 ? (
        <div className="mt-6 flex flex-col gap-2">
          <p className="text-xs font-medium text-muted-foreground">
            つながっていないブロック（キャンバスでつなげます）
          </p>
          {orphans.map((n) => (
            <div key={n.id}>{card(n)}</div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
