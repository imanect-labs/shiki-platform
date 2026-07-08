"use client";

import * as React from "react";

import { FileDown, Share2 } from "lucide-react";

import {
  getThread,
  getThreadMessages,
  isEmptyContent,
  notifyThreadsChanged,
  resumeMessage,
  streamMessage,
  submitApproval,
  ThreadNotFound,
  type ApprovalRequest,
  type Attachment,
  type Citation,
  type ContentBlock,
  type Message as ChatMessageT,
  type PlanSubtask,
  type RunStatus,
  type StreamHandlers,
} from "@/lib/chat-api";
import { popPending } from "@/lib/pending-message";
import { triggerDownload } from "@/lib/storage";
import { linkifyCitations } from "@/lib/citation";
import { newId } from "@/lib/chat-store";
import { Message, MessageContent } from "@/components/prompt-kit/message";
import { Loader } from "@/components/prompt-kit/loader";
import { Markdown } from "@/components/prompt-kit/markdown";
import { Sources } from "@/components/prompt-kit/source";
import { MessageFooter } from "./message-footer";
import { type ToolActivityItem } from "./tool-activity";
import { ChainOfThought } from "./chain-of-thought";
import { Composer } from "./composer";
import { ThreadShareDialog } from "./share-dialog";
import { ApprovalCard, BudgetBanner, PlanPanel } from "./agent-progress";

/// ストリーミング中のアシスタント応答の蓄積状態。
type StreamState = {
  text: string;
  thinking: string;
  tools: ToolActivityItem[];
  citations: Citation[];
  /// ツール成果物（code_interpreter が保存したファイル参照）。
  files: Attachment[];
  /// 自律エージェント（Phase 5）: 計画・承認要求・予算警告。
  plan: PlanSubtask[];
  approval: ApprovalRequest | null;
  budget: { kind: string; used: number; limit: number } | null;
  runId: string | null;
  approvalPending: boolean;
};

const EMPTY_STREAM: StreamState = {
  text: "",
  thinking: "",
  tools: [],
  citations: [],
  files: [],
  plan: [],
  approval: null,
  budget: null,
  runId: null,
  approvalPending: false,
};

