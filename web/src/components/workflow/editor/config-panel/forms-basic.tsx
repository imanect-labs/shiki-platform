"use client";

/// 能力ノードのフォーム（社内検索・AI に聞く・別のワークフローを実行・スキル）。
/// ファイル（storage.* / csv.*）のフォームは forms-files.tsx、AI エージェントは forms-agent.tsx にある。
///
/// フィールドは codegen の typed 契約（NodeParamsByType）に対応する（効かない設定は出さない）。

import * as React from "react";

import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type { LlmEffort, Node as IrNode, ValidationError, ValueExpr } from "@/generated/workflow-ir";
import { Select, SelectContent, SelectItem, SelectValue } from "@/components/ui/select";
import { listWorkflows, type WorkflowSummary } from "@/lib/workflow-api";
import type { EditorAction } from "../ir-state";
import { Field, FieldInput, FieldNativeSelect, FieldSelectTrigger, NumberInput } from "./common";
import { EffortField } from "./effort-field";
import { ModelField } from "./model-field";
import { OutputSchemaEditor } from "./output-schema-editor";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput, type RefCandidate } from "./value-expr-input";
import { ANY_SHAPES, OBJECT_SHAPE } from "./value-shape";

/// このノードへのサーバ検証の誤り（path 付き）と、その検証の対象だったこのノード（無ければ `null`）。
/// 誤りの path の添字はこのノードの値で引く（検証の後に並びが変わっても、今の値に取り違えない）。
export type ServerCheck = { errors: ValidationError[]; node: IrNode | null };

export type FormProps = {
  node: IrNode;
  dispatch: React.Dispatch<EditorAction>;
  refCandidates: RefCandidate[];
  inMapRegion: boolean;
  fieldErrors: Map<string, string>;
  /// 欄の下に出す保存時の警告（params の項目名 → 文言・欄に写すコードだけ。index.tsx の FIELD_WARNING_CODES）。
  fieldWarnings?: Map<string, string>;
  serverCheck: ServerCheck;
  /// サーバが導いたこのノードの出口（判定のノード・検証 API の `node_ports`。まだ無ければ undefined）。
  serverPorts?: readonly string[];
  /// `serverPorts` が今の設定から導いたものか（違えば次の検証の応答までの古い出口）。
  serverPortsFresh?: boolean;
  /// このノードから出るエッジの出口（重なりなし・判定の選択肢のキーの付け替えで、どの出口に線があるかを見る）。
  edgePorts?: readonly string[];
  /// 繰り返しの中（入れ子の中を含む）のブロックの種類（繰り返しのときだけ・利用量の注記に使う）。
  insideTypes?: readonly string[];
};

export function RagSearchForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["rag.search"]>(node);
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="検索する内容"
        text
        value={p.query as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { query: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="例: 経費精算のルール"
        error={fieldErrors.get("query")}
      />
      <Field label="取得件数（省略可）" hint="空欄で標準">
        <NumberInput
          value={typeof p.top_k === "number" ? p.top_k : null}
          min={1}
          max={50}
          onChange={(n) => patchParams(dispatch, node, { top_k: n ?? undefined })}
        />
      </Field>
    </div>
  );
}

