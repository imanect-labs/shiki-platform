/// ホームの「今日のこと」の API（#965）。どれも本人の分だけを返す読み取り API。
/// 型は OpenAPI からの生成物（手書きの対応物を作らない）。

import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";

export type PendingApproval = components["schemas"]["PendingApproval"];
export type WorkflowRunDigest = components["schemas"]["WorkflowRunDigestResponse"];
export type FailedWorkflowRun = components["schemas"]["FailedWorkflowRun"];
export type MyUsage = components["schemas"]["MyUsageResponse"];

async function getJson<T>(path: string): Promise<T> {
  const res = await apiFetch(path);
  if (!res.ok) throw new Error(`${path} が ${res.status} を返しました`);
  return (await res.json()) as T;
}

/// 自分の承認を待っている run（新しい順・最大 5 件）。
export async function fetchPendingApprovals(): Promise<PendingApproval[]> {
  const data = await getJson<components["schemas"]["PendingApprovalsResponse"]>("/me/approvals");
  return data.items;
}

/// 本人に見えるワークフローの、直近 24 時間に終わった実行の集計。
export function fetchWorkflowRunDigest(): Promise<WorkflowRunDigest> {
  return getJson<WorkflowRunDigest>("/me/workflow-runs");
}

/// 本人の当月の利用額と上限（LLM 未構成の環境では 503）。
export function fetchMyUsage(): Promise<MyUsage> {
  return getJson<MyUsage>("/me/usage");
}

/// 上限に対する利用の割合（0〜100 の整数・上限が無ければ null）。
export function usagePercent(usage: MyUsage): number | null {
  const limit = usage.limit_usd_micros;
  if (limit === null || limit === undefined || limit <= 0) return null;
  return Math.min(100, Math.floor((usage.used_usd_micros / limit) * 100));
}
