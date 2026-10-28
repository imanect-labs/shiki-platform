import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";
import { ApiError } from "@/lib/admin-ai-api";

/// 構造化データの管理（組織管理者・`/org-admin/data/*`・Task 15.12）。型は OpenAPI(utoipa) から生成した
/// ものだけを使う（手書きの対応物を作らない）。
export type DataTenantSettings = components["schemas"]["DataTenantSettings"];
export type UpdateDataSettingsRequest = components["schemas"]["UpdateDataSettingsRequest"];
export type OptimizerStatus = components["schemas"]["OptimizerStatus"];
export type CalibrationReport = components["schemas"]["CalibrationReport"];
export type UnitCosts = components["schemas"]["UnitCosts"];
export type TableOptimizerReport = components["schemas"]["TableOptimizerReport"];
export type OptimizerIndexCandidate = components["schemas"]["OptimizerIndexCandidate"];

export { ApiError };

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let detail = "";
    try {
      const body = (await res.json()) as { detail?: string; message?: string };
      detail = body.detail ?? body.message ?? "";
    } catch {
      // 本文が JSON でない（プロキシのエラーページ等）
    }
    const fallback =
      res.status === 403
        ? "組織の管理者だけが使えます"
        : res.status === 404
          ? "テーブルが見つかりません（削除されたか、ID が違います）"
          : `エラーが発生しました（${res.status}）`;
    throw new ApiError(res.status, detail || fallback);
  }
  return (await res.json()) as T;
}

const put = (body: unknown): RequestInit => ({
  method: "PUT",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify(body),
});

export async function fetchDataSettings(): Promise<DataTenantSettings> {
  return json(await apiFetch("/org-admin/data/settings"));
}

/// 設定を更新する。`count_cap` は**省略すると既定へ戻る**契約なので、変えないときも今の値を送る。
export async function updateDataSettings(body: UpdateDataSettingsRequest): Promise<DataTenantSettings> {
  return json(await apiFetch("/org-admin/data/settings", put(body)));
}

export async function fetchOptimizerStatus(): Promise<OptimizerStatus> {
  return json(await apiFetch("/org-admin/data/optimizer"));
}

export async function fetchTableOptimizer(tableId: string): Promise<TableOptimizerReport> {
  return json(await apiFetch(`/org-admin/data/tables/${encodeURIComponent(tableId)}/optimizer`));
}

export async function updateTableOptimizer(tableId: string, fixedPlan: boolean): Promise<TableOptimizerReport> {
  return json(
    await apiFetch(
      `/org-admin/data/tables/${encodeURIComponent(tableId)}/optimizer`,
      put({ fixed_plan: fixedPlan } satisfies components["schemas"]["UpdateTableOptimizerRequest"]),
    ),
  );
}
