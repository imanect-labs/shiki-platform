"use client";

/// スキルエディタの編集状態と、AI が用意した下書きスキル（skill_draft・#919）。
///
/// - 編集状態（[`SkillDraft`]）は保存 API の `SkillBody` と相互変換する。エディタで編集しない
///   項目（ファイル単位のスコープ・参照資料・スラッシュコマンド）も持ち回し、再保存で失わない。
/// - チャットの save_skill が返す skill_draft は、ノート等の下書きと同じ generic ストア
///   （(threadId, name) キー）へ入れ、スキルエディタ（`/skills/new?thread=&name=`）で詰めて保存する。

import type { ScriptKind, SkillBody, ToolName } from "@/generated/gui-spec";
import { createDraftStore } from "@/lib/drafts/store";

export type SkillDraft = {
  name: string;
  description: string;
  /// SKILL.md 本文（Markdown・ドライブ参照は `shiki://drive/...` のリンク）。
  instructions: string;
  /// 知識スコープのフォルダ id（表示はパスを引き直す）。
  scopeFolders: string[];
  /// ファイル単位スコープ（エディタでは一覧表示と削除のみ）。
  scopeFiles: string[];
  allowedTools: ToolName[] | null; // null = 全ツール
  model: string;
  temperature: string;
  maxTokens: string;
  fewShot: { user: string; assistant: string }[];
  scripts: { path: string; kind: ScriptKind; source: string }[];
  /// 参照資料（エディタでは編集しないが、既存値を保存で失わない）。
  references: string[];
  /// スラッシュコマンド宣言（#387）。エディタでは編集しないが、既存値を保存で失わない
  /// （first-party バンドルの宣言をこの画面の再保存で消さないため）。
  command: SkillBody["command"];
};

export function emptySkillDraft(name = ""): SkillDraft {
  return {
    name,
    description: "",
    instructions: "",
    scopeFolders: [],
    scopeFiles: [],
    allowedTools: null,
    model: "",
    temperature: "",
    maxTokens: "",
    fewShot: [],
    scripts: [],
    references: [],
    command: null,
  };
}

export function draftFromBody(name: string, b: SkillBody): SkillDraft {
  return {
    name,
    description: b.description ?? "",
    instructions: b.instructions ?? "",
    scopeFolders: b.knowledge_scope?.folders ?? [],
    scopeFiles: b.knowledge_scope?.files ?? [],
    allowedTools: b.allowed_tools ?? null,
    model: b.model?.model ?? "",
    temperature: b.model?.temperature != null ? String(b.model.temperature) : "",
    maxTokens: b.model?.max_tokens != null ? String(b.model.max_tokens) : "",
    fewShot: (b.few_shot ?? []).map((f) => ({ user: f.user, assistant: f.assistant })),
    scripts: (b.scripts ?? []).map((s) => ({ path: s.path, kind: s.kind, source: s.source })),
    references: b.references ?? [],
    command: b.command ?? null,
  };
}

export function draftToBody(d: SkillDraft): SkillBody {
  const model =
    d.model.trim() || d.temperature.trim() || d.maxTokens.trim()
      ? {
          model: d.model.trim() || null,
          temperature: d.temperature.trim() ? Number(d.temperature) : null,
          max_tokens: d.maxTokens.trim() ? Number(d.maxTokens) : null,
        }
      : null;
  return {
    description: d.description.trim(),
    instructions: d.instructions,
    knowledge_scope:
      d.scopeFolders.length > 0 || d.scopeFiles.length > 0
        ? { folders: d.scopeFolders, files: d.scopeFiles }
        : null,
    allowed_tools: d.allowedTools,
    model,
    few_shot: d.fewShot.filter((f) => f.user.trim() && f.assistant.trim()),
    scripts: d.scripts.filter((s) => s.path.trim()),
    references: d.references,
    command: d.command,
  };
}

/// AI の下書きを編集中の状態へ当てる。AI が書かない項目（参照資料・コマンド）は今の値を保ち、
/// 名前は未入力のときだけ下書きの名前を使う（既存スキルの名前は変えられない）。
export function applyAiSkill(current: SkillDraft, name: string, body: SkillBody): SkillDraft {
  const next = draftFromBody(current.name.trim() ? current.name : name, body);
  return { ...next, references: current.references, command: current.command };
}

/// アシスタントへ渡す「編集中のスキル」（選択コンテキスト skill_draft の抜粋）。
/// save_skill の入力と同じ形にし、AI がそのまま直して呼び直せるようにする。
export function draftContextExcerpt(d: SkillDraft): string {
  const body = draftToBody(d);
  return JSON.stringify(
    {
      name: d.name.trim(),
      description: body.description,
      instructions: body.instructions,
      ...(body.allowed_tools ? { allowed_tools: body.allowed_tools } : {}),
      ...(body.knowledge_scope ? { knowledge_scope: body.knowledge_scope } : {}),
      ...(body.model ? { model: body.model } : {}),
      ...(body.few_shot.length > 0 ? { few_shot: body.few_shot } : {}),
      ...(body.scripts.length > 0 ? { scripts: body.scripts } : {}),
    },
    null,
    1,
  );
}

// ── 下書きスキル（skill_draft）──────────────────────────────────────────

export const skillDraftStore = createDraftStore("skill");

/// 下書きスキルを開くスキルエディタの URL（(threadId, name) キー・カード/ストリームで共有）。
export function skillDraftHref(threadId: string, name: string): string {
  return `/skills/new?thread=${encodeURIComponent(threadId)}&name=${encodeURIComponent(name)}`;
}

/// skill_draft イベント/ブロックの payload（`{name, skill}`）を検証する（fail-closed）。
/// 中身はサーバの保存時検証を通ったものだが、エディタが読む形だけはここで確かめる。
export function parseSkillDraft(raw: unknown): { name: string; body: SkillBody } | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.name !== "string" || o.name.length === 0) return null;
  const body = parseSkillBody(o.skill);
  return body ? { name: o.name, body } : null;
}

/// 下書きストアの本文（SkillBody の JSON 文字列）を読む。
export function parseStoredSkillBody(content: string): SkillBody | null {
  try {
    return parseSkillBody(JSON.parse(content));
  } catch {
    return null;
  }
}

function parseSkillBody(raw: unknown): SkillBody | null {
  if (!raw || typeof raw !== "object") return null;
  const b = raw as Partial<SkillBody>;
  if (typeof b.description !== "string" || typeof b.instructions !== "string") return null;
  const strings = (v: unknown) =>
    Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  return {
    description: b.description,
    instructions: b.instructions,
    knowledge_scope:
      b.knowledge_scope && typeof b.knowledge_scope === "object"
        ? { folders: strings(b.knowledge_scope.folders), files: strings(b.knowledge_scope.files) }
        : null,
    allowed_tools: Array.isArray(b.allowed_tools) ? (strings(b.allowed_tools) as ToolName[]) : null,
    model: b.model && typeof b.model === "object" ? b.model : null,
    few_shot: Array.isArray(b.few_shot)
      ? b.few_shot.filter(
          (f): f is { user: string; assistant: string } =>
            !!f && typeof f.user === "string" && typeof f.assistant === "string",
        )
      : [],
    scripts: Array.isArray(b.scripts)
      ? b.scripts.filter(
          (s): s is SkillBody["scripts"][number] =>
            !!s && typeof s.path === "string" && typeof s.source === "string",
        )
      : [],
    references: strings(b.references),
    command: null,
  };
}
