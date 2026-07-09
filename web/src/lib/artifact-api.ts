/// アーティファクト（skill / UIスペック / ミニアプリ）のクライアント側データ層（Phase 6）。
///
/// backend の `/skills` `/mini-apps` `/ui-specs` `/artifacts`（共有・版）と
/// UI アクション実行 API を叩く。型は codegen（gui-spec.ts / api.d.ts）を正とし、
/// 手書きミラーを作らない（camelCase 変換の薄いラッパのみ）。

"use client";

import { apiFetch } from "@/lib/api";
import type { MiniAppBody, SkillBody, UiSpecDoc } from "@/generated/gui-spec";

export type { MiniAppBody, SkillBody, UiSpecDoc };

/// 共有語彙（backend artifact::ArtifactRole / storage::ShareTarget と一致）。
export type ArtifactRole = "viewer" | "editor";
export type ShareTarget = { type: "user"; id: string } | { type: "role"; id: string };
export type ArtifactShareEntry = { target: ShareTarget; role: ArtifactRole };

export type ArtifactKind = "workflow" | "ui_spec" | "mini_app" | "skill" | "script";

export type ArtifactMeta = {
  id: string;
  kind: ArtifactKind;
  name: string;
  owner: string;
  currentVersion: number;
  createdAt: string;
  updatedAt: string;
};

export type VersionMeta = { version: number; createdBy: string; createdAt: string };

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) {
    // 検証エラー（400 の {errors:[...]}）は本文をそのまま投げて UI が全件表示できるようにする。
    let detail = "";
    try {
      const body = (await res.json()) as { errors?: { code: string; message: string; path?: string }[] };
      if (body.errors?.length) {
        detail = body.errors
          .map((e) => (e.path ? `${e.message}（${e.path}）` : e.message))
          .join(" / ");
      }
    } catch {
      // 本文なし
    }
    throw new Error(detail || `API ${res.status}`);
  }
  return (await res.json()) as T;
}

function json(body: unknown): RequestInit {
  return {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  };
}

// ── アーティファクト共通枠（一覧・版・共有）───────────────────────────

type ApiArtifact = {
  id: string;
  kind: ArtifactKind;
  name: string;
  owner: string;
  current_version: number;
  created_at: string;
  updated_at: string;
};

function toMeta(a: ApiArtifact): ArtifactMeta {
  return {
    id: a.id,
    kind: a.kind,
    name: a.name,
    owner: a.owner,
    currentVersion: a.current_version,
    createdAt: a.created_at,
    updatedAt: a.updated_at,
  };
}

export async function listArtifacts(kind: ArtifactKind): Promise<ArtifactMeta[]> {
  const data = await ok<{ items: ApiArtifact[] }>(
    await apiFetch(`/artifacts?kind=${encodeURIComponent(kind)}`),
  );
  return data.items.map(toMeta);
}

export async function getArtifact(id: string): Promise<ArtifactMeta> {
  return toMeta(await ok<ApiArtifact>(await apiFetch(`/artifacts/${id}`)));
}

export async function deleteArtifact(id: string): Promise<void> {
  const res = await apiFetch(`/artifacts/${id}`, { method: "DELETE" });
  if (!res.ok) throw new Error(`削除に失敗しました (${res.status})`);
}

export async function listArtifactVersions(id: string): Promise<VersionMeta[]> {
  const data = await ok<{ items: { version: number; created_by: string; created_at: string }[] }>(
    await apiFetch(`/artifacts/${id}/versions`),
  );
  return data.items.map((v) => ({
    version: v.version,
    createdBy: v.created_by,
    createdAt: v.created_at,
  }));
}

export async function shareArtifact(
  id: string,
  target: ShareTarget,
  role: ArtifactRole,
): Promise<void> {
  const res = await apiFetch(`/artifacts/${id}/shares`, { ...json({ target, role }), method: "PUT" });
  if (!res.ok) throw new Error(`共有に失敗しました (${res.status})`);
}

export async function unshareArtifact(
  id: string,
  target: ShareTarget,
  role: ArtifactRole,
): Promise<void> {
  const res = await apiFetch(`/artifacts/${id}/shares`, {
    ...json({ target, role }),
    method: "DELETE",
  });
  if (!res.ok) throw new Error(`共有解除に失敗しました (${res.status})`);
}

export async function listArtifactShares(id: string): Promise<ArtifactShareEntry[]> {
  return ok<ArtifactShareEntry[]>(await apiFetch(`/artifacts/${id}/shares`));
}

// ── skill（Task 6.7）────────────────────────────────────────────────────

export type SkillVersion = { id: string; version: number; body: SkillBody };

export async function createSkill(name: string, body: SkillBody): Promise<SkillVersion> {
  return ok<SkillVersion>(await apiFetch("/skills", json({ name, body })));
}

export async function updateSkill(
  id: string,
  body: SkillBody,
  expectedVersion?: number,
): Promise<SkillVersion> {
  return ok<SkillVersion>(
    await apiFetch(`/skills/${id}`, {
      ...json({ body, expected_version: expectedVersion }),
      method: "PUT",
    }),
  );
}

export async function getSkill(id: string, version?: number): Promise<SkillVersion> {
  const path = version != null ? `/skills/${id}/versions/${version}` : `/skills/${id}`;
  return ok<SkillVersion>(await apiFetch(path));
}

// ── ミニアプリ（Task 6.10）──────────────────────────────────────────────

export type MiniAppVersion = { id: string; version: number; body: MiniAppBody };

export type ResolvedMiniApp = {
  id: string;
  version: number;
  body: MiniAppBody;
  /// 検証済み UI スペック（描画の正）。
  ui_spec: UiSpecDoc;
};

export async function createMiniApp(name: string, body: MiniAppBody): Promise<MiniAppVersion> {
  return ok<MiniAppVersion>(await apiFetch("/mini-apps", json({ name, body })));
}

export async function updateMiniApp(
  id: string,
  body: MiniAppBody,
  expectedVersion?: number,
): Promise<MiniAppVersion> {
  return ok<MiniAppVersion>(
    await apiFetch(`/mini-apps/${id}`, {
      ...json({ body, expected_version: expectedVersion }),
      method: "PUT",
    }),
  );
}

export async function resolveMiniApp(id: string, version?: number): Promise<ResolvedMiniApp> {
  const qs = version != null ? `?version=${version}` : "";
  return ok<ResolvedMiniApp>(await apiFetch(`/mini-apps/${id}/resolved${qs}`));
}

// ── UI アクション実行（Task 6.5・action_id + params のみ送れる）─────────

export type UiActionResult = { result: Record<string, unknown> };

export async function invokeChatUiAction(
  threadId: string,
  messageId: string,
  actionId: string,
  params: unknown,
): Promise<UiActionResult> {
  return ok<UiActionResult>(
    await apiFetch(
      `/threads/${threadId}/messages/${messageId}/ui-actions`,
      json({ action_id: actionId, params }),
    ),
  );
}

export async function invokeMiniAppUiAction(
  appId: string,
  version: number,
  actionId: string,
  params: unknown,
): Promise<UiActionResult> {
  return ok<UiActionResult>(
    await apiFetch(
      `/mini-apps/${appId}/ui-actions`,
      json({ version, action_id: actionId, params }),
    ),
  );
}
