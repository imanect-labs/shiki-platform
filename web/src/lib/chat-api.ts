/// チャットのクライアント側データ層（実 backend 配線）。
///
/// backend（Phase 3 / #70）の `/threads` REST ＋ `/threads/:id/stream` SSE を叩く。生成は
/// **接続非依存ジョブ**（Task 3.11）で、送信は 202 を受けて即返し、SSE は replay-then-subscribe
/// で購読する（`Last-Event-ID`=seq で再接続時に途中から・重複しない）。ページ離脱しても生成は
/// 継続し、再訪時に `generation_event` から途中経過/確定/失敗/キャンセルを復元表示する。
///
/// 公開 API（型・関数シグネチャ）はモック時代から不変に保つ（UI 側は無改修）。

"use client";

import * as React from "react";

import { apiFetch } from "@/lib/api";
import type { components } from "@/generated/api";
import { newId } from "@/lib/chat-store";
import type { SelectionContext } from "@/lib/selection-context";

// ── content-block / SSE イベント（backend の単一定義から生成）───────────────

/// メッセージ本文の構造化ブロック。**`crates/chat` の `ContentBlock` から生成**した型を使う
/// （utoipa `ToSchema` → OpenAPI → openapi-typescript）。手書きのミラーは作らない —
/// CLAUDE.md の「codegen が正」に従い、Rust 側にフィールドを足したらここは自動で追随する。
export type ContentBlock = components["schemas"]["ContentBlock"];

/// 未保存の下書きノート（save_note の下書き確定型・issue #282）。
export type NoteDraft = { name: string; markdown: string };

/// 未保存の下書きスライド（save_slide の下書き確定型・Task 11.3）。content=正規化スライド JSON。
export type SlideDraft = { name: string; content: string };

/// 未保存の下書き CSV（save_csv の下書き確定型・Task 11.11）。csv=CSV 本文。
export type CsvDraft = { name: string; csv: string };

/// AI が作成/編集した文書への参照（#381）。kind=office/note/slide/csv/file。
export type DocumentRefPayload = {
  id: string;
  name: string;
  kind: string;
  version: number | null;
  created: boolean;
};

export type ChatRole = "user" | "assistant" | "system" | "tool";
/// 生成 run の状態（backend の `RunStatus` から生成）。手書きミラーには
/// `waiting_approval`（承認待ち・#350）が欠落していた。
export type RunStatus = components["schemas"]["RunStatus"];

/// 自律 run の承認モード（backend chat::AutonomousMode と一致・#350）。
/// require_approval=承認必須（既定）/ auto=版管理で復元可能な書込のみ自動 / bypass=全自動（危険）。
export type AutonomousMode = "require_approval" | "auto" | "bypass";

/// skill のバージョンピン 1 件（thread の「最初からロード済み」スキル・#344）。
export type SkillPin = { skillId: string; skillVersion: number };

export type Thread = {
  id: string;
  title: string;
  agentMode: boolean;
  /// 自律 run の承認モード（#350・実行中トグル可）。
  autonomousMode: AutonomousMode;
  /// 最初からロード済みにする skill のピン（順序付き・複数可・#344）。
  skillPins: SkillPin[];
  miniAppId?: string | null;
  miniAppVersion?: number | null;
  /// 由来ノート（ノートの分割ビューから作られたスレッド・issue #282）。通常チャットは null。
  originNoteId?: string | null;
  originNoteName?: string | null;
  createdAt: string;
  updatedAt: string;
};

/// スレッド作成時の skill / ミニアプリ選択（version 省略は current をピン）。
export type ArtifactPin = { artifactId: string; version?: number | null };

/// エージェントモードのワークスペース作成場所（Phase 6 UX）。
/// `existing`＝選んだフォルダをそのままワークスペースにする、`new_under`＝選んだ親の配下に新規作成。
export type WorkspaceChoice = {
  mode: "existing" | "new_under";
  folderId: string;
  /// 表示用のフォルダ名（送信はしない）。
  folderName: string;
};

export type Message = {
  id: string;
  role: ChatRole;
  content: ContentBlock[];
  agentMode?: boolean;
  createdAt: string;
};

