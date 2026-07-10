/// params 編集の共通ヘルパ（typed 契約 NodeParamsByType へ patch を書き戻す）。

import type { Node as IrNode } from "@/generated/workflow-ir";
import type { EditorAction } from "../ir-state";

export function paramsOf<T>(node: IrNode): Partial<T> {
  return (node.params ?? {}) as Partial<T>;
}

export function patchParams(
  dispatch: React.Dispatch<EditorAction>,
  node: IrNode,
  patch: Record<string, unknown>,
): void {
  const current = (node.params ?? {}) as Record<string, unknown>;
  const next: Record<string, unknown> = { ...current, ...patch };
  // undefined のキーは削除（deny_unknown ではなく「省略」に写す）。
  for (const key of Object.keys(next)) {
    if (next[key] === undefined) delete next[key];
  }
  dispatch({
    type: "update_node",
    id: node.id,
    patch: { params: next } as Partial<IrNode>,
  });
}
