/// skill レジストリ / 同意インストール API（#344 Task 10.11）。
///
/// publish = 所有 skill をテナントのレジストリへ不変公開（owner のみ）。
/// install = 本人のカタログへ載せる明示行為（skill ツールの一覧に載る）。

import { apiFetch } from "@/lib/api";

export type SkillRegistryEntry = {
  name: string;
  version: string;
  artifactId: string;
  trustTier: string;
  yanked: boolean;
  createdAt: string;
};

export type SkillInstallation = {
  name: string;
  registryVersion: string;
  skillId: string;
  skillVersion: number;
  trustTier: string;
};

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`API ${res.status}`);
  return (await res.json()) as T;
}

type ApiEntry = {
  name: string;
  version: string;
  artifact_id: string;
  trust_tier: string;
  yanked: boolean;
  created_at: string;
};

export async function listSkillRegistry(): Promise<SkillRegistryEntry[]> {
  const data = await ok<{ entries: ApiEntry[] }>(await apiFetch("/skills/registry"));
  return data.entries.map((e) => ({
    name: e.name,
    version: e.version,
    artifactId: e.artifact_id,
    trustTier: e.trust_tier,
    yanked: e.yanked,
    createdAt: e.created_at,
  }));
}

type ApiInstallation = {
  name: string;
  registry_version: string;
  skill_id: string;
  skill_version: number;
  trust_tier: string;
};

export async function listSkillInstallations(): Promise<SkillInstallation[]> {
  const data = await ok<{ installations: ApiInstallation[] }>(
    await apiFetch("/skills/installations"),
  );
  return data.installations.map((i) => ({
    name: i.name,
    registryVersion: i.registry_version,
    skillId: i.skill_id,
    skillVersion: i.skill_version,
    trustTier: i.trust_tier,
  }));
}

export async function installSkill(name: string, version?: string): Promise<void> {
  const res = await apiFetch("/skills/installations", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, version }),
  });
  if (!res.ok) throw new Error(`API ${res.status}`);
}

export async function uninstallSkill(name: string): Promise<void> {
  const res = await apiFetch(`/skills/installations/${encodeURIComponent(name)}`, {
    method: "DELETE",
  });
  if (!res.ok) throw new Error(`API ${res.status}`);
}

/// 所有 skill をレジストリへ公開する（version 未指定は現行バージョン番号・in-house）。
export async function publishSkill(artifactId: string, version?: string): Promise<void> {
  const res = await apiFetch(`/skills/${artifactId}/publish`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ version, trust_tier: "in_house" }),
  });
  if (res.status === 409) throw new Error("このバージョンは既に公開済みです");
  if (!res.ok) throw new Error(`API ${res.status}`);
}