export function Conversation({ threadId }: { threadId: string }) {
  const [messages, setMessages] = React.useState<ChatMessageT[]>([]);
  const [stream, setStream] = React.useState<StreamState | null>(null);
  const [notFound, setNotFound] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [agentMode, setAgentMode] = React.useState(false);
  const [autonomous, setAutonomous] = React.useState(false);
  const [shareOpen, setShareOpen] = React.useState(false);
  const bottomRef = React.useRef<HTMLDivElement | null>(null);
  // 停止関数。`cancelServer` でサーバ側もキャンセル（明示停止）。離脱は継続（呼ばない）。
  const cancelRef = React.useRef<((opts?: { cancelServer?: boolean }) => void) | null>(null);
  const sentPending = React.useRef(false);
  // 最新の stream を ref に写し、確定処理は setState updater の外で 1 度だけ行う
  // （Strict Mode で updater が二重実行され finalize が重複するのを防ぐ）。
  const streamRef = React.useRef<StreamState | null>(null);
  React.useEffect(() => {
    streamRef.current = stream;
  }, [stream]);

  // 蓄積中のストリームを確定メッセージへ移して閉じる（onDone / stop 共通）。
  const flushStream = React.useCallback(() => {
    const s = streamRef.current;
    if (s) finalizeStream(s, setMessages);
    streamRef.current = null;
    setStream(null);
  }, []);

  // 送信/復元で共通の SSE ハンドラ。蓄積を stream state に反映し、端末で確定する。
  const makeHandlers = React.useCallback((): StreamHandlers => {
    return {
      onThinking: (t) => setStream((s) => (s ? { ...s, thinking: s.thinking + t } : s)),
      onToken: (t) => setStream((s) => (s ? { ...s, text: s.text + t } : s)),
      onToolCall: (call) =>
        setStream((s) =>
          s
            ? {
                ...s,
                tools: [...s.tools, { id: call.id, name: call.name, running: true, input: call.input }],
              }
            : s,
        ),
      onToolResult: (res) =>
        setStream((s) =>
          s ? { ...s, tools: s.tools.map((t) => (t.id === res.id ? { ...t, running: false } : t)) } : s,
        ),
      onCitation: (c) => setStream((s) => (s ? { ...s, citations: [...s.citations, c] } : s)),
      onFileRef: (f) => setStream((s) => (s ? { ...s, files: [...s.files, f] } : s)),
      // --- 自律エージェント（Phase 5・Task 5.11） ---
      onRunId: (runId) => setStream((s) => (s ? { ...s, runId } : s)),
      onPlan: (subtasks) => setStream((s) => (s ? { ...s, plan: mergePlan(s.plan, subtasks) } : s)),
      onBudgetWarning: (b) => setStream((s) => (s ? { ...s, budget: b } : s)),
      onApprovalRequested: (req) =>
        setStream((s) => (s ? { ...s, approval: req, approvalPending: false } : s)),
      onApprovalResolved: (res) =>
        setStream((s) =>
          s && s.approval?.tool_call_id === res.tool_call_id
            ? { ...s, approval: null, approvalPending: false }
            : s,
        ),
      onStatus: (status: RunStatus) => {
        if (status === "cancelled") setError("生成をキャンセルしました。");
        if (status === "failed") setError("生成に失敗しました。");
      },
      onDone: () => {
        flushStream();
        cancelRef.current = null;
        notifyThreadsChanged();
      },
      onError: (msg) => {
        setError(msg);
        setStream(null);
        cancelRef.current = null;
      },
    };
  }, [flushStream]);

  const send = React.useCallback(
    (text: string, attachments: Attachment[]) => {
      setError(null);
      // 楽観的にユーザーメッセージを表示。
      const userBlocks: ContentBlock[] = [
        ...attachments.map((a) => ({ type: "file_ref" as const, node_id: a.node_id, name: a.name })),
        { type: "text" as const, text },
      ];
      setMessages((prev) => [
        ...prev,
        { id: newId(), role: "user", content: userBlocks, createdAt: new Date().toISOString() },
      ]);
      setStream({ ...EMPTY_STREAM });
      cancelRef.current = streamMessage(
        threadId,
        text,
        attachments,
        makeHandlers(),
        agentMode || autonomous,
        autonomous,
      );
    },
    [threadId, makeHandlers, agentMode, autonomous],
  );

  // 承認/却下を送る（自律エージェントのブロックを解く・Task 5.6）。
  const decideApproval = React.useCallback(
    (approved: boolean) => {
      const s = streamRef.current;
      if (!s?.approval || !s.runId) return;
      const { tool_call_id, name } = s.approval;
      setStream((prev) => (prev ? { ...prev, approvalPending: true } : prev));
      void submitApproval(threadId, s.runId, {
        toolCallId: tool_call_id,
        toolName: name,
        approved,
      }).catch((e) => setError(e instanceof Error ? e.message : "承認の送信に失敗しました"));
    },
    [threadId],
  );

  // 生成を停止する（明示停止＝サーバ側もキャンセル）。中断時点までを確定メッセージに残す。
  const stop = React.useCallback(() => {
    cancelRef.current?.({ cancelServer: true });
    cancelRef.current = null;
    flushStream();
    notifyThreadsChanged();
  }, [flushStream]);

  // 初回ロード: スレッド既定モード＋既存メッセージを取得し、進行中生成があれば復元購読する。
  React.useEffect(() => {
    let active = true;
    getThread(threadId)
      .then((t) => {
        if (active) setAgentMode(t.agentMode);
      })
      .catch(() => {});
    getThreadMessages(threadId)
      .then(({ messages: msgs, activeRunId }) => {
        if (!active) return;
        // 末尾が空の assistant プレースホルダなら生成進行中（or クラッシュ）→ 復元購読する。
        const last = msgs[msgs.length - 1];
        const resuming = last?.role === "assistant" && isEmptyContent(last.content);
        setMessages(resuming ? msgs.slice(0, -1) : msgs);
        if (resuming) {
          // 進行中 run の id を復元し、承認待ちなら承認/却下を送れるようにする（Task 5.6）。
          setStream({ ...EMPTY_STREAM, runId: activeRunId });
          cancelRef.current = resumeMessage(threadId, makeHandlers());
          return;
        }
        // 末尾が user で未応答なら（=新規スレッド直後）ホームからの pending を送る。
        if (!sentPending.current) {
          sentPending.current = true;
          const pending = popPending(threadId);
          if (pending && msgs.length === 0) {
            send(pending.text, pending.attachments);
          }
        }
      })
      .catch((e) => {
        if (!active) return;
        if (e instanceof ThreadNotFound) setNotFound(true);
        else setError(e instanceof Error ? e.message : "読み込みに失敗しました");
      });
    return () => {
      active = false;
      // ページ離脱では**サーバ側キャンセルしない**（生成は継続・再訪で復元）。SSE 購読だけ閉じる。
      cancelRef.current?.();
    };
    // send/makeHandlers は threadId 固定で安定。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [threadId]);

  React.useEffect(() => {
    bottomRef.current?.scrollIntoView({ block: "end" });
  }, [messages.length, stream?.text, stream?.thinking, stream?.tools.length]);

  if (notFound) {
    return (
      <div className="flex h-full items-center justify-center px-4">
        <p className="text-sm text-muted-foreground">この会話は見つかりませんでした。</p>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* ヘッダ: 共有 */}
      <div className="flex items-center justify-end border-b border-border/60 px-4 py-2">
        <button
          type="button"
          onClick={() => setShareOpen(true)}
          className="inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-[13px] font-medium text-foreground/70 transition-colors hover:bg-secondary hover:text-foreground"
        >
          <Share2 className="size-4" aria-hidden />
          共有
        </button>
      </div>
      <ThreadShareDialog open={shareOpen} onOpenChange={setShareOpen} threadId={threadId} />

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto flex w-full max-w-3xl flex-col gap-6 px-4 py-8">
          {messages.map((m) =>
            m.role === "user" ? (
              <UserRow key={m.id} blocks={m.content} />
            ) : (
              <AssistantRow key={m.id} blocks={m.content} />
            ),
          )}
          {stream ? <StreamingRow stream={stream} onApproval={decideApproval} /> : null}
          {error ? (
            <div className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
              {error}
            </div>
          ) : null}
          <div ref={bottomRef} />
        </div>
      </div>

      <div className="bg-background">
        <div className="mx-auto w-full max-w-3xl px-4 py-4">
          <Composer
            onSubmit={send}
            onStop={stop}
            streaming={stream !== null}
            agentMode={agentMode}
            onAgentModeChange={setAgentMode}
            autonomous={autonomous}
            onAutonomousChange={setAutonomous}
            autoFocus
          />
          <p className="mt-2 text-center text-xs text-muted-foreground">
            Shiki は社内文書を参照して回答します。誤りが含まれる場合があります。
          </p>
        </div>
      </div>
    </div>
  );
}

/// ストリーミング完了時に蓄積を確定メッセージへ変換して追加する。
function finalizeStream(
  s: StreamState,
  setMessages: React.Dispatch<React.SetStateAction<ChatMessageT[]>>,
) {
  const blocks: ContentBlock[] = [];
  // 思考は先頭に置き、完了後も「思考プロセス」として残す。
  if (s.thinking.trim()) blocks.push({ type: "thinking", text: s.thinking });
  // ツール実行履歴（検索など）も確定メッセージへ残す。AssistantRow / ChainOfThought は
  // tool_call ブロックから履歴を描画するため、これが無いと done 後に履歴が消える。
  for (const t of s.tools) blocks.push({ type: "tool_call", id: t.id, name: t.name, input: t.input });
  if (s.text.trim()) blocks.push({ type: "text", text: s.text });
  for (const c of s.citations) blocks.push(c);
  // ツール成果物（保存済みファイル）も確定メッセージへ残す。
  for (const f of s.files) blocks.push({ type: "file_ref", node_id: f.node_id, name: f.name });
  if (blocks.length === 0) return;
  setMessages((prev) => [
    ...prev,
    { id: newId(), role: "assistant", content: blocks, createdAt: new Date().toISOString() },
  ]);
}

// ── 行レンダリング ───────────────────────────────────────────────────

function UserRow({ blocks }: { blocks: ContentBlock[] }) {
  const text = blocks
    .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
    .map((b) => b.text)
    .join("\n");
  const files = blocks.filter((b): b is Extract<ContentBlock, { type: "file_ref" }> => b.type === "file_ref");
  return (
    <Message className="justify-end">
      <div className="flex max-w-[85%] flex-col items-end gap-1.5">
        {files.length > 0 ? (
          <div className="flex flex-wrap justify-end gap-1.5">
            {files.map((f) => (
              <span
                key={f.node_id}
                className="inline-flex items-center gap-1 rounded-full border border-border bg-card px-2.5 py-1 text-[12px] text-foreground/80"
              >
                📎 {f.name}
              </span>
            ))}
          </div>
        ) : null}
        {text ? (
          <MessageContent className="rounded-2xl bg-secondary px-4 py-2.5 text-[15px] leading-relaxed text-secondary-foreground">
            {text}
          </MessageContent>
        ) : null}
      </div>
    </Message>
  );
}

function AssistantRow({ blocks }: { blocks: ContentBlock[] }) {
  const thinking = blocks
    .filter((b): b is Extract<ContentBlock, { type: "thinking" }> => b.type === "thinking")
    .map((b) => b.text)
    .join("");
  const text = blocks
    .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
    .map((b) => b.text)
    .join("");
  const tools: ToolActivityItem[] = blocks
    .filter((b): b is Extract<ContentBlock, { type: "tool_call" }> => b.type === "tool_call")
    .map((b) => ({ id: b.id, name: b.name, running: false, input: b.input }));
  const citations = blocks.filter((b): b is Citation => b.type === "citation");
  const files = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "file_ref" }> => b.type === "file_ref",
  );

  return (
    <Message className="group justify-start">
      <div className="w-full min-w-0">
        <ChainOfThought thinking={thinking} tools={tools} citations={citations} />
        {text ? <Markdown>{linkifyCitations(text, citations)}</Markdown> : null}
        <ArtifactFiles files={files} />
        <Sources citations={citations} />
        {text ? <MessageFooter text={text} /> : null}
      </div>
    </Message>
  );
}

