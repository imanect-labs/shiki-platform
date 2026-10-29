/// ブロックを画面で指すときの名前（付録 C の P1・用語集の「ブロックの種類」）。
///
/// 作成者が付けた名前（`label`）が無ければ、カタログのブロック名（「スキルを使う」など）で言う。ブロックの ID
/// （`invoke_1` など）は出さない。

import { NODE_CATALOG } from "@/generated/workflow-catalog";
import type { Node as IrNode } from "@/generated/workflow-ir";

/// ブロックの名前（付けた名前 → カタログのブロック名 → 「ブロック」）。
export function blockDisplayName(node: Pick<IrNode, "label" | "type"> | undefined): string {
  return node?.label || NODE_CATALOG.find((c) => c.type === node?.type)?.label_ja || "ブロック";
}
