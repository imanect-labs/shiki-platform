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
import { WARNING_CODES } from "@/lib/workflow-api";
import { inputFieldsOf } from "@/lib/workflow-input-schema";
import type { Node as IrNode, Trigger } from "@/generated/workflow-ir";
import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { EditorContext } from "../workflow-editor";
import { nodeIcon } from "../icons";
import { defaultParams } from "../ir-state";
import { edgePortsOf } from "../ports";
import { mapErrors, nodeErrorView, paramField } from "../validation";
import { CommonSection } from "./common";
import { HumanApprovalForm } from "./forms-approval";
import { WorkflowReplyForm } from "./forms-reply";
import { AgentInvokeForm } from "./forms-agent";
import { AiDecideForm } from "./forms-ai-decide";
import {
  LlmInvokeForm,
  RagSearchForm,
  SkillInvokeForm,
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
import {
  CsvPatchForm,
  CsvQueryForm,
  CsvWriteForm,
  StorageListForm,
  StorageReadForm,
  StorageWriteForm,
} from "./forms-files";
import { patchParams } from "./params";
import type { InputRefUsage } from "./input-schema-editor";
import { TriggerPanel } from "./trigger-panel";
import { InputFieldsContext, TriggerSourcesContext, type RefCandidate } from "./value-expr-input";
import { inputRefKeys } from "./value-refs";

const FORMS: Record<string, React.ComponentType<FormProps>> = {
  "storage.read": StorageReadForm,
  "storage.write": StorageWriteForm,
  "storage.list": StorageListForm,
  "rag.search": RagSearchForm,
  "llm.invoke": LlmInvokeForm,
  "agent.invoke": AgentInvokeForm,
  "ai.decide": AiDecideForm,
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
  "csv.query": CsvQueryForm,
  "csv.patch": CsvPatchForm,
  "csv.write": CsvWriteForm,
  "human.approval": HumanApprovalForm,
  "workflow.reply": WorkflowReplyForm,
};

/// 承認を待つブロックを置いたときに勧める実行の期限（14 日・承認は数日かかる・GAP-08）。
const APPROVAL_RUN_TIMEOUT_SEC = 14 * 24 * 3600;

/// 承認を待つ間に実行の期限が来ると、承認は時間切れの出口へ流れずに取り消される。期限を延ばす提案を出す。
function RunTimeoutSuggestion({ ctx }: { ctx: EditorContext }) {
  const policies = ctx.state.ir.policies;
  const current = policies?.run_timeout_sec ?? 259_200;
  if (current >= APPROVAL_RUN_TIMEOUT_SEC) return null;
  const days = Math.round((current / 86_400) * 10) / 10;
  return (
    <div
      role="note"
      className="space-y-2 rounded-md border p-2.5 text-[11px] leading-relaxed"
      style={{
        borderColor: "color-mix(in oklab, var(--season-autumn) 45%, transparent)",
        backgroundColor: "color-mix(in oklab, var(--season-autumn) 8%, transparent)",
      }}
      data-testid="approval-run-timeout"
    >
      <p>
        実行の期限はいま {days} 日です。承認を待つ間に期限が来ると、承認は取り消され、時間切れの出口へも進みません。
      </p>
      <Button
        variant="outline"
        size="sm"
        className="h-7 text-xs"
        onClick={() =>
          ctx.dispatch({
            type: "update_meta",
            patch: { policies: { ...policies, run_timeout_sec: APPROVAL_RUN_TIMEOUT_SEC } },
          })
        }
      >
        実行の期限を 14 日にする
      </Button>
    </div>
  );
}

/// シートの本文の下端の余白（浮いているボタンの高さ 44px ＋ 余裕）。
const SHEET_BOTTOM_SPACE = "pb-20";

/// 欄の下に出す保存時の警告のコード（注意の帯には出さない・生成した定数で引く）。書いた思考の深さが組織の設定に
/// 従うので使われない（D-75）。必須の文字の欄の空（`REQUIRED_TEXT_EMPTY`）は「入力してください」として別に扱う。
const FIELD_WARNING_CODES: ReadonlySet<string> = new Set([WARNING_CODES.EFFORT_IGNORED]);

/// 置いたときに空の文字で入れてある必須の欄（`defaultParams` の `""`）のうち、まだ空のもの。
/// 保存は通るが中身の無い指示で動いてしまうので、欄の下で入力を促す（BUG-15）。
function blankRequired(node: IrNode): string[] {
  const defaults = defaultParams(node.type);
  if (!defaults || typeof defaults !== "object") return [];
  const params = (node.params ?? {}) as Record<string, unknown>;
  return Object.entries(defaults as Record<string, unknown>)
    .filter(([key, v]) => {
      const cur = params[key];
      return v === "" && (cur === undefined || (typeof cur === "string" && cur.trim() === ""));
    })
    .map(([key]) => key);
}

/// 祖先ノード（`$from nodes.<id>.output` の参照候補）を逆辺 BFS で求める。ノード種は出力の形から
/// 項目の候補を出すのに使う。
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
      type: n.type,
      params: n.params,
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
  // 入力項目（「差し込む」・参照先の候補）。
  const inputFields = React.useMemo(() => inputFieldsOf(ctx.state.ir.input_schema), [ctx.state.ir.input_schema]);
  // きっかけの内容の項目（イベントトリガの source ごと・D-47）。
  const triggerSources = React.useMemo(
    () =>
      (ctx.state.ir.triggers ?? []).flatMap((t) => (t.kind === "event" ? [t.source as string] : [])),
    [ctx.state.ir.triggers],
  );

  // 欄に写せる誤り（path か文言の項目名）は欄の下に出し、上の帯には欄の無いものだけを出す。
  const nodeErrors = React.useMemo(
    () => mapErrors(ctx.state.serverErrors).byNode.get(node.id) ?? [],
    [ctx.state.serverErrors, node.id],
  );
  const errorView = React.useMemo(() => nodeErrorView(nodeErrors), [nodeErrors]);
  // 誤りの添字を検証した時点の値で引くための材料（フォームが今の並びに当てて取り違えない）。
  const serverCheck = React.useMemo(
    () => ({ errors: nodeErrors, node: ctx.state.serverErrorsIr?.nodes.find((n) => n.id === node.id) ?? null }),
    [nodeErrors, ctx.state.serverErrorsIr, node.id],
  );
  const allWarnings = React.useMemo(
    () => mapErrors(ctx.state.serverWarnings).byNode.get(node.id) ?? [],
    [ctx.state.serverWarnings, node.id],
  );
  // 必須の文字の欄が空の警告（D-43）は注意の帯ではなく、その欄の下に出す（保存はできるが有効化・実行で止まる）。
  const blankFields = React.useMemo(
    () =>
      allWarnings.flatMap((w) => {
        const field = w.code === WARNING_CODES.REQUIRED_TEXT_EMPTY ? paramField(w.path) : null;
        return field ? [field] : [];
      }),
    [allWarnings],
  );
  const fieldErrors = React.useMemo(() => {
    const map = new Map(errorView.fields);
    for (const key of [...blankRequired(node), ...blankFields]) if (!map.has(key)) map.set(key, "入力してください");
    return map;
  }, [errorView, node, blankFields]);
  // 欄に写す警告（書いた思考の深さが組織の設定に従うので使われない・D-75 など）は、その欄の下に出す。
  const fieldWarnings = React.useMemo(() => {
    const map = new Map<string, string>();
    for (const w of allWarnings) {
      const field = FIELD_WARNING_CODES.has(w.code) ? paramField(w.path) : null;
      if (field && !map.has(field)) map.set(field, w.message);
    }
    return map;
  }, [allWarnings]);
  const params = (node.params ?? {}) as Record<string, unknown>;
  const nodeWarnings = allWarnings.filter(
    (w) =>
      !((w.code === WARNING_CODES.REQUIRED_TEXT_EMPTY || FIELD_WARNING_CODES.has(w.code)) && paramField(w.path)),
  );
  // このノードから出るエッジの出口（中身が同じなら同じ配列を渡す）。
  const edgePortsKey = JSON.stringify(edgePortsOf(node.id, ctx.state.ir.edges));
  const edgePorts = React.useMemo(() => JSON.parse(edgePortsKey) as string[], [edgePortsKey]);

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
      {/* シートでは下端に余白を取る（画面の左下に浮くボタン・ブラウザの下端の操作部に最後の欄が隠れないよう、
          末尾まで送ったときに欄を持ち上げる・BUG-40）。 */}
      <div className={cn("flex-1 space-y-5 overflow-y-auto overscroll-y-contain px-4 py-4 scrollbar-subtle", sheet && SHEET_BOTTOM_SPACE)}>
        {errorView.banner.length > 0 ? (
          <ul className="space-y-1 rounded-md border border-destructive/40 bg-destructive/5 p-2 text-[11px] text-destructive">
            {errorView.banner.map((e, i) => (
              <li key={i} className="flex items-start justify-between gap-2">
                <span className="min-w-0">{e.message}</span>
                {e.removable && e.removable in params ? (
                  <button
                    type="button"
                    className="shrink-0 font-medium underline underline-offset-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    onClick={() => patchParams(ctx.dispatch, node, { [e.removable!]: undefined })}
                  >
                    取り除く
                  </button>
                ) : e.removableAttr === "cache" && node.cache ? (
                  <button
                    type="button"
                    className="shrink-0 font-medium underline underline-offset-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                    onClick={() =>
                      ctx.dispatch({ type: "update_node", id: node.id, patch: { cache: null } as Partial<IrNode> })
                    }
                  >
                    使い回しをやめる
                  </button>
                ) : null}
              </li>
            ))}
          </ul>
        ) : null}
        {nodeWarnings.length > 0 ? (
          <div
            role="note"
            // 長いキー・path（/answers/<キー>/… など）が枠からはみ出さないよう、どこでも折り返す。
            className="space-y-1 rounded-md border p-2 text-[11px] leading-relaxed [overflow-wrap:anywhere]"
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
        {node.type === "human.approval" ? <RunTimeoutSuggestion ctx={ctx} /> : null}
        {Form ? (
          // ノードを選び直したらフォームの手元の状態（入力方法の切り替えなど）を持ち越さない。
          <InputFieldsContext.Provider value={inputFields}>
            <TriggerSourcesContext.Provider value={triggerSources}>
              <Form
                key={node.id}
                node={node}
                dispatch={ctx.dispatch}
                refCandidates={refCandidates}
                inMapRegion={inMapRegion}
                fieldErrors={fieldErrors}
                fieldWarnings={fieldWarnings}
                serverCheck={serverCheck}
                serverPorts={ctx.state.serverPorts[node.id]}
                serverPortsFresh={ctx.state.serverPortsParams[node.id] === node.params}
                edgePorts={edgePorts}
              />
            </TriggerSourcesContext.Provider>
          </InputFieldsContext.Provider>
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

/// ブロックが参照している入力項目のキー（`$from: "input"` の path の先頭の段）と、そのブロックの表示名。
function inputUsagesOf(nodes: IrNode[]): InputRefUsage[] {
  const byKey = new Map<string, string[]>();
  for (const n of nodes) {
    const label = n.label || NODE_CATALOG.find((c) => c.type === n.type)?.label_ja || n.id;
    for (const key of inputRefKeys(n.params)) byKey.set(key, [...(byKey.get(key) ?? []), label]);
  }
  return [...byKey].map(([key, blocks]) => ({ key, blocks }));
}

export function ConfigPanel({ ctx }: { ctx: EditorContext }) {
  const { selection, ir } = ctx.state;
  const inputUsages = React.useMemo(() => inputUsagesOf(ir.nodes), [ir.nodes]);
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
        <div className={cn("flex-1 overflow-y-auto overscroll-y-contain px-4 py-4 scrollbar-subtle", sheet && SHEET_BOTTOM_SPACE)}>
          <TriggerPanel
            triggers={ir.triggers as Trigger[]}
            index={selection.index}
            onChange={(triggers) => ctx.dispatch({ type: "set_triggers", triggers })}
            inputSchema={ir.input_schema}
            onInputSchemaChange={(input_schema, inputRenames) =>
              ctx.dispatch({ type: "update_meta", patch: { input_schema }, inputRenames })
            }
            inputUsages={inputUsages}
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