export type Attachment = { node_id: string; name: string };
export type Citation = Extract<ContentBlock, { type: "citation" }>;

/// 共有語彙（backend chat::ThreadRole / storage::ShareTarget と一致）。
export type ThreadRole = "viewer" | "commenter" | "editor";
export type ShareTarget = { type: "user"; id: string } | { type: "role"; id: string };
export type ThreadShareEntry = { target: ShareTarget; role: ThreadRole };

// ── スレッド一覧の購読（サイドバー履歴）────────────────────────────────

const threadListeners = new Set<() => void>();

/// スレッド一覧が変わったことを購読者へ通知する（作成・更新時に呼ぶ）。
export function notifyThreadsChanged(): void {
  for (const l of threadListeners) l();
}

const DAY_MS = 86_400_000;
const GROUP_ORDER = ["今日", "昨日", "過去 7 日間", "それ以前"] as const;
export type ThreadGroupLabel = (typeof GROUP_ORDER)[number];

/// スレッドを更新日で「今日 / 昨日 / 過去 7 日間 / それ以前」に分ける（サイドバー共用）。
export function groupThreadsByDate(
  threads: Thread[],
  now = Date.now(),
): { label: ThreadGroupLabel; threads: Thread[] }[] {
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  const today = start.getTime();
  const yesterday = today - DAY_MS;
  const week = today - 6 * DAY_MS;
  const buckets: Record<ThreadGroupLabel, Thread[]> = {
    今日: [],
    昨日: [],
    "過去 7 日間": [],
    それ以前: [],
  };
  for (const t of threads) {
    const ts = Date.parse(t.updatedAt);
    if (ts >= today) buckets["今日"].push(t);
    else if (ts >= yesterday) buckets["昨日"].push(t);
    else if (ts >= week) buckets["過去 7 日間"].push(t);
    else buckets["それ以前"].push(t);
  }
  return GROUP_ORDER.map((label) => ({ label, threads: buckets[label] })).filter(
    (g) => g.threads.length > 0,
  );
}

/// 自分のスレッド一覧を購読する React フック（更新日降順の先頭ページ）。
/// `loading` で「取得前」と「本当に空」を区別できる（空状態フラッシュ/スケルトン用）。
export function useThreadsState(): { threads: Thread[]; loading: boolean } {
  const [threads, setThreads] = React.useState<Thread[]>([]);
  const [loading, setLoading] = React.useState(true);
  const reload = React.useCallback(() => {
    listThreads()
      .then((r) => {
        setThreads(r.threads);
        setLoading(false);
      })
      .catch(() => {
        setThreads([]);
        setLoading(false);
      });
  }, []);
  React.useEffect(() => {
    reload();
    threadListeners.add(reload);
    return () => {
      threadListeners.delete(reload);
    };
  }, [reload]);
  return { threads, loading };
}

/// スレッド一覧（配列のみ）。既存呼び出し互換のため useThreadsState を薄くラップする。
export function useThreads(): Thread[] {
  return useThreadsState().threads;
}

// ── REST ──────────────────────────────────────────────────────────────

type ApiThread = {
  id: string;
  title: string;
  agent_mode: boolean;
  autonomous_mode?: AutonomousMode;
  skill_pins?: { skill_id: string; skill_version: number }[];
  mini_app_id?: string | null;
  mini_app_version?: number | null;
  origin_note_id?: string | null;
  origin_note_name?: string | null;
  created_at: string;
  updated_at: string;
};

function toThread(t: ApiThread): Thread {
  return {
    id: t.id,
    title: t.title,
    agentMode: t.agent_mode,
    autonomousMode: t.autonomous_mode ?? "require_approval",
    skillPins: (t.skill_pins ?? []).map((p) => ({
      skillId: p.skill_id,
      skillVersion: p.skill_version,
    })),
    miniAppId: t.mini_app_id ?? null,
    miniAppVersion: t.mini_app_version ?? null,
    originNoteId: t.origin_note_id ?? null,
    originNoteName: t.origin_note_name ?? null,
    createdAt: t.created_at,
    updatedAt: t.updated_at,
  };
}

