"use client";

/// 右サイドバー: 選択中のノード/トリガの設定パネル（Task 10.12）。
///
/// フォームは codegen の typed 契約（NodeParamsByType）で型検査された per-node 実装。
/// サーバ検証エラー（path 付き）を該当フィールドへ写像して表示する。
///
/// PC（md 以上）はキャンバス右の浮いたパネル（w-80）、スマホは下からのシートで同じ中身を出す。

import * as React from "react";
import { Trash2, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { FadeSlide } from "@/components/ui/motion-primitives";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import { useMediaQuery } from "@/hooks/use-media-query";
import type { Node as IrNode, Trigger } from "@/generated/workflow-ir";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { EditorContext } from "../workflow-editor";
import { nodeIcon } from "../icons";
import { mapErrors, paramField, serdeParamError } from "../validation";
import { CommonSection } from "./common";
import {
  AgentInvokeForm,
  LlmInvokeForm,
  RagSearchForm,
  SkillInvokeForm,
  StorageListForm,
  StorageReadForm,
  StorageWriteForm,
  WorkflowStartForm,
  type FormProps,
} from "./forms-basic";
import { BranchForm, JoinForm, MapForm, SwitchForm, WaitForm } from "./forms-control";
import {
  DataBulkUpsertForm,
  DataQueryForm,
  DataRecordCreateForm,
  DataRecordDeleteForm,
  DataRecordUpdateForm,
  DataTransitionForm,
} from "./forms-data";
import { HttpRequestForm, ScriptRunForm } from "./forms-external";
import { TriggerPanel } from "./trigger-panel";
import type { RefCandidate } from "./value-expr-input";

const FORMS: Record<string, React.ComponentType<FormProps>> = {
  "storage.read": StorageReadForm,
  "storage.write": StorageWriteForm,
  "storage.list": StorageListForm,
  "rag.search": RagSearchForm,
  "llm.invoke": LlmInvokeForm,
  "agent.invoke": AgentInvokeForm,
  "workflow.start": WorkflowStartForm,
  "control.branch": BranchForm,
  "control.switch": SwitchForm,
  "control.map": MapForm,
  "control.wait": WaitForm,
  "control.join": JoinForm,
  "http.request": HttpRequestForm,
  "script.run": ScriptRunForm,
  "skill.invoke": SkillInvokeForm,
  "data.query": DataQueryForm,
  "data.record.create": DataRecordCreateForm,
  "data.record.update": DataRecordUpdateForm,
  "data.record.delete": DataRecordDeleteForm,
  "data.transition": DataTransitionForm,
  "data.bulk_upsert": DataBulkUpsertForm,
};

/// 祖先ノード（`$from nodes.<id>.output` の参照候補）を逆辺 BFS で求める。
function ancestorsOf(ctx: EditorContext, nodeId: string): RefCandidate[] {
  const incoming = new Map<string, string[]>();
  for (const e of ctx.state.ir.edges) {
    const list = incoming.get(e.to) ?? [];
    list.push(e.from);
    incoming.set(e.to, list);
  }
  const seen = new Set<string>();
  const queue = [...(incoming.get(nodeId) ?? [])];
  while (queue.length > 0) {
    const id = queue.shift()!;
    if (seen.has(id)) continue;
    seen.add(id);
    queue.push(...(incoming.get(id) ?? []));
  }
  return ctx.state.ir.nodes
    .filter((n) => seen.has(n.id))
    .map((n) => ({
      id: n.id,
      label:
        n.label || NODE_CATALOG.find((c) => c.type === n.type)?.label_ja || n.id,
    }));
}

function NodePanel({ ctx, node, sheet }: { ctx: EditorContext; node: IrNode; sheet: boolean }) {
  const entry = NODE_CATALOG.find((c) => c.type === node.type);
  const Icon = nodeIcon(node.type);
  const Form = FORMS[node.type];
  const refCandidates = React.useMemo(
    () => ancestorsOf(ctx, node.id),
    [ctx, node.id],
  );
  const inMapRegion = node.parent != null;

  // path 付きエラー → 先頭フィールド名で写像（同一フィールド複数はまとめる）。
  const fieldErrors = React.useMemo(() => {
    const errors = mapErrors(ctx.state.serverErrors).byNode.get(node.id) ?? [];
    const map = new Map<string, string>();
    for (const e of errors) {
      const serde = paramField(e.path) ? null : serdeParamError(e.message);
      const field = paramField(e.path) ?? serde?.field;
      if (field && !map.has(field)) map.set(field, serde?.message ?? e.message);
    }
    return map;
  }, [ctx.state.serverErrors, node.id]);
  const nodeErrors = mapErrors(ctx.state.serverErrors).byNode.get(node.id) ?? [];
  // 欄に写せた誤り（path か serde の原文の項目名）は欄の下に出し、上の帯には欄の無いものだけを出す。
  const unmappedErrors = nodeErrors.filter((e) => !paramField(e.path) && !serdeParamError(e.message));
  const nodeWarnings = mapErrors(ctx.state.serverWarnings).byNode.get(node.id) ?? [];

  return (
    <>
      <div className={cn("flex items-start gap-2.5 border-b px-4 py-3", sheet && "pr-14")}>
        <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
          <Icon className="size-4" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <PanelTitle sheet={sheet}>{node.label || entry?.label_ja || node.type}</PanelTitle>
          <PanelDescription sheet={sheet}>{entry?.description_ja}</PanelDescription>
        </div>
        {/* シートは右上に共通の閉じるボタンがある（二重にしない）。 */}
        {sheet ? null : (
          <Button
            variant="ghost"
            size="icon"
            className="size-7"
            aria-label="パネルを閉じる"
            onClick={() => ctx.dispatch({ type: "select", selection: { kind: "none" } })}
          >
            <X className="size-4" aria-hidden />
          </Button>
        )}
      </div>
      <div className="flex-1 space-y-5 overflow-y-auto overscroll-y-contain px-4 py-4 scrollbar-subtle">
        {unmappedErrors.length > 0 ? (
          <ul className="space-y-1 rounded-md border border-destructive/40 bg-destructive/5 p-2 text-[11px] text-destructive">
            {unmappedErrors.map((e, i) => (
              <li key={i}>{e.message}</li>
            ))}
          </ul>
        ) : null}
        {nodeWarnings.length > 0 ? (
          <div
            role="note"
            className="space-y-1 rounded-md border p-2 text-[11px] leading-relaxed"
            style={{
              borderColor: "color-mix(in oklab, var(--season-autumn) 45%, transparent)",
              backgroundColor: "color-mix(in oklab, var(--season-autumn) 8%, transparent)",
            }}
            data-testid="node-warnings"
          >
            <p className="font-medium">保存はできますが、確認してください</p>
            <ul className="list-disc space-y-1 pl-4">
              {nodeWarnings.map((w, i) => (
                <li key={i}>{w.message}</li>
              ))}
            </ul>
          </div>
        ) : null}
        {Form ? (
          <Form
            node={node}
            dispatch={ctx.dispatch}
            refCandidates={refCandidates}
            inMapRegion={inMapRegion}
            fieldErrors={fieldErrors}
          />
        ) : (
          <p className="text-xs text-muted-foreground">このブロックに設定はありません</p>
        )}
        <div className="border-t pt-4">
          <CommonSection node={node} dispatch={ctx.dispatch} />
        </div>
        <Button
          variant="outline"
          size="sm"
          className="w-full text-destructive"
          onClick={() => ctx.dispatch({ type: "delete_nodes", ids: [node.id] })}
        >
          <Trash2 className="size-4" aria-hidden />
          このブロックを削除
        </Button>
      </div>
    </>
  );
}

export function ConfigPanel({ ctx }: { ctx: EditorContext }) {
  const { selection, ir } = ctx.state;
  const desktop = useMediaQuery("(min-width: 768px)");
  const sheet = !desktop;
  const close = () => ctx.dispatch({ type: "select", selection: { kind: "none" } });

  let label: string | null = null;
  let content: React.ReactNode = null;
  if (selection.kind === "node") {
    const node = ir.nodes.find((n) => n.id === selection.id);
    if (node) {
      label = "ブロックの設定";
      content = <NodePanel ctx={ctx} node={node} sheet={sheet} />;
    }
  } else if (selection.kind === "trigger") {
    label = "きっかけの設定";
    content = (
      <>
        <div className={cn("flex items-center justify-between shiki-dash-bottom px-4 py-3", sheet && "min-h-14 pr-14")}>
          <PanelTitle sheet={sheet}>きっかけ</PanelTitle>
          {sheet ? (
            <PanelDescription sheet className="sr-only">
              いつ・何をきっかけに動かすかを設定します。
            </PanelDescription>
          ) : (
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              aria-label="パネルを閉じる"
              onClick={close}
            >
              <X className="size-4" aria-hidden />
            </Button>
          )}
        </div>
        <div className="flex-1 overflow-y-auto overscroll-y-contain px-4 py-4 scrollbar-subtle">
          <TriggerPanel
            triggers={ir.triggers as Trigger[]}
            index={selection.index}
            onChange={(triggers) => ctx.dispatch({ type: "set_triggers", triggers })}
          />
        </div>
      </>
    );
  }

  if (sheet) {
    // スマホ: 選択中だけ下からのシートで開く。閉じる＝選択解除。
    return (
      <Sheet open={content !== null} onOpenChange={(open) => !open && close()}>
        <SheetContent
          side="bottom"
          aria-label={label ?? undefined}
          className="h-[85dvh]"
          // 開いた瞬間に先頭の入力へフォーカスしてキーボードを出さない（まず全体を見せる）。
          onOpenAutoFocus={(e) => e.preventDefault()}
        >
          {content}
        </SheetContent>
      </Sheet>
    );
  }
  if (!content || !label) return null;
  return <PanelShell label={label}>{content}</PanelShell>;
}

/// シートでは Radix の Title/Description（読み上げの見出し）、パネルでは素の見出し。
function PanelTitle({ sheet, children }: { sheet: boolean; children: React.ReactNode }) {
  if (sheet) return <SheetTitle className="truncate text-[15px] leading-snug">{children}</SheetTitle>;
  return <h2 className="truncate text-sm font-semibold">{children}</h2>;
}

function PanelDescription({
  sheet,
  className,
  children,
}: {
  sheet: boolean;
  className?: string;
  children: React.ReactNode;
}) {
  if (sheet) {
    return (
      <SheetDescription className={cn("truncate text-xs", className)}>{children}</SheetDescription>
    );
  }
  return <p className={cn("truncate text-xs text-muted-foreground", className)}>{children}</p>;
}

/// 設定パネルの外殻。キャンバスから少し浮かせた elevated カード（右から fade/slide 入場）。
function PanelShell({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="shrink-0 p-2 pl-0">
      <FadeSlide
        from="right"
        role="complementary"
        aria-label={label}
        className="flex h-full w-80 flex-col overflow-hidden rounded-xl border bg-card shadow-lg"
      >
        {children}
      </FadeSlide>
    </div>
  );
}
