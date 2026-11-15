import type { components } from "@/generated/api";
import { apiFetch } from "@/lib/api";
export type Role = components["schemas"]["ManagedRole"];
export type RolePage = components["schemas"]["ManagedRolePage"];
export type Group = components["schemas"]["AdGroup"];
export type GroupPage = components["schemas"]["AdGroupPage"];
export type FolderPage = components["schemas"]["AdFolderPage"];
export type Changes = components["schemas"]["DirectoryChangeSet"];
export type RoleEdit = components["schemas"]["RoleEdit"];
export type Mapping = components["schemas"]["GroupRoleMapping"];
export type Operation = components["schemas"]["DirectoryOperation"];
export type Plan = components["schemas"]["DirectoryPlan"];
export type Info = components["schemas"]["DirectoryInfo"];
export type Connection = components["schemas"]["LdapConnectionStatus"];
export type ConnectionInput = components["schemas"]["LdapConnectionInput"];
export const base = "/org-admin/directory";
export async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await apiFetch(`${base}${path}`, init);
  if (!response.ok) {
    const error = await response.json().catch(() => null);
    throw new Error(
      error?.error?.message ??
        error?.message ??
        (response.status === 503
          ? "Keycloak・ADへの接続や証明書、管理接続の設定を確認してください。"
          : response.status === 409
            ? "他の操作が進行中、または表示後に内容が変更されました。操作履歴を確認し、再読込してください。"
            : `取得・保存に失敗しました（${response.status}）`),
    );
  }
  return response.json();
}
export const post = <T>(path: string, body: unknown) =>
  request<T>(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
export const params = (values: Record<string, string | null | undefined>) =>
  `?${new URLSearchParams(Object.entries(values).filter((entry): entry is [string, string] => entry[1] != null))}`;
export const newChanges = (): Changes => ({
  idempotency_key: crypto.randomUUID(),
  roles: [],
  mappings: [],
});
export const hasChanges = (changes: Changes) =>
  changes.roles.length + changes.mappings.length > 0;
export const rolePath = (role: Role) =>
  role.path_labels.join(" / ") || role.label;
