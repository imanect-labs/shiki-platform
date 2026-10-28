/// アーティファクト（skill / ミニアプリ）のクライアント側データ層（Phase 6）。
///
/// backend の `/skills` `/mini-apps` `/artifacts`（共有・版）と図のボタンの実行 API を叩く。型は codegen（gui-spec.ts / api.d.ts）を正とし、
/// 手書きミラーを作らない（camelCase 変換の薄いラッパのみ）。

"use client";

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";
import type { MiniAppBody, SkillBody } from "@/generated/gui-spec";

export type { MiniAppBody, SkillBody };

/// 共有語彙（backend artifact::ArtifactRole / storage::ShareTarget と一致）。
export type ArtifactRole = "viewer" | "editor";
export type ShareTarget = { type: "user"; id: string } | { type: "role"; id: string };
/// 共有一覧の 1 件（表示名・email はサーバがテナント内ディレクトリから解決・#741）。
export type ArtifactShareEntry = components["schemas"]["ArtifactShareEntry"];

export type ArtifactKind = "workflow" | "mini_app" | "skill" | "script";

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
    // 状態コードを添える（見つからない・権限がない＝403/404 を画面が「見つかりません」として出し分ける）。
    throw Object.assign(new Error(detail || `API ${res.status}`), { status: res.status });
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
  /// 検証済みの本文（画面＝Yui の ```ui を含む Markdown と、束ねたワークフロー）。
  body: MiniAppBody;
};

/// 画面のボタンで起動したワークフロー 1 件（本人の分・#604・codegen）。
export type MiniAppRun = components["schemas"]["MiniAppRun"];

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

// ── 図のボタンの実行（Task 6.5・#604・action_id + params のみ送れる）─────

export type UiActionResult = { result: Record<string, unknown> };

/// 同じ図の同じ操作が既に実行済み（サーバが二重送信を拒否した・#410）。
///
/// 表示の遅れ（別タブ・戻る操作・再送）で二度押しても事故にならないよう、UI はこれを
/// 失敗ではなく「送信済み」として扱い、図を送信済み表示へ倒す。
export class UiActionAlreadyInvoked extends Error {
  constructor() {
    super("この操作は送信済みです");
    this.name = "UiActionAlreadyInvoked";
  }
}

/// 本文の図（```ui）のボタンの実行（#604）。`actionId` は `{ブロックの識別子}:{a1}`。
///
/// ワークフローのボタンは送信ごとの `idempotencyKey` が要る（同じキーの再送は前の起動の結果を返す）。
/// 誤りは状況ごとに分けて投げる（画面が文言を出し分ける）。
export async function invokeChatYuiAction(
  threadId: string,
  messageId: string,
  actionId: string,
  values: Record<string, unknown>,
  idempotencyKey?: string,
): Promise<UiActionResult> {
  let res: Response;
  try {
    res = await apiFetch(
      `/threads/${threadId}/messages/${messageId}/ui-actions`,
      json({
        action_id: actionId,
        params: values,
        ...(idempotencyKey ? { idempotency_key: idempotencyKey } : {}),
      }),
    );
  } catch {
    throw new UiActionUncertain();
  }
  if (res.status >= 500) throw new UiActionUncertain();
  if (res.status === 409) throw new UiActionAlreadyInvoked();
  if (res.status === 403) throw new UiActionForbidden();
  if (res.status === 404) throw new UiActionNotFound();
  return ok<UiActionResult>(res);
}

/// 送信の結果が分からない（通信が途中で切れた・サーバーの障害）。受け付けられたかもしれないので、
/// 押し直すときは同じ冪等キーで送る。
export class UiActionUncertain extends Error {
  constructor() {
    super("送信の結果を確かめられませんでした。もう一度押すと、同じ送信として扱います");
    this.name = "UiActionUncertain";
  }
}

/// 会話の閲覧者が押した（ボタンは編集者だけが使える）。
export class UiActionForbidden extends Error {
  constructor(message = "この会話は閲覧のみのため、ボタンを使えません") {
    super(message);
    this.name = "UiActionForbidden";
  }
}

/// ボタンが見つからない（本文に無い・ワークフローを解決できなかった・権限が無い）。
export class UiActionNotFound extends Error {
  constructor() {
    super("このボタンの実行先が見つからないか、使う権限がありません");
    this.name = "UiActionNotFound";
  }
}

/// ミニアプリの画面の ```ui のボタンを実行する（#604）。誤りの投げ分けは会話のボタンと同じ。
export async function invokeMiniAppYuiAction(
  appId: string,
  version: number,
  actionId: string,
  values: Record<string, unknown>,
  idempotencyKey: string,
): Promise<UiActionResult> {
  let res: Response;
  try {
    res = await apiFetch(
      `/mini-apps/${appId}/ui-actions`,
      json({ version, action_id: actionId, params: values, idempotency_key: idempotencyKey }),
    );
  } catch {
    throw new UiActionUncertain();
  }
  if (res.status >= 500) throw new UiActionUncertain();
  if (res.status === 409) throw new UiActionAlreadyInvoked();
  if (res.status === 403) throw new UiActionForbidden("このアプリを使う権限がありません");
  if (res.status === 404) throw new UiActionNotFound();
  return ok<UiActionResult>(res);
}

/// 本人がミニアプリの画面のボタンで起動した記録（新しい 20 件を古い順に）。
export async function listMiniAppRuns(appId: string): Promise<MiniAppRun[]> {
  const res = await ok<{ runs: MiniAppRun[] }>(await apiFetch(`/mini-apps/${appId}/runs`));
  return res.runs;
}

/// 回答の本文の ```ui app を、そのボタンの実行先ごとミニアプリとして保存する（#604）。
export async function saveYuiBlockAsApp(
  threadId: string,
  messageId: string,
  block: string,
  name: string,
  description?: string,
): Promise<MiniAppVersion> {
  return ok<MiniAppVersion>(
    await apiFetch(
      `/threads/${threadId}/messages/${messageId}/save-as-app`,
      json({ block, name, ...(description ? { description } : {}) }),
    ),
  );
}
