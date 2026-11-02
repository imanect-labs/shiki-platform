/// コードベース・ミニアプリ（B1）のクライアント側データ層（Task 9.11/9.13b）。
///
/// レジストリ一覧・同意インストール・インストール済み一覧・アンインストールを叩く。
/// 型は backend（utoipa → api.d.ts）を正とし、ここは薄いラッパのみ。

"use client";

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";

type InstallAppRequest = components["schemas"]["InstallAppRequest"];
type MiniAppManifest = components["schemas"]["MiniAppManifest"];
/// アプリのテーブルと、それを使える部署・グループ（GAP-68）。
export type AppTableAccess = components["schemas"]["AppTableAccess"];
/// 足す・外す部署・グループ（インストールの要求の viewer_roles・editor_roles・submitter_roles と同じ意味）。
export type AppTableRolesChange = components["schemas"]["AppTableRolesChange"];

export type RegistryEntry = {
  id: string;
  artifact_kind: string;
  name: string;
  version: string;
  artifact_id: string;
  artifact_version: number;
  manifest_digest: string;
  publisher: string;
  trust_tier: string;
  yanked: boolean;
  created_at: string;
};

export type AppInstallation = {
  id: string;
  app_id: string;
  app_name: string;
  installed_version: string;
  granted_scopes: string[];
  client_id_b1: string | null;
  client_id_b2: string | null;
  installed_by: string;
  created_at: string;
  ai: {
    budget_models: string[];
    budget_daily_usd_micros: number | null;
    budget_max_tokens: number | null;
    agent_tools: string[];
  };
  frontend_bundle: string | null;
  /// Keycloak にアプリの登録が無く、サーバーが作り直せなかった（入れ直しが要る・GAP-70）。前の版のサーバーは返さない。
  oauth_client_missing?: boolean;
  /// 管理者が Keycloak で B2（サーバーで動く処理）の登録を止めていて、サーバーが戻さなかった（BUG-201）。前の版のサーバーは返さない。
  oauth_client_disabled?: boolean;
  /// アプリが持つテーブルの数（テーブルを使える部署・グループを変える口を出すか・GAP-68）。前の版のサーバーは返さない。
  table_count?: number;
};

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = `HTTP ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string; title?: string; detail?: string };
      message = body.error ?? body.detail ?? body.title ?? message;
    } catch {
      // 本文なし（204 等）はステータスのみ。
    }
    // 呼び出し側が 403（変えられない人）を見分けられるよう、状態を添える。
    throw Object.assign(new Error(message), { status: res.status });
  }
  return (res.status === 204 ? undefined : await res.json()) as T;
}

export async function listRegistry(): Promise<RegistryEntry[]> {
  const res = await apiFetch("/apps/registry");
  const body = await ok<{ items: RegistryEntry[] }>(res);
  return body.items;
}

export async function listInstallations(): Promise<AppInstallation[]> {
  const res = await apiFetch("/apps/installations");
  const body = await ok<{ items: AppInstallation[] }>(res);
  return body.items;
}

export async function installApp(input: {
  name: string;
  version: string;
  grantedScopes: string[];
  viewerRoles?: string[];
  editorRoles?: string[];
  /// アプリのテーブルに「追加だけ」（レコードを追加し、自分が追加したレコードだけ見られる）を渡す部署・グループ。
  submitterRoles?: string[];
}): Promise<{ app_id: string; table_ids: string[] }> {
  const body: InstallAppRequest = {
    name: input.name,
    version: input.version,
    granted_scopes: input.grantedScopes,
    viewer_roles: input.viewerRoles ?? [],
    editor_roles: input.editorRoles ?? [],
    submitter_roles: input.submitterRoles ?? [],
  };
  // 文字列の本文は Content-Type を付けないと text/plain になり、サーバは 415 で断る（画面からのインストールが通らなかった）。
  const res = await apiFetch("/apps/installations", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  return ok(res);
}

export async function uninstallApp(appId: string): Promise<void> {
  const res = await apiFetch(`/apps/installations/${appId}`, { method: "DELETE" });
  await ok<void>(res);
}

/// アプリのテーブルと、それを使える部署・グループを読む（インストールできる人だけ・それ以外は 403）。
export async function getAppTableRoles(appId: string): Promise<AppTableAccess> {
  return ok(await apiFetch(`/apps/installations/${appId}/roles`));
}

/// アプリのテーブルを使える部署・グループを足す・外す（アプリのすべてのテーブルに効く）。変えた後の状態を返す。
export async function updateAppTableRoles(
  appId: string,
  change: AppTableRolesChange,
): Promise<AppTableAccess> {
  const res = await apiFetch(`/apps/installations/${appId}/roles`, {
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(change),
  });
  return ok(res);
}

/// マニフェスト（レジストリ経由の要求スコープ表示に使う）。
export async function fetchManifest(
  artifactId: string,
  version?: number,
): Promise<MiniAppManifest> {
  const q = version !== undefined ? `?version=${version}` : "";
  const res = await apiFetch(`/apps/manifests/${artifactId}${q}`);
  const body = await ok<{ manifest: MiniAppManifest }>(res);
  return body.manifest;
}

/// B1 配信オリジン（第3リスナ）。compose/dev の既定はポート 8091。
export function b1Origin(): string {
  return process.env.NEXT_PUBLIC_B1_ORIGIN ?? "http://localhost:8091";
}

/// ゲートウェイ（第2リスナ）のオリジン。ミニアプリの connect-src と同一。
export function gatewayOrigin(): string {
  return process.env.NEXT_PUBLIC_GATEWAY_ORIGIN ?? "http://localhost:8090";
}
