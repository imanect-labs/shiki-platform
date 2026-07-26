"use client";

/// 能力ノードのフォーム（ファイル読む/保存/一覧・社内検索・AI に聞く・AI エージェント・別フロー起動）。
///
/// フィールドは codegen の typed 契約（NodeParamsByType）に対応する（効かない設定は出さない）。

import * as React from "react";

import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import type { NodeParamsByType } from "@/generated/workflow-catalog";
import type { Node as IrNode, ValueExpr } from "@/generated/workflow-ir";
import type { EditorAction } from "../ir-state";
import { Field, NumberInput } from "./common";
import { paramsOf, patchParams } from "./params";
import { ValueExprInput, type RefCandidate } from "./value-expr-input";

export type FormProps = {
  node: IrNode;
  dispatch: React.Dispatch<EditorAction>;
  refCandidates: RefCandidate[];
  inMapRegion: boolean;
  fieldErrors: Map<string, string>;
};

export function StorageReadForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["storage.read"]>(node);
  return (
    <ValueExprInput
      label="読むファイル"
      value={p.file as ValueExpr | undefined}
      onChange={(v) => patchParams(dispatch, node, { file: v })}
      refCandidates={refCandidates}
      inMapRegion={inMapRegion}
      placeholder="ファイル ID"
      error={fieldErrors.get("file")}
    />
  );
}

export function StorageWriteForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["storage.write"]>(node);
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="ファイル名"
        value={p.name as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { name: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="例: report.md"
        error={fieldErrors.get("name")}
      />
      <ValueExprInput
        label="内容"
        value={p.content as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { content: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        error={fieldErrors.get("content")}
      />
      <ValueExprInput
        label="保存先フォルダ（省略可）"
        value={p.folder as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { folder: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        placeholder="フォルダ ID（空欄でマイドライブ）"
        error={fieldErrors.get("folder")}
      />
    </div>
  );
}

export function StorageListForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["storage.list"]>(node);
  return (
    <ValueExprInput
      label="一覧するフォルダ（省略可）"
      value={p.folder as ValueExpr | undefined}
      onChange={(v) => patchParams(dispatch, node, { folder: v })}
      refCandidates={refCandidates}
      inMapRegion={inMapRegion}
      placeholder="フォルダ ID（空欄でマイドライブ）"
      error={fieldErrors.get("folder")}
    />
  );
}

export function RagSearchForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["rag.search"]>(node);
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="検索する内容"
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
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="AI への指示"
        value={p.prompt as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { prompt: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        placeholder="例: 次の文章を 3 行で要約してください"
        error={fieldErrors.get("prompt")}
      />
      <Field
        label="モデル"
        hint="管理者が登録したモデル名"
        error={fieldErrors.get("model")}
      >
        <Input
          value={(p.model as string | undefined) ?? ""}
          onChange={(e) =>
            patchParams(dispatch, node, { model: e.target.value || undefined })
          }
          placeholder="例: gpt-x-mini"
          className="h-8"
        />
      </Field>
      <ValueExprInput
        label="前提の指示（省略可）"
        value={p.system as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { system: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        placeholder="例: あなたは丁寧なアシスタントです"
      />
    </div>
  );
}

export function AgentInvokeForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["agent.invoke"]>(node);
  const allowlist = (p.egress_allowlist as string[] | undefined) ?? [];
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="エージェントへの依頼"
        value={p.instruction as ValueExpr | undefined}
        onChange={(v) => patchParams(dispatch, node, { instruction: v })}
        refCandidates={refCandidates}
        inMapRegion={inMapRegion}
        multiline
        placeholder="例: 添付の資料を確認して要点をまとめて"
        error={fieldErrors.get("instruction")}
      />
      <Field
        label="接続を許可する外部サイト（省略可）"
        hint="1 行 1 ドメイン。空欄なら外部接続なし（安全側）"
        error={fieldErrors.get("egress_allowlist")}
      >
        <Textarea
          value={allowlist.join("\n")}
          rows={2}
          placeholder={"api.example.com"}
          onChange={(e) =>
            patchParams(dispatch, node, {
              egress_allowlist: e.target.value
                .split("\n")
                .map((s) => s.trim())
                .filter(Boolean),
            })
          }
          className="font-mono text-xs"
        />
      </Field>
      <Field label="実行時間の上限（秒・省略可）">
        <NumberInput
          value={typeof p.max_duration_sec === "number" ? p.max_duration_sec : null}
          min={10}
          max={3600}
          onChange={(n) =>
            patchParams(dispatch, node, { max_duration_sec: n ?? undefined })
          }
        />
      </Field>
    </div>
  );
}

export function WorkflowStartForm({ node, dispatch, refCandidates, inMapRegion, fieldErrors }: FormProps) {
  const p = paramsOf<NodeParamsByType["workflow.start"]>(node);
  return (
    <div className="space-y-3">
      <ValueExprInput
        label="起動するワークフロー名"
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
            className="h-9 w-full rounded-md border border-input bg-background px-2 text-sm"
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
