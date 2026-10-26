/// ノードの出力ポート（描画を持たない・キャンバスのハンドル・追加先・エディタの reducer が同じ規則で使う）。

import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { Node as IrNode } from "@/generated/workflow-ir";

/// ノードの出力ポート（カタログ＋設定から導出・ir.md §5.2）。
export function outputPorts(irNode: IrNode): string[] {
  const entry = NODE_CATALOG.find((c) => c.type === irNode.type);
  let ports: string[];
  if (irNode.type === "control.switch") {
    const cases = (irNode.params as { cases?: { port: string }[] } | null)?.cases ?? [];
    // どの case にも一致しない値は実行エンジンが `default` へ流す（control/mod.rs）。
    // フォールバック経路を配線できるよう常に default ハンドルを出す（`out` は発しない）。
    ports = [...new Set([...cases.map((c) => c.port), "default"])];
  } else if (irNode.type === "human.approval") {
    // 却下・差し戻しは指定したときだけ。取り下げは常に・時間切れは continue のとき（vocab::approval_ports と同じ規則）。
    const p = (irNode.params ?? {}) as { reject?: unknown; return?: unknown; on_timeout?: string };
    ports = ["approved"];
    if (p.reject != null) ports.push("rejected");
    if (p.return != null) ports.push("returned");
    ports.push("withdrawn");
    if (p.on_timeout === "continue") ports.push("timeout");
    if (irNode.on_error === "continue") ports.push("error");
    return ports;
  } else {
    ports = [...(entry?.output_ports ?? ["out"])];
  }
  if (
    irNode.type === "control.wait" &&
    (irNode.params as { on_timeout?: string } | null)?.on_timeout === "continue"
  ) {
    ports.push("timeout");
  }
  if (irNode.on_error === "continue") ports.push("error");
  return ports;
}
