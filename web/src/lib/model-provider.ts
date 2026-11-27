/// モデルの提供元（D-99）。入れた内容がどこへ送られるかを画面で言うための、提供元の表示名と国外処理の印。
///
/// 表示名と国外処理の印は `GET /llm/models` の一覧（llm-gateway のカタログとプロバイダの設定が単一定義）から
/// 引き、画面に提供元の表を手で書かない。

import type { LlmModelsResponse } from "@/lib/admin-ai-api";

export type ModelProvider = {
  /// 提供元の表示名（例: Anthropic・社内 GPU）。
  label: string;
  /// 日本国外で処理されるか（「国外処理」の印）。
  abroad: boolean;
};

/// ブロックのモデル（省略なら組織の既定）が送る先の提供元。一覧に無いモデル・組織の既定を引けないときは `null`。
///
/// 組織の既定がおまかせ（Auto）なら、いまの振り先のモデルの提供元を返す（管理者が振り先を変えると変わる）。
export function providerOf(data: LlmModelsResponse, model: string | undefined | null): ModelProvider | null {
  const id = model || (data.default_model === "auto" ? data.auto?.model : data.default_model);
  const found = id ? data.models.find((m) => m.id === id) : undefined;
  return found ? { label: found.provider_label, abroad: found.processed_abroad } : null;
}
