"use client";

/// 右サイドバー: 選択中のノード/トリガの設定パネル（Task 10.12）。
///
/// フォームは codegen の typed 契約（NodeParamsByType）で型検査された per-node 実装。
/// サーバ検証エラー（path 付き）を該当フィールドへ写像して表示する。

import * as React from "react";
import { Trash2, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { FadeSlide } from "@/components/ui/motion-primitives";
import type { Node as IrNode, Trigger } from "@/generated/workflow-ir";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { EditorContext } from "../workflow-editor";
import { nodeIcon } from "../icons";
import { mapErrors, paramField } from "../validation";
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

function NodePanel({ ctx, node }: { ctx: EditorContext; node: IrNode }) {
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
      const field = paramField(e.path);
      if (field && !map.has(field)) map.set(field, e.message);
    }
    return map;
  }, [ctx.state.serverErrors, node.id]);
  const nodeErrors = mapErrors(ctx.state.serverErrors).byNode.get(node.id) ?? [];
  const unmappedErrors = nodeErrors.filter((e) => !paramField(e.path));

  return (
    <>
      <div className="flex items-start gap-2.5 border-b px-4 py-3">
        <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
          <Icon className="size-4" aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <h2 className="truncate text-sm font-semibold">
            {node.label || entry?.label_ja || node.type}
          </h2>
          <p className="truncate text-xs text-muted-foreground">{entry?.description_ja}</p>
        </div>
        <Button
          variant="ghost"
          size="icon"
          className="size-7"
          aria-label="パネルを閉じる"
          onClick={() => ctx.dispatch({ type: "select", selection: { kind: "none" } })}
        >
          <X className="size-4" aria-hidden />
        </Button>
      </div>
      <div className="flex-1 space-y-5 overflow-y-auto px-4 py-4 scrollbar-subtle">
        {unmappedErrors.length > 0 ? (
          <ul className="space-y-1 rounded-md border border-destructive/40 bg-destructive/5 p-2 text-[11px] text-destructive">
            {unmappedErrors.map((e, i) => (
              <li key={i}>{e.message}</li>
            ))}
          </ul>
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

  if (selection.kind === "node") {
    const node = ir.nodes.find((n) => n.id === selection.id);
    if (node) {
      return (
        <PanelShell label="ブロックの設定">
          <NodePanel ctx={ctx} node={node} />
        </PanelShell>
      );
    }
  }
  if (selection.kind === "trigger") {
    return (
      <PanelShell label="きっかけの設定">
        <div className="flex items-center justify-between shiki-dash-bottom px-4 py-3">
          <h2 className="text-sm font-semibold">きっかけ</h2>
          <Button
            variant="ghost"
            size="icon"
            className="size-7"
            aria-label="パネルを閉じる"
            onClick={() => ctx.dispatch({ type: "select", selection: { kind: "none" } })}
          >
            <X className="size-4" aria-hidden />
          </Button>
        </div>
        <div className="flex-1 overflow-y-auto px-4 py-4 scrollbar-subtle">
          <TriggerPanel
            triggers={ir.triggers as Trigger[]}
            index={selection.index}
            onChange={(triggers) => ctx.dispatch({ type: "set_triggers", triggers })}
          />
        </div>
      </PanelShell>
    );
  }
  return null;
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
