"use client";

/// AI のブロックに入れた内容の送り先の提供元（D-99）。自動実行のダイアログで、同意の前に言う。
///
/// 提供元の表示名と国外処理の印は `GET /llm/models` の一覧（llm-gateway のカタログとプロバイダの設定が単一定義）
/// から引く（[`providerOf`]）。

import { AbroadBadge } from "@/components/llm/model-list";
import { useLlmModelsState } from "@/lib/model-choice";
import { providerOf, type ModelProvider } from "@/lib/model-provider";
import type { Node as IrNode } from "@/generated/workflow-ir";

/// モデルを選ぶ AI のブロックの種類と、選べるモデルの種類（`model` の欄があるブロック）。
const MODEL_NODES: Record<string, "chat" | "decision"> = {
  "llm.invoke": "chat",
  "agent.invoke": "chat",
  "ai.decide": "decision",
};

/// ワークフローに AI のブロックがあるか（[`AiProvidersNote`] を出すか）。
export function hasAiNodes(nodes: IrNode[]): boolean {
  return nodes.some((n) => n.type in MODEL_NODES || n.type === "skill.invoke");
}

/// AI のブロックに入れた内容の送り先（D-99）。ブロックのモデル（選ばなければ組織の既定）ごとに提供元を引き、
/// 重ねずに並べる。スキルのモデルはスキルが決めるので、提供元を言わずにその旨だけを言う。
export function AiProvidersNote({ nodes }: { nodes: IrNode[] }) {
  const chat = useLlmModelsState("workflow", "chat");
  const decision = useLlmModelsState("workflow", "decision");
  const providers = new Map<string, ModelProvider>();
  let unknown = false;
  for (const n of nodes) {
    const modality = MODEL_NODES[n.type];
    if (!modality) continue;
    const state = modality === "chat" ? chat : decision;
    const model = (n.params as { model?: unknown } | undefined)?.model;
    const provider =
      state.status === "ready" ? providerOf(state.data, typeof model === "string" ? model : undefined) : null;
    if (provider) providers.set(provider.label, provider);
    else unknown = true;
  }
  const skills = nodes.filter((n) => n.type === "skill.invoke");
  const loading = chat.status === "loading" || decision.status === "loading";
  return (
    <div className="space-y-1 text-xs leading-relaxed text-muted-foreground" data-testid="enable-ai-providers">
      {providers.size > 0 ? (
        <p className="flex flex-wrap items-center gap-x-1 gap-y-1">
          <span>AI のブロックに入れた内容は、</span>
          {[...providers.values()].map((p, i) => (
            <span key={p.label} className="inline-flex items-center gap-1">
              <span className="font-medium text-foreground">{p.label}</span>
              {p.abroad ? <AbroadBadge /> : null}
              {i < providers.size - 1 ? <span>、</span> : null}
            </span>
          ))}
          <span>に送られます。</span>
        </p>
      ) : null}
      {unknown && !loading ? (
        <p>一部の AI のブロックは、送り先の提供元を読み込めませんでした。組織の AI の設定で決めたモデルの提供元に送られます。</p>
      ) : null}
      {skills.length > 0 ? (
        <p>
          {skills.map((s) => `「${s.label || s.id}」`).join("")}
          は、スキルで決めたモデルの提供元に送ります。
        </p>
      ) : null}
    </div>
  );
}