export function LlmInvokeForm({
  node,
  dispatch,
  refCandidates,
  inMapRegion,
  fieldErrors,
  fieldWarnings,
  serverCheck,
}: FormProps) {
  const p = paramsOf<NodeParamsByType["llm.invoke"]>(node);
  // 出力の上限は数で入れる。前の結果から決める定義（AI が作ったものなど）はそのまま値の欄で見せる。
  const maxTokensLiteral = p.max_tokens === undefined || typeof p.max_tokens === "number";
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="AI への指示"
        text
        value={p.prompt as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { prompt: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        placeholder="例: 次の文章を 3 行で要約してください"
        error={fieldErrors.get("prompt")}
      />
      <ModelField
        value={typeof p.model === "string" ? p.model : undefined}
        onChange={(model) => patchParams(dispatch, node, { model })}
        error={fieldErrors.get("model")}
      />
      <EffortField
        value={p.effort as LlmEffort | undefined}
        model={typeof p.model === "string" ? p.model : undefined}
        onChange={(effort) => patchParams(dispatch, node, { effort })}
        error={fieldErrors.get("effort")}
        warning={fieldWarnings?.get("effort")}
      />
      <ValueExprInput
        label="前提の指示（省略可）"
        text
        value={p.system as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { system: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        placeholder="例: あなたは丁寧なアシスタントです"
        error={fieldErrors.get("system")}
      />
      {maxTokensLiteral ? (
        <Field
          label="答えの長さの上限（トークン・省略可）"
          hint="空欄で標準"
          error={fieldErrors.get("max_tokens")}
        >
          <NumberInput
            value={typeof p.max_tokens === "number" ? p.max_tokens : null}
            min={1}
            placeholder="例: 1000"
            error={Boolean(fieldErrors.get("max_tokens"))}
            onChange={(n) =>
              patchParams(dispatch, node, { max_tokens: n === null ? undefined : Math.max(1, Math.round(n)) })
            }
          />
        </Field>
      ) : (
        <ValueExprInput
          label="答えの長さの上限（トークン・省略可）"
          value={p.max_tokens as ValueExpr | undefined}
          onChange={(v) => patchParams(dispatch, node, { max_tokens: v })}
          refCandidates={refCandidates}
          inMapRegion={inMapRegion}
          error={fieldErrors.get("max_tokens")}
        />
      )}
      <OutputSchemaEditor
        schema={p.output_schema}
        onChange={(output_schema) => patchParams(dispatch, node, { output_schema })}
        serverErrors={serverCheck.errors}
        checkedNode={serverCheck.node}
      />
    </div>
  );
}

/// 実行するワークフローの一覧の読み込み（見られるワークフローの最近の 100 件・GET /workflows）。
type WorkflowList = { state: "loading" } | { state: "error" } | { state: "ready"; items: WorkflowSummary[] };

function useWorkflowList(): [WorkflowList, () => void] {
  const [list, setList] = React.useState<WorkflowList>({ state: "loading" });
  const [attempt, setAttempt] = React.useState(0);
  React.useEffect(() => {
    let active = true;
    listWorkflows({ limit: 100 })
      .then((items) => active && setList({ state: "ready", items }))
      .catch(() => active && setList({ state: "error" }));
    return () => {
      active = false;
    };
  }, [attempt]);
  const reload = React.useCallback(() => {
    setList({ state: "loading" });
    setAttempt((n) => n + 1);
  }, []);
  return [list, reload];
}

/// 一覧の 1 行の名前（画面に出る名前。同じ表示名が 2 つあるときだけ内部の名前を添えて見分ける）。
function workflowOptionLabel(w: WorkflowSummary, items: WorkflowSummary[]): string {
  const shown = w.displayName?.trim() || w.name;
  const twins = items.filter((x) => (x.displayName?.trim() || x.name) === shown).length;
  return twins > 1 ? `${shown}（${w.name}）` : shown;
}

/// workflow.start — 別のワークフローを実行（BUG-107）。相手はワークフローの一覧から表示名で選び、保存するのは
/// 今と同じ内部の名前（IR の `name`）。入力や結果から名前を渡す組み方も残す。相手が無い・見えないことは保存時に
/// サーバーが確かめる（V4 の `ir.unknown_workflow`・欄の下に出る）。
export function WorkflowStartForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["workflow.start"]>(node);
  const value = p.name as ValueExpr | undefined;
  const literal = typeof value === "string" ? value : null;
  const [mode, setMode] = React.useState<"pick" | "expr">(value !== undefined && literal === null ? "expr" : "pick");
  const [list, reload] = useWorkflowList();
  const error = fieldErrors.get("name");
  const items = list.state === "ready" ? list.items : [];
  const listed = literal !== null && items.some((w) => w.name === literal);
  return (
    <div className="space-y-3">
      <div className="space-y-1.5">
        {mode === "pick" ? (
          <Field
            label="実行するワークフロー"
            hint="同じ組織のワークフローから選びます。選んだワークフローの、保存されている最新の内容を実行します。"
            error={error}
          >
            <Select
              value={literal ?? ""}
              disabled={list.state === "loading"}
              onValueChange={(v) => patchParams(dispatch, node, { name: v })}
            >
              <FieldSelectTrigger className="h-8 text-[13px]" data-testid="workflow-start-select">
                <SelectValue placeholder={list.state === "loading" ? "読み込み中…" : "ワークフローを選んでください"} />
              </FieldSelectTrigger>
              <SelectContent>
                {items.map((w) => (
                  <SelectItem key={w.id} value={w.name}>
                    {workflowOptionLabel(w, items)}
                  </SelectItem>
                ))}
                {/* 一覧（最近の 100 件）に無い保存済みの名前も選んだまま見せる（見えるかは保存時にサーバーが確かめる）。 */}
                {literal && !listed ? <SelectItem value={literal}>{literal}</SelectItem> : null}
              </SelectContent>
            </Select>
          </Field>
        ) : (
          <ValueExprInput
            label="実行するワークフロー"
            text
            value={value}
            onChange={(v) => patchParams(dispatch, node, { name: v })}
            refCandidates={refCandidates}
            inMapRegion={inMapRegion}
            error={error}
          />
        )}
        {mode === "pick" && list.state === "error" ? (
          <p role="alert" className="flex flex-wrap items-center gap-x-2 text-[11px] text-destructive">
            ワークフローの一覧を読み込めませんでした。
            <button type="button" className="text-primary underline-offset-2 hover:underline" onClick={reload}>
              もう一度読み込む
            </button>
          </p>
        ) : null}
        {mode === "pick" && list.state === "ready" && items.length === 0 ? (
          <p className="text-[11px] text-muted-foreground">
            選べるワークフローがありません。先に実行するワークフローを作るか、作成した人に共有を頼んでください。
          </p>
        ) : null}
        <button
          type="button"
          className="text-[11px] text-primary underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          onClick={() => setMode(mode === "pick" ? "expr" : "pick")}
        >
          {mode === "pick" ? "入力や結果から選ぶ" : "一覧から選ぶ"}
        </button>
      </div>
      <ValueExprInput
        label="渡す入力項目（省略可）"
        shapes={OBJECT_SHAPE}
        value={p.input as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { input: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        hint="実行するワークフローの入力項目に合う値を渡します。合わないと、このブロックは失敗します。"
      />
      <div className="space-y-1 text-[11px] leading-relaxed text-muted-foreground" data-testid="workflow-start-notes">
        {/* 権限と内容は run/launcher.rs の start_child（親の実行主体の権限で、相手の最新の保存を動かす・D-91）。 */}
        <p>
          実行したワークフローは、このワークフローと同じ権限で動きます（手動では実行した人、自動実行ではこのワークフローに使えるようにしたテーブルやフォルダー）。相手のワークフローを編集できる人が保存した最新の内容が、そのまま動きます。
        </p>
        <p>
          別のワークフローを実行し、その結果は待たずに次へ進みます。このブロックの自動のやり直しでは、別のワークフローはふつう 1 回だけ実行されます。
        </p>
        <p>実行したワークフローの「結果を画面に返す」は、どの画面にも出ません。結果は、そのワークフローの実行履歴で確かめます。</p>
      </div>
    </div>
  );
}

/// skill.invoke — インストール済みスキルの実行（#344）。
/// 参照はリテラル `skill:<name>@<version>`（保存時にインストール集合へ照合＝V4）。
export function SkillInvokeForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["skill.invoke"]>(node);
  const [installed, setInstalled] = React.useState<
    { name: string; registryVersion: string }[] | null
  >(null);
  React.useEffect(() => {
    void import("@/lib/skill-registry-api").then((m) =>
      m
        .listSkillInstallations()
        .then((list) =>
          setInstalled(list.map((i) => ({ name: i.name, registryVersion: i.registryVersion }))),
        )
        .catch(() => setInstalled([])),
    );
  }, []);
  const current = (p.skill as string | undefined) ?? "";
  return (
    <div className="space-y-3">
      <Field
        label="実行するスキル"
        hint="インストール済みスキルから選ぶ（スキルページのストアで追加できる）"
        error={fieldErrors.get("skill")}
      >
        {installed && installed.length > 0 ? (
          <FieldNativeSelect
            className="h-9 w-full rounded-md border border-input bg-background px-2 text-sm pointer-coarse:text-base"
            value={current}
            onChange={(e) => patchParams(dispatch, node, { skill: e.target.value })}
          >
            <option value="">選択してください</option>
            {installed.map((i) => {
              const v = `skill:${i.name}@${i.registryVersion}`;
              return (
                <option key={v} value={v}>
                  {i.name}（v{i.registryVersion}）
                </option>
              );
            })}
            {current && !installed.some((i) => `skill:${i.name}@${i.registryVersion}` === current) ? (
              <option value={current}>{current}（未インストール）</option>
            ) : null}
          </FieldNativeSelect>
        ) : (
          <FieldInput
            value={current}
            placeholder="skill:<name>@<version>"
            onChange={(e) => patchParams(dispatch, node, { skill: e.target.value })}
          />
        )}
      </Field>
      <ValueExprInput
        label="渡す入力（省略可）"
        shapes={ANY_SHAPES}
        value={p.input as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { input: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
      />
    </div>
  );
}