/// ツール成果物（保存済みファイル）のチップ列。クリックでダウンロードする。
function ArtifactFiles({ files }: { files: { node_id: string; name: string }[] }) {
  if (files.length === 0) return null;
  return (
    <div className="mt-2 flex flex-wrap gap-1.5">
      {files.map((f) => (
        <button
          key={f.node_id}
          type="button"
          onClick={() => void triggerDownload(f.node_id)}
          title={`${f.name} をダウンロード`}
          className="inline-flex items-center gap-1.5 rounded-full border border-border bg-card px-2.5 py-1 text-[12px] text-foreground/80 transition-colors hover:border-primary/40 hover:bg-secondary hover:text-foreground"
        >
          <FileDown className="size-3.5 text-primary" aria-hidden />
          {f.name}
        </button>
      ))}
    </div>
  );
}

function StreamingRow({
  stream,
  onApproval,
}: {
  stream: StreamState;
  onApproval: (approved: boolean) => void;
}) {
  const showLoader =
    !stream.text &&
    !stream.thinking &&
    stream.tools.length === 0 &&
    stream.plan.length === 0 &&
    !stream.approval;
  return (
    <Message className="justify-start">
      <div className="w-full min-w-0 space-y-2">
        {stream.plan.length > 0 ? <PlanPanel subtasks={stream.plan} /> : null}
        <ChainOfThought
          thinking={stream.thinking}
          tools={stream.tools}
          citations={stream.citations}
          streaming={!stream.text}
        />
        {stream.budget ? <BudgetBanner {...stream.budget} /> : null}
        {stream.approval ? (
          <ApprovalCard
            request={stream.approval}
            pending={stream.approvalPending}
            onDecision={onApproval}
          />
        ) : null}
        {showLoader ? (
          <MessageContent className="py-1">
            <Loader variant="typing" />
          </MessageContent>
        ) : stream.text ? (
          <div className="text-[15px] leading-relaxed">
            <Markdown>{linkifyCitations(stream.text, stream.citations)}</Markdown>
          </div>
        ) : null}
        <ArtifactFiles files={stream.files} />
      </div>
    </Message>
  );
}

/// 計画イベントを蓄積する。フル計画（全 title 非空）は置換、単一の空 title は id で status 更新。
function mergePlan(prev: PlanSubtask[], incoming: PlanSubtask[]): PlanSubtask[] {
  const isStatusOnly = incoming.length === 1 && incoming[0].title === "";
  if (!isStatusOnly) return incoming;
  const upd = incoming[0];
  return prev.map((s) => (s.id === upd.id ? { ...s, status: upd.status } : s));
}
