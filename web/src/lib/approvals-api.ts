/// 承認の受信箱の API クライアント（human.approval・GAP-08）。
///
/// 型はすべて OpenAPI（utoipa）から生成した `@/generated/api` を使う（手書きの型を作らない）。
/// 判断の誤りは {@link ApprovalApiError} に状態コードと `code` を載せて投げ、画面が日本語の文言に写す。

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

type S = components["schemas"];

export type ApprovalItem = S["ApprovalItem"];
export type ApprovalDecision = S["ApprovalDecisionDto"];
export type ApprovalDecisionKind = S["ApprovalDecisionKind"];
export type ApprovalListResponse = S["ApprovalListResponse"];
export type ApprovalCountResponse = S["ApprovalCountResponse"];
export type ApprovalErrorCode = S["ApprovalErrorCode"];
export type ApprovalDecisionResponse = S["ApprovalDecisionResponse"];

/// 判断の API が返しうる `code`（判断の誤りと、遷移を data に任せた誤りの rev 不一致・読めない列）。
export type ApprovalApiErrorCode = ApprovalErrorCode | S["ConflictCode"] | S["ForbiddenCode"];

/// 誤りの本文（判断の誤り・data の 409 / 403 の構造化本文に共通する項目だけを読む）。
type ErrorBody = Partial<Pick<S["ApprovalErrorBody"], "message">> & { code?: ApprovalApiErrorCode };

/// 承認の API の誤り（状態コードと、本文の `code`）。
export class ApprovalApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: ApprovalApiErrorCode | null,
    message: string,
  ) {
    super(message);
  }
}

/// `code` → 利用者向けの文言（サーバの文言は使わず、画面の言葉で揃える）。判断の誤りの code はすべて扱う
/// （生成した型に code が増えたら型検査で気づく）。data の汎用の code は判断で起きるものだけを扱い、ほかは null。
function approvalCodeMessage(code: ApprovalApiErrorCode): string | null {
  switch (code) {
    case "comment_required":
      return "この判断にはコメントが必要です。";
    case "comment_too_long":
      return "コメントは 2000 字以内で書いてください。";
    case "unknown_decision":
      return "この承認ではその判断はできません。一覧を読み直してください。";
    case "not_allowed":
      return "この判断をする権限がありません（状態の遷移の規則で許されていません）。";
    case "not_decidable":
      return "いまの状態からは、この判断の行き先へ進めません。表を開いて状態を確かめてください。";
    case "decided":
      return "この承認はもう決まっているか、取り消されています。";
    case "expired":
      return "期限を過ぎたため判断できません。";
    case "deciding":
      return "ほかの担当者がこの承認を判断しています。一覧を読み直してください。";
    case "rev_conflict":
      return "ほかの人が先に行を更新しました。最新の内容を読み直したので、確かめてから判断してください。";
    case "field_unavailable":
      return "状態の列を読めないため判断できません。";
    default: {
      // 判断の誤りの code を取りこぼしていないこと（残りは data の汎用の 409 の code だけ）。
      const rest: Exclude<ApprovalApiErrorCode, ApprovalErrorCode> = code;
      void rest;
      return null;
    }
  }
}

/// 状態コード・`code` → 利用者向けの文言（サーバの文言は使わず、画面の言葉で揃える）。
export function approvalErrorMessage(e: unknown): string {
  if (!(e instanceof ApprovalApiError)) return "通信に失敗しました。時間をおいてもう一度お試しください。";
  const byCode = e.code ? approvalCodeMessage(e.code) : null;
  if (byCode) return byCode;
  if (e.status === 404) return "この承認は見つかりません（担当でないか、もう決まっています）。";
  if (e.status === 403) return "この判断をする権限がありません。";
  if (e.status === 409) return "ほかの操作と競合しました。一覧を読み直してください。";
  return "判断できませんでした。時間をおいてもう一度お試しください。";
}

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let code: ApprovalApiErrorCode | null = null;
    let message = `API ${res.status}`;
    try {
      const body = (await res.json()) as ErrorBody;
      code = body.code ?? null;
      if (body.message) message = body.message;
    } catch {
      // 本文なし
    }
    throw new ApprovalApiError(res.status, code, message);
  }
  return (await res.json()) as T;
}

/// 自分宛ての承認待ち（新しい順・50 件ずつ）。
export async function listApprovals(cursor?: string | null): Promise<ApprovalListResponse> {
  const q = cursor ? `?cursor=${encodeURIComponent(cursor)}` : "";
  return ok<ApprovalListResponse>(await apiFetch(`/approvals${q}`));
}

/// 自分宛ての承認待ちの件数（99 で打ち切り）。
export async function countApprovals(): Promise<ApprovalCountResponse> {
  return ok<ApprovalCountResponse>(await apiFetch("/approvals/count"));
}

/// 承認待ち 1 件（判断ごとに、いま実行できるかが付く）。
export async function getApproval(id: string): Promise<ApprovalItem> {
  return ok<ApprovalItem>(await apiFetch(`/approvals/${id}`));
}

/// 判断する（承認者本人の権限で行を判断の状態へ進める・run の前進は数秒以内）。
export async function decideApproval(
  id: string,
  decision: ApprovalDecisionKind,
  expectedRev: number,
  comment: string,
): Promise<ApprovalDecisionResponse> {
  const res = await apiFetch(`/approvals/${id}/decision`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      decision,
      expected_rev: expectedRev,
      comment: comment.trim() ? comment.trim() : null,
    }),
  });
  return ok<ApprovalDecisionResponse>(res);
}

/// 受信箱の件数が変わったことを、同じ画面の購読者（サイドバーのバッジ）へ知らせる。
export const APPROVALS_CHANGED_EVENT = "shiki:approvals-changed";

export function notifyApprovalsChanged(): void {
  if (typeof window !== "undefined") window.dispatchEvent(new Event(APPROVALS_CHANGED_EVENT));
}