async function ok<T>(res: Response): Promise<T> {
  if (!res.ok) throw new Error(`API ${res.status}`);
  return (await res.json()) as T;
}

export async function listThreads(
  cursor?: string,
  opts?: { originNoteId?: string },
): Promise<{ threads: Thread[]; nextCursor: string | null }> {
  const params = new URLSearchParams();
  if (cursor) params.set("cursor", cursor);
  if (opts?.originNoteId) params.set("origin_note_id", opts.originNoteId);
  const qs = params.toString() ? `?${params.toString()}` : "";
  const data = await ok<{ threads: ApiThread[]; next_cursor: string | null }>(
    await apiFetch(`/threads${qs}`),
  );
  return { threads: data.threads.map(toThread), nextCursor: data.next_cursor };
}

export async function createThread(
  title?: string,
  agentMode = false,
  pins?: {
    skill?: ArtifactPin;
    /// 複数 skill（順序付き・#344）。`skill` と併用時はこちらが優先。
    skills?: ArtifactPin[];
    miniApp?: ArtifactPin;
    workspace?: WorkspaceChoice;
    /// 由来ノート（ノートの分割ビューから作るスレッド・issue #282）。
    originNoteId?: string;
  },
): Promise<Thread> {
  const toPin = (p?: ArtifactPin) =>
    p ? { artifact_id: p.artifactId, version: p.version ?? undefined } : undefined;
  const workspace = pins?.workspace
    ? { mode: pins.workspace.mode, folder_id: pins.workspace.folderId }
    : undefined;
  const skills = pins?.skills?.length ? pins.skills.map((p) => toPin(p)) : undefined;
  const data = await ok<ApiThread>(
    await apiFetch("/threads", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        title: title?.trim() || undefined,
        agent_mode: agentMode,
        skill: toPin(pins?.skill),
        skills,
        mini_app: toPin(pins?.miniApp),
        workspace,
        origin_note_id: pins?.originNoteId,
      }),
    }),
  );
  notifyThreadsChanged();
  return toThread(data);
}

/// スレッドの skill ピン集合を置き換える（owner のみ・途中変更・#344）。
/// ミニアプリ経由のスレッドはバンドル定義のピンが正のため 400 になる。
export async function setThreadSkills(threadId: string, skills: ArtifactPin[]): Promise<void> {
  const res = await apiFetch(`/threads/${threadId}/skills`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      skills: skills.map((p) => ({ artifact_id: p.artifactId, version: p.version ?? undefined })),
    }),
  });
  if (!res.ok) throw new Error(`API ${res.status}`);
  notifyThreadsChanged();
}

