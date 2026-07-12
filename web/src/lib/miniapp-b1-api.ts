/// コードベース・ミニアプリ（B1）のクライアント側データ層（Task 9.11/9.13b）。
///
/// レジストリ一覧・同意インストール・インストール済み一覧・アンインストールを叩く。
/// 型は backend（utoipa → api.d.ts）を正とし、ここは薄いラッパのみ。

"use client";

import { apiFetch } from "@/lib/api";

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
};

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) {
    let message = `HTTP ${res.status}`;
    try {
      const body = (await res.json()) as { error?: string; title?: string };
      message = body.error ?? body.title ?? message;
    } catch {
      // 本文なし（204 等）はステータスのみ。
    }
    throw new Error(message);
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
}): Promise<{ app_id: string; table_ids: string[] }> {
  const res = await apiFetch("/apps/installations", {
    method: "POST",
    body: JSON.stringify({
      name: input.name,
      version: input.version,
      granted_scopes: input.grantedScopes,
      viewer_roles: input.viewerRoles ?? [],
      editor_roles: input.editorRoles ?? [],
    }),
  });
  return ok(res);
}

export async function uninstallApp(appId: string): Promise<void> {
  const res = await apiFetch(`/apps/installations/${appId}`, { method: "DELETE" });
  await ok<void>(res);
}

/// マニフェスト（レジストリ経由の要求スコープ表示に使う）。
export async function fetchManifest(
  artifactId: string,
  version?: number,
): Promise<{ requested_scopes: string[]; description: string; name: string; version: string }> {
  const q = version !== undefined ? `?version=${version}` : "";
  const res = await apiFetch(`/apps/manifests/${artifactId}${q}`);
  const body = await ok<{ manifest: { requested_scopes: string[]; description: string; name: string; version: string } }>(res);
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
