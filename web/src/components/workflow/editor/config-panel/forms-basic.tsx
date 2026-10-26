"use client";

/// 能力ノードのフォーム（社内検索・AI に聞く・別フロー起動・スキル）。
/// ファイル（storage.* / csv.*）のフォームは forms-files.tsx、AI エージェントは forms-agent.tsx にある。
///
/// フィールドは codegen の typed 契約（NodeParamsByType）に対応する（効かない設定は出さない）。

import * as React from "react";

import { Input } from "@/components/ui/input";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type { Node as IrNode, ValueExpr } from "@/generated/workflow-ir";
import type { EditorAction } from "../ir-state";
import { Field, NumberInput } from "./common";
import { ModelField } from "./model-field";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput, type RefCandidate } from "./value-expr-input";

export type FormProps = {
  node: IrNode;
  dispatch: React.Dispatch<EditorAction>;
  refCandidates: RefCandidate[];
  inMapRegion: boolean;
  fieldErrors: Map<string, string>;
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

export function LlmInvokeForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
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
    </div>
  );
}

export function WorkflowStartForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["workflow.start"]>(node);
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="起動するワークフロー名"
        text
        value={p.name as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { name: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="例: expense-approval"
        error={fieldErrors.get("name")}
      />
      <ValueExprInput
        label="渡す入力（省略可）"
        value={p.input as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { input: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
      />
      <p className="text-[11px] text-muted-foreground">
        起動した先の結果は待ちません（起動のみ・高々 1 回保証）
      </p>
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
          <select
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
          </select>
        ) : (
          <Input
            value={current}
            placeholder="skill:<name>@<version>"
            onChange={(e) => patchParams(dispatch, node, { skill: e.target.value })}
          />
        )}
      </Field>
      <ValueExprInput
        label="渡す入力（省略可）"
        value={p.input as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { input: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
      />
    </div>
  );
}
