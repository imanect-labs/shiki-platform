import type { components } from "@/generated/api";
import { ApiError } from "@/lib/admin-ai-api";
import { apiFetch } from "@/lib/api";

/// カスタム指示（設定 → パーソナライズ・#900）。型は OpenAPI(utoipa) からの生成物。
export type CustomInstructions = components["schemas"]["CustomInstructions"];
export type CustomInstructionTone = components["schemas"]["CustomInstructionTone"];

/// 「あなたについて」「答え方の希望」それぞれの上限（サーバの chat::custom_instructions::MAX_CHARS と同じ）。
export const CUSTOM_INSTRUCTION_MAX_CHARS = 1500;

/// サーバと同じ数え方の文字数（前後の空白を除いた Unicode のコードポイント数）。
export function instructionChars(text: string): number {
  return Array.from(text.trim()).length;
}

/// 欠けたフィールドを空の既定値で埋めた形（生成型ではすべて任意）。
export type CustomInstructionsDraft = Required<CustomInstructions>;

export function draftOf(v: CustomInstructions): CustomInstructionsDraft {
  return { about: v.about ?? "", style: v.style ?? "", tone: v.tone ?? "default" };
}

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    if (res.status === 401 && typeof window !== "undefined") {
      const next = `${window.location.pathname}${window.location.search}`;
      window.location.assign(`/login?next=${encodeURIComponent(next)}`);
    }
    let detail = "";
    try {
      detail = ((await res.json()) as { detail?: string }).detail ?? "";
    } catch {
      // 本文が JSON でない
    }
    const fallback =
      res.status === 503
        ? "この環境ではチャットが有効になっていないため、カスタム指示を使えません"
        : `エラーが発生しました（${res.status}）`;
    throw new ApiError(res.status, detail || fallback);
  }
  return (await res.json()) as T;
}

/// 自分のカスタム指示（未登録なら空）。
export async function fetchCustomInstructions(): Promise<CustomInstructionsDraft> {
  return draftOf(await json<CustomInstructions>(await apiFetch("/me/custom-instructions")));
}

/// 自分のカスタム指示を置き換える（保存後の値＝前後の空白などを整えた値を返す）。
export async function saveCustomInstructions(
  body: CustomInstructionsDraft,
): Promise<CustomInstructionsDraft> {
  const res = await apiFetch("/me/custom-instructions", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  return draftOf(await json<CustomInstructions>(res));
}