/// スレッドの由来ノートを設定する（下書き確定→ノート実体化の紐付け・issue #282）。
/// これでこの会話が「ノート由来」になり、ノートの会話一覧・サイドバー履歴に反映される。
export async function setThreadOriginNote(threadId: string, noteId: string): Promise<void> {
  const res = await apiFetch(`/threads/${threadId}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ note_id: noteId }),
  });
  if (!res.ok) throw new Error(`API ${res.status}`);
  notifyThreadsChanged();
}

/// スレッドの承認モードを取得する（bypass の org 許可も返す・#350）。
export async function getAutonomousMode(
  threadId: string,
): Promise<{ mode: AutonomousMode; bypassAllowed: boolean }> {
  const data = await ok<{ mode: AutonomousMode; bypass_allowed: boolean }>(
    await apiFetch(`/threads/${threadId}/autonomous-mode`),
  );
  return { mode: data.mode, bypassAllowed: data.bypass_allowed };
}

/// スレッドの承認モードを設定する（editor・実行中トグル可・#350）。
/// bypass が org ポリシで禁止されている場合は 400（明示エラー）。
export async function setAutonomousMode(
  threadId: string,
  mode: AutonomousMode,
): Promise<{ mode: AutonomousMode; bypassAllowed: boolean }> {
  const data = await ok<{ mode: AutonomousMode; bypass_allowed: boolean }>(
    await apiFetch(`/threads/${threadId}/autonomous-mode`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ mode }),
    }),
  );
  return { mode: data.mode, bypassAllowed: data.bypass_allowed };
}

export class ThreadNotFound extends Error {
  constructor() {
    super("スレッドが見つかりません");
    this.name = "ThreadNotFound";
  }
}

export async function getThread(id: string): Promise<Thread> {
  const res = await apiFetch(`/threads/${id}`);
  if (res.status === 404 || res.status === 403) throw new ThreadNotFound();
  return toThread(await ok<ApiThread>(res));
}

type ApiMessage = {
  id: string;
  role: ChatRole;
  content: ContentBlock[];
  agent_mode?: boolean;
  created_at: string;
};

export async function getThreadMessages(
  id: string,
): Promise<{
  messages: Message[];
  activeRunId: string | null;
  activeRunAutonomous: boolean;
  /// 進行中 run の生成先 assistant メッセージ id（genui アクションの照合先）。
  activeAssistantMessageId: string | null;
  /// まだ生成が始まっていない発話（順番待ち・投入順）。
  queuedRuns: { userMessageId: string; runId: string }[];
}> {
  const res = await apiFetch(`/threads/${id}/messages`);
  if (res.status === 404 || res.status === 403) throw new ThreadNotFound();
  const data = await ok<{
    messages: ApiMessage[];
    active_run_id?: string | null;
    active_run_autonomous?: boolean | null;
    active_assistant_message_id?: string | null;
    queued_runs?: { user_message_id: string; run_id: string }[] | null;
  }>(res);
  return {
    messages: data.messages.map((m) => ({
      id: m.id,
      role: m.role,
      content: m.content,
      agentMode: m.agent_mode,
      createdAt: m.created_at,
    })),
    activeRunId: data.active_run_id ?? null,
    activeRunAutonomous: data.active_run_autonomous ?? false,
    activeAssistantMessageId: data.active_assistant_message_id ?? null,
    queuedRuns: (data.queued_runs ?? []).map((q) => ({
      userMessageId: q.user_message_id,
      runId: q.run_id,
    })),
  };
}

// ── ストリーミング（SSE・replay-then-subscribe）─────────────────────────

/// 計画のサブタスク（自律エージェント・Task 5.2）。
export type PlanSubtask = { id: string; title: string; status: string };

/// skill ツールの発動記録 1 件（skill_invoked イベント・#344）。
export type SkillInvocation = {
  skill_id: string;
  skill_version: number;
  name: string;
};

/// `skill_invoked` の payload を検査する（生成型では `unknown`）。
function parseSkillInvocation(raw: unknown): SkillInvocation | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.skill_id !== "string" || typeof o.name !== "string") return null;
  if (typeof o.skill_version !== "number") return null;
  return { skill_id: o.skill_id, skill_version: o.skill_version, name: o.name };
}

/// サブエージェント委譲の記録 1 件（subagent_run イベント・#391）。
///
/// 子の生イベント（取得本文・思考）は親へ流れない。UI が出せるのは**この要約だけ**で、
/// 「どの範囲を担当し、何ステップ・何回のツール呼び出しで調べたか」を展開時に見せる。
export type SubagentRun = {
  tool_call_id: string;
  objective: string;
  boundary: string;
  steps: number;
  tool_calls: string[];
};

/// `subagent_run` の payload を検査する（生成型では `unknown`）。
function parseSubagentRun(raw: unknown): SubagentRun | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Record<string, unknown>;
  if (typeof o.tool_call_id !== "string" || typeof o.boundary !== "string") return null;
  return {
    tool_call_id: o.tool_call_id,
    objective: typeof o.objective === "string" ? o.objective : "",
    boundary: o.boundary,
    steps: typeof o.steps === "number" ? o.steps : 0,
    tool_calls: Array.isArray(o.tool_calls) ? o.tool_calls.filter((t) => typeof t === "string") : [],
  };
}

/// 承認要求（破壊系/egress/高コスト・Task 5.6）。
export type ApprovalRequest = {
  tool_call_id: string;
  name: string;
  input: unknown;
  reason: string;
};

export type StreamHandlers = {
  onToken?: (text: string) => void;
  onThinking?: (text: string) => void;
  onToolCall?: (call: {
    id: string;
    name: string;
    input: unknown;
    step?: number;
    /// サブエージェントが中継した呼び出し（#391）。親の step 空間には属さない。
    viaSubagent?: boolean;
  }) => void;
  /// ツール結果。`content` は観測テキスト（成功要約 or エラー）。UI は成否と要約を出す（#358/#386）。
  onToolResult?: (res: { id: string; ok: boolean; content: string }) => void;
  onCitation?: (c: Citation) => void;
  onFileRef?: (f: Attachment) => void;
  /// 検証済み generative UI スペック（Phase 6・emit_ui）。
  onGenerativeUi?: (spec: unknown) => void;
  /// 保存済みワークフローへの参照（Task 10.13・emit_workflow）。
  onWorkflowRef?: (workflow: unknown) => void;
  onNoteRef?: (note: unknown) => void;
  /// 未保存の下書きノート（save_note の下書き確定型・issue #282）。下書き画面を開く/流し込む。
  onNoteDraft?: (draft: unknown) => void;
  /// 未保存の下書きスライド（save_slide の下書き確定型・Task 11.3）。下書き画面を開く/流し込む。
  onSlideDraft?: (draft: unknown) => void;
  /// 未保存の下書き CSV（save_csv の下書き確定型・Task 11.11）。下書き画面を開く/流し込む。
  onCsvDraft?: (draft: unknown) => void;
  /// AI が作成/編集した文書への参照（#381）。カード化し、新規作成ならエディタへ遷移する。
  onDocumentRef?: (document: unknown) => void;
  /// skill ツールの発動記録（#344）。会話中に読み込んだスキルのチップ表示に使う。
  onSkillInvoked?: (skill: SkillInvocation) => void;
  /// サブエージェント委譲の記録（#391）。担当範囲とステップ数を展開表示に足す。
  onSubagentRun?: (run: SubagentRun) => void;
  onStatus?: (status: RunStatus) => void;
  // 自律エージェント（Phase 5）。
  onPlan?: (subtasks: PlanSubtask[]) => void;
  onBudgetWarning?: (w: { kind: string; used: number; limit: number }) => void;
  onApprovalRequested?: (req: ApprovalRequest) => void;
  onApprovalResolved?: (res: { tool_call_id: string; approved: boolean }) => void;
  onFailureRecovery?: (r: { detail: string; action: string }) => void;
  /// 生成 run_id（承認 API 呼び出しに使う）。
  onRunId?: (runId: string) => void;
  /// 生成先の assistant メッセージ id。**まだ本文は保存されていない**が、この run で出る
  /// genui カードのアクション照合先はこの id になる（run 完了後に有効になる）。
  onAssistantMessageId?: (messageId: string) => void;
  onDone?: () => void;
  onError?: (message: string) => void;
};

/// 生成イベント種別。**`crates/chat` の `StreamEventKind` から生成**した型を使う（同上）。
/// 追加 variant を握りつぶすのは `subscribe` の `default` 分岐が担う。
type StreamEventKind = components["schemas"]["StreamEventKind"];

/// SSE 購読を開始し、イベントを handlers へ振り分ける。返り値でストリームを閉じる。
function subscribe(threadId: string, handlers: StreamHandlers): () => void {
  const es = new EventSource(`/api/threads/${threadId}/stream`, { withCredentials: true });
  let closed = false;
  const finish = () => {
    if (closed) return;
    closed = true;
    es.close();
  };
  es.onmessage = (ev) => {
    let kind: StreamEventKind;
    try {
      kind = JSON.parse(ev.data) as StreamEventKind;
    } catch {
      return;
    }
    switch (kind.type) {
      case "token":
        handlers.onToken?.(kind.text);
        break;
      case "thinking":
        handlers.onThinking?.(kind.text);
        break;
      case "tool_call":
        handlers.onToolCall?.({
          id: kind.id,
          name: kind.name,
          input: kind.input,
          // 旧 run の replay では step が無い。**0 で埋めない**（逐次実行だった過去の
          // ツール群が「並行して N 件」に化ける）。不明は undefined のまま流す。
          step: kind.step ?? undefined,
          viaSubagent: kind.via_subagent ?? false,
        });
        break;
      case "tool_result":
        handlers.onToolResult?.({
          id: kind.tool_call_id,
          ok: kind.ok,
          content: kind.content,
        });
        break;
      case "citation":
        handlers.onCitation?.({
          type: "citation",
          node_id: kind.node_id,
          chunk_id: kind.chunk_id,
          snippet: kind.snippet,
          page: kind.page,
          heading_path: kind.heading_path,
          score: kind.score,
        });
        break;
      case "file_ref":
        handlers.onFileRef?.({ node_id: kind.node_id, name: kind.name });
        break;
      case "generative_ui":
        handlers.onGenerativeUi?.(kind.spec);
        break;
      case "workflow_ref":
        handlers.onWorkflowRef?.(kind.workflow);
        break;
      case "note_ref":
        handlers.onNoteRef?.(kind.note);
        break;
      case "note_draft":
        handlers.onNoteDraft?.(kind.draft);
        break;
      case "slide_draft":
        handlers.onSlideDraft?.(kind.draft);
        break;
      case "csv_draft":
        handlers.onCsvDraft?.(kind.draft);
        break;
      case "document_ref":
        handlers.onDocumentRef?.(kind.document);
        break;
      case "subagent_run": {
        // payload は serde_json::Value（生成型では unknown）。形を検査してから渡す。
        const run = parseSubagentRun(kind.subagent);
        if (run) handlers.onSubagentRun?.(run);
        break;
      }
      case "skill_invoked": {
        // payload は serde_json::Value（生成型では unknown）。形を検査してから渡す。
        const skill = parseSkillInvocation(kind.skill);
        if (skill) handlers.onSkillInvoked?.(skill);
        break;
      }
      case "plan":
        handlers.onPlan?.(kind.subtasks);
        break;
      case "budget_warning":
        handlers.onBudgetWarning?.({ kind: kind.kind, used: kind.used, limit: kind.limit });
        break;
      case "approval_requested":
        handlers.onApprovalRequested?.({
          tool_call_id: kind.tool_call_id,
          name: kind.name,
          input: kind.input,
          reason: kind.reason,
        });
        break;
      case "approval_resolved":
        handlers.onApprovalResolved?.({
          tool_call_id: kind.tool_call_id,
          approved: kind.approved,
        });
        break;
      case "failure_recovery":
        handlers.onFailureRecovery?.({ detail: kind.detail, action: kind.action });
        break;
      case "status":
        handlers.onStatus?.(kind.status);
        // キャンセル/失敗は端末状態。途中までを確定させて閉じる。
        if (kind.status === "cancelled" || kind.status === "failed") {
          handlers.onDone?.();
          finish();
        }
        break;
      case "error":
        handlers.onError?.(kind.message);
        finish();
        break;
      case "done":
        handlers.onDone?.();
        finish();
        break;
      default:
        break;
    }
  };
  // ネットワーク断は EventSource が Last-Event-ID 付きで自動再接続する（接続非依存）。
  // 端末イベントで既に閉じている場合のみ、無駄な再接続を止める。
  es.onerror = () => {
    if (closed) es.close();
  };
  return finish;
}

/// メッセージを送信し、生成イベントを SSE で受け取る（返り値で停止できる）。
/// `cancelServer=true`（明示停止）ではサーバ側もキャンセルする。ページ離脱（既定）は継続する。
export function streamMessage(
  threadId: string,
  text: string,
  attachments: Attachment[],
  handlers: StreamHandlers,
  agentMode?: boolean,
  autonomous?: boolean,
  // エディタの選択コンテキスト（選択→AI 指示・Task 11.10）。
  context?: SelectionContext,
  /// **この発話にだけ**適用する skill（スラッシュコマンド起動・#387）。
  /// thread のピンは変えない＝次の発話へ持ち越さない。
  skills?: ArtifactPin[],
): (opts?: { cancelServer?: boolean }) => void {
  let unsub: (() => void) | null = null;
  let runId: string | null = null;
  let stopped = false;

  postMessage(threadId, text, attachments, { agentMode, autonomous, context, skills })
    .then((posted) => {
      runId = posted.runId;
      // 承認 API 呼び出しのため run_id を UI へ渡す（自律プロファイル・Task 5.6）。
      handlers.onRunId?.(runId);
      handlers.onAssistantMessageId?.(posted.assistantMessageId);
      if (stopped) return;
      unsub = subscribe(threadId, handlers);
    })
    .catch((e) => handlers.onError?.(e instanceof Error ? e.message : "送信に失敗しました"));

  return (opts) => {
    stopped = true;
    unsub?.();
    if (opts?.cancelServer && runId) void cancelRun(threadId, runId);
  };
}

/// 発話を投入する（**購読しない**）。
///
/// 生成中に送った発話は「順番待ち」としてサーバが受理し、先行 run が終わってから走る
/// （直列化はワーカー側・`blocked_by_earlier_run`）。購読対象はスレッドで 1 本なので、
/// 積むだけのときは SSE を開かず、先行 run の完了後に張り直す。
export async function postMessage(
  threadId: string,
  text: string,
  attachments: Attachment[],
  opts: {
    agentMode?: boolean;
    autonomous?: boolean;
    context?: SelectionContext;
    skills?: ArtifactPin[];
  } = {},
): Promise<{ runId: string; userMessageId: string; assistantMessageId: string }> {
  const res = await apiFetch(`/threads/${threadId}/messages`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      text,
      attachments,
      context: opts.context ?? null,
      agent_mode: opts.agentMode,
      autonomous: opts.autonomous,
      skills: opts.skills?.map((p) => ({
        artifact_id: p.artifactId,
        version: p.version ?? undefined,
      })),
    }),
  });
  if (!res.ok) throw new Error(`送信に失敗しました (${res.status})`);
  const body = (await res.json()) as {
    run_id: string;
    user_message_id: string;
    assistant_message_id: string;
  };
  return {
    runId: body.run_id,
    userMessageId: body.user_message_id,
    assistantMessageId: body.assistant_message_id,
  };
}

/// 既存 run の生成イベントを購読して復元表示する（ページ再訪・POST しない）。
export function resumeMessage(threadId: string, handlers: StreamHandlers): () => void {
  return subscribe(threadId, handlers);
}

/// 生成をユーザー明示停止する（サーバ側キャンセル）。
export async function cancelRun(threadId: string, runId: string): Promise<void> {
  await apiFetch(`/threads/${threadId}/runs/${runId}/cancel`, { method: "POST" });
}

/// 自律エージェントの承認要求へ決定を下す（承認/却下・Task 5.6）。
export async function submitApproval(
  threadId: string,
  runId: string,
  decision: { toolCallId: string; toolName: string; approved: boolean },
): Promise<void> {
  await apiFetch(`/threads/${threadId}/runs/${runId}/approvals`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      tool_call_id: decision.toolCallId,
      tool_name: decision.toolName,
      approved: decision.approved,
    }),
  });
}

// ── 共有（ReBAC）───────────────────────────────────────────────────────

export async function shareThread(
  threadId: string,
  target: ShareTarget,
  role: ThreadRole,
): Promise<void> {
  const res = await apiFetch(`/threads/${threadId}/shares`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target, role }),
  });
  if (!res.ok) throw new Error(`共有に失敗しました (${res.status})`);
}

export async function unshareThread(
  threadId: string,
  target: ShareTarget,
  role: ThreadRole,
): Promise<void> {
  const res = await apiFetch(`/threads/${threadId}/shares`, {
    method: "DELETE",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target, role }),
  });
  if (!res.ok) throw new Error(`共有解除に失敗しました (${res.status})`);
}

export async function listThreadShares(threadId: string): Promise<ThreadShareEntry[]> {
  const data = await ok<{ shares: ThreadShareEntry[] }>(
    await apiFetch(`/threads/${threadId}/shares`),
  );
  return data.shares;
}

/// content-block が空（生成前のプレースホルダ）か。復元判定に使う。
export function isEmptyContent(content: ContentBlock[]): boolean {
  return content.length === 0;
}

export { newId };
