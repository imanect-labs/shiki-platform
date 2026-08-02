"use client";

import * as React from "react";

import { useRouter } from "next/navigation";
import { AlertTriangle, Clock3, FileDown, LayoutGrid, Sparkles } from "lucide-react";

import {
  cancelRun,
  getAutonomousMode,
  getThreadMessages,
  postMessage,
  isEmptyContent,
  notifyThreadsChanged,
  resumeMessage,
  setAutonomousMode,
  streamMessage,
  submitApproval,
  ThreadNotFound,
  type ApprovalRequest,
  type AutonomousMode,
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
import { selectionKindLabel, type SelectionContext } from "@/lib/selection-context";
import type { ActiveCommand } from "@/lib/slash-command";
import { Message, MessageContent } from "@/components/prompt-kit/message";
import { ChatGenUiProvider } from "@/components/genui/action-context";
import { invokeChatUiAction, UiActionAlreadyInvoked } from "@/lib/artifact-api";
import { SpecRenderer } from "@/components/genui/spec-renderer";
import { SaveAsAppDialog, specHasChatOnlyAction } from "@/components/artifacts/save-as-app-dialog";
import { Loader } from "@/components/prompt-kit/loader";
import { Markdown } from "@/components/prompt-kit/markdown";
import { Sources } from "@/components/prompt-kit/source";
import { MessageFooter } from "./message-footer";
import { type ToolActivityItem } from "./tool-activity";
import { ChainOfThought } from "./chain-of-thought";
import { Composer } from "./composer";
import { WorkflowRefCard } from "./workflow-ref-card";
import { NoteRefCard } from "./note-ref-card";
import { NoteDraftCard } from "./note-draft-card";
import { SlideDraftCard } from "./slide-draft-card";
import { CsvDraftCard } from "./csv-draft-card";
import { DocumentRefCard, LegacyDocumentDraftCard, parseDocumentRef, documentRefHref } from "./document-ref-card";
import { upsertDraft, parseNoteDraft } from "@/lib/notes/draft-store";
import { draftHref } from "@/lib/notes/draft-nav";
import { parseSlideDraft, slideDraftHref, slideDraftStore } from "@/lib/slides/draft";
import { csvDraftHref, csvDraftStore, parseCsvDraft } from "@/lib/csv/draft";
import { ThreadShareDialog } from "./share-dialog";
import { ChatPageHeaderSlot } from "./chat-header-actions";
import { ApprovalCard, BudgetBanner, PlanPanel } from "./agent-progress";
import { cn } from "@/lib/utils";

/// ストリーミング中のアシスタント応答の蓄積状態。
type StreamState = {
  text: string;
  thinking: string;
  tools: ToolActivityItem[];
  citations: Citation[];
  /// ツール成果物（code_interpreter が保存したファイル参照）。
  files: Attachment[];
  /// 検証済み generative UI スペック（Phase 6・emit_ui）。
  uiSpecs: unknown[];
  /// 保存済みワークフロー参照（Task 10.13・emit_workflow）。
  workflowRefs: unknown[];
  /// 保存済みノート参照（Task 11P.5・save_note）。
  noteRefs: unknown[];
  /// 未保存の下書きノート（issue #282・save_note の下書き確定型）。
  noteDrafts: unknown[];
  /// 未保存の下書きスライド（Task 11.3・save_slide の下書き確定型）。
  slideDrafts: unknown[];
  /// 未保存の下書き CSV（Task 11.11・save_csv の下書き確定型）。
  csvDrafts: unknown[];
  /// AI が作成/編集した文書の参照（#381・save_document / save_sheet / 各編集ツール）。
  documentRefs: unknown[];
  /// 自律エージェント（Phase 5）: 計画・承認要求・予算警告。
  plan: PlanSubtask[];
  approval: ApprovalRequest | null;
  budget: { kind: string; used: number; limit: number } | null;
  runId: string | null;
  /// 生成先の assistant メッセージ id（genui アクションの照合先）。run 完了までは
  /// **保存されていない**ので実行はできないが、id は POST 応答で分かっている。
  assistantMessageId: string | null;
  approvalPending: boolean;
};

/// 順番待ちの発話（サーバは受理済み・生成がまだ始まっていない）。
///
/// 生成中でも発話は投入でき、サーバが 1 スレッド 1 本ずつ直列化する。ここに積まれる間も
/// **メッセージはサーバに存在する**ので、ページを離れても消えない（再訪で復元される）。
type QueuedMessage = {
  /// 描画キー（サーバの user メッセージ id。POST 前の楽観表示中は仮 id）。
  key: string;
  blocks: ContentBlock[];
  /// 取り消し用（投入前は null）。
  runId: string | null;
  /// 投入自体に失敗したとき（この行だけ赤くして再送を促す）。
  error: string | null;
};

const EMPTY_STREAM: StreamState = {
  text: "",
  thinking: "",
  tools: [],
  citations: [],
  files: [],
  uiSpecs: [],
  workflowRefs: [],
  noteRefs: [],
  noteDrafts: [],
  slideDrafts: [],
  csvDrafts: [],
  documentRefs: [],
  plan: [],
  approval: null,
  budget: null,
  assistantMessageId: null,
  runId: null,
  approvalPending: false,
};

export function Conversation({
  threadId,
  variant = "page",
  onNoteDraftOpened,
  onSlideDraftOpened,
  onCsvDraftOpened,
  onDocumentCreated,
}: {
  threadId: string;
  /// "page"=/c/[id] 単独表示（統一ヘッダにタイトル/共有/設定を注入・幅は max-w-3xl 中央）。
  /// "panel"=ノート分割ビューの埋め込み（自前ヘッダ無し・幅いっぱい・免責は凝縮）。
  variant?: "page" | "panel";
  /// save_note の下書き（note_draft）を受けたときの導線（issue #282）。渡されない場合は
  /// 下書きノート画面へ遷移する（主線）。下書き画面自身は自前で受けて遷移せずアクティブ切替する。
  onNoteDraftOpened?: (name: string) => void;
  /// save_slide の下書き（slide_draft）を受けたときの導線（Task 11.3・note と同型）。
  onSlideDraftOpened?: (name: string) => void;
  /// save_csv の下書き（csv_draft）を受けたときの導線（Task 11.11・note と同型）。
  onCsvDraftOpened?: (name: string) => void;
  /// AI が Office 文書を**新規作成**したときの導線（#381）。渡されない場合は
  /// 作成された文書（Collabora）へ遷移する（主線）。編集の参照では発火しない。
  onDocumentCreated?: (href: string) => void;
}) {
  const isPanel = variant === "panel";
  const router = useRouter();
  const [messages, setMessages] = React.useState<ChatMessageT[]>([]);
  const [stream, setStream] = React.useState<StreamState | null>(null);
  const [notFound, setNotFound] = React.useState(false);
  // 作成した文書の遷移先。**run 完了後**に遷移する（#381）: document_ref はツール結果直後に
  // 届くため、その場で router.push すると Conversation がアンマウントされて SSE 購読が閉じ、
  // 複合依頼の後続ツールの承認カードが出せず run が承認待ちで止まる。
  const pendingOpenRef = React.useRef<string | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  // 実行への注意喚起（承認モードのクランプ等・#350）。エラーではないが黙らせない。
  const [notice, setNotice] = React.useState<string | null>(null);
  // エージェントモード（＝Autonomous・ワークスペース＋計画＋承認）。通常チャットでもツールは
  // モデル裁量で自動発火する（issue #102）ため、旧「自動」トグルは廃止した。
  const [autonomous, setAutonomous] = React.useState(false);
  // 承認モード（承認必須/オート/全自動・#350）。thread から復元し、実行中も切替可。
  // null=未ロード（セレクタ非表示）。bypass の可否は org 管理者ポリシ。
  const [approvalMode, setApprovalModeState] = React.useState<AutonomousMode | null>(null);
  const [bypassAllowed, setBypassAllowed] = React.useState(true);
  // PUT を直列化するチェーン（連打時の応答順逆転で危険な実効ポリシを誤表示しない）。
  // 各成功応答のサーバ確定値で状態を上書きし、失敗時はサーバの真実を読み直して収束させる。
  const modeRequestChain = React.useRef<Promise<unknown>>(Promise.resolve());
  const changeApprovalMode = React.useCallback(
    (mode: AutonomousMode) => {
      setApprovalModeState(mode); // 楽観更新（確定/失敗時にサーバ値で上書き）
      modeRequestChain.current = modeRequestChain.current
        .then(() => setAutonomousMode(threadId, mode))
        .then((r) => {
          setApprovalModeState(r.mode);
          setBypassAllowed(r.bypassAllowed);
        })
        .catch(async () => {
          setError("承認モードの変更に失敗しました（組織ポリシで禁止されている可能性があります）");
          try {
            const r = await getAutonomousMode(threadId);
            setApprovalModeState(r.mode);
            setBypassAllowed(r.bypassAllowed);
          } catch {
            /* 読み直し失敗時は現状表示を維持（次の操作で再同期） */
          }
        });
    },
    [threadId],
  );
  const [shareOpen, setShareOpen] = React.useState(false);
  // ヘッダスロットへ渡すため安定した参照にする（streaming の毎レンダーで再注入しない）。
  const openShare = React.useCallback(() => setShareOpen(true), []);
  // UI アクション（chat.submit 等）成功後に会話を再読込するためのキー。
  const [reloadKey, setReloadKey] = React.useState(0);
  // 順番待ちの発話（サーバ受理済み・生成未開始）。ストリーム行の下に並べる。
  const [queued, setQueued] = React.useState<QueuedMessage[]>([]);
  // 生成中に押された genui アクション（質問カードの回答・計画の承認）。生成が終わって
  // assistant メッセージが保存された瞬間に、押された順で実行する。
  const queuedActionsRef = React.useRef<
    { messageId: string; actionId: string; params: unknown }[]
  >([]);
  // onDone は makeHandlers の中で閉じ込められるため、最新の件数を ref で見る。
  const queuedCountRef = React.useRef(0);
  React.useEffect(() => {
    queuedCountRef.current = queued.length;
  }, [queued]);
  const bottomRef = React.useRef<HTMLDivElement | null>(null);
  // 停止関数。`cancelServer` でサーバ側もキャンセル（明示停止）。離脱は継続（呼ばない）。
  const cancelRef = React.useRef<((opts?: { cancelServer?: boolean }) => void) | null>(null);
  const sentPending = React.useRef(false);
  // stream の正は ref に置き、state は描画用の写しとする。setState updater 経由の
  // 遅延同期（useEffect）だと、SSE が同一マイクロタスクで連続到達したとき ref が
  // 1 レンダー分古いまま onDone に入り、最後のバッチ（generative_ui 等）が確定から
  // 欠落する（stub LLM の高速バーストで顕在化）。ref を同期更新して確実に拾う。
  const streamRef = React.useRef<StreamState | null>(null);
  const updateStream = React.useCallback(
    (fn: (s: StreamState | null) => StreamState | null) => {
      streamRef.current = fn(streamRef.current);
      setStream(streamRef.current);
    },
    [],
  );

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
      onThinking: (t) => updateStream((s) => (s ? { ...s, thinking: s.thinking + t } : s)),
      onToken: (t) => updateStream((s) => (s ? { ...s, text: s.text + t } : s)),
      onToolCall: (call) =>
        updateStream((s) =>
          s
            ? {
                ...s,
                // ステップの区切りで段落を落とす。ツールを挟んだ発話は別の思考なので、
                // 繋げて書くと「〜します。〜しました。〜します。」が 1 段落に連なって読めない。
                text: paragraphBreak(s.text),
                tools: [
                  ...s.tools,
                  {
                    // この出現の一意キー（呼び出し ID はループで再利用され得る）。
                    key: newId(),
                    id: call.id,
                    name: call.name,
                    running: true,
                    input: call.input,
                    step: call.step,
                    viaSubagent: call.viaSubagent,
                  },
                ],
              }
            : s,
        ),
      // 成否と観測テキストを保持する（失敗を成功と同じ見た目にしない・#358/#386）。
      // **同じ id が複数ステップで再利用され得る**（stub の `loop:` は毎ステップ
      // `stubtool_1` を出す）。全件更新すると過去行の成否まで上書きされるため、
      // 同一 id の中で**まだ実行中の最初の 1 件**にだけ結果を対応付ける。
      onToolResult: (res) =>
        updateStream((s) => {
          if (!s) return s;
          const i = s.tools.findIndex((t) => t.id === res.id && t.running);
          if (i < 0) return s;
          const tools = s.tools.slice();
          tools[i] = { ...tools[i], running: false, ok: res.ok, result: res.content };
          return { ...s, tools };
        }),
      // skill ツールの発動記録（#344）。対応する skill 呼び出しへ版を添えて「どの版を読んだか」を出す。
      // skill_invoked に tool_call_id が無いため名前で突き合わせるが、**同じスキルを複数回
      // 読み込み得る**ので全件更新はしない（後の版が過去行にも付く）。まだ版が付いていない
      // 最初の 1 件へ FIFO で対応付ける。イベントは projection 対象外＝ライブ限定の付加情報。
      onSkillInvoked: (skill) =>
        updateStream((s) => {
          if (!s) return s;
          const i = s.tools.findIndex(
            (t) =>
              t.name === "skill" &&
              t.skillVersion === undefined &&
              skillNameOf(t.input) === skill.name,
          );
          if (i < 0) return s;
          const tools = s.tools.slice();
          tools[i] = { ...tools[i], skillVersion: skill.skill_version };
          return { ...s, tools };
        }),
      // サブエージェント委譲の要約（#391）。`tool_call_id` で一意に突き合わせる
      // （skill_invoked と違い id が載るので FIFO 推測が要らない）。
      onSubagentRun: (run) =>
        updateStream((s) => {
          if (!s) return s;
          const i = s.tools.findIndex((t) => t.id === run.tool_call_id);
          if (i < 0) return s;
          const tools = s.tools.slice();
          tools[i] = {
            ...tools[i],
            subagent: {
              boundary: run.boundary,
              steps: run.steps,
              toolCalls: run.tool_calls.length,
            },
          };
          return { ...s, tools };
        }),
      onCitation: (c) => updateStream((s) => (s ? { ...s, citations: [...s.citations, c] } : s)),
      onFileRef: (f) => updateStream((s) => (s ? { ...s, files: [...s.files, f] } : s)),
      onGenerativeUi: (spec) =>
        updateStream((s) => (s ? { ...s, uiSpecs: [...s.uiSpecs, spec] } : s)),
      onWorkflowRef: (workflow) =>
        updateStream((s) =>
          s ? { ...s, workflowRefs: [...s.workflowRefs, workflow] } : s,
        ),
      onNoteRef: (note) =>
        updateStream((s) => (s ? { ...s, noteRefs: [...s.noteRefs, note] } : s)),
      onNoteDraft: (raw) => {
        // 下書きをストリームに残しつつ、クライアント下書きストアへ upsert（source=ai＝流し込み）。
        // 主線は下書きノート画面へ遷移。下書き画面自身は onNoteDraftOpened を渡してアクティブ切替のみ。
        updateStream((s) => (s ? { ...s, noteDrafts: [...s.noteDrafts, raw] } : s));
        const d = parseNoteDraft(raw);
        if (!d) return;
        upsertDraft(threadId, d.name, d.markdown, "ai");
        if (onNoteDraftOpened) onNoteDraftOpened(d.name);
        else router.push(draftHref(threadId, d.name));
      },
      onSlideDraft: (raw) => {
        // note_draft と同型: ストリームに残しつつ下書きストアへ upsert（source=ai＝流し込み）。
        // 主線は下書きスライド画面へ遷移。下書き画面自身はアクティブ切替のみ。
        updateStream((s) => (s ? { ...s, slideDrafts: [...s.slideDrafts, raw] } : s));
        const d = parseSlideDraft(raw);
        if (!d) return;
        slideDraftStore.upsert(threadId, d.name, d.content, "ai");
        if (onSlideDraftOpened) onSlideDraftOpened(d.name);
        else router.push(slideDraftHref(threadId, d.name));
      },
      onCsvDraft: (raw) => {
        // note_draft と同型: ストリームに残しつつ下書きストアへ upsert（source=ai＝流し込み）。
        updateStream((s) => (s ? { ...s, csvDrafts: [...s.csvDrafts, raw] } : s));
        const d = parseCsvDraft(raw);
        if (!d) return;
        csvDraftStore.upsert(threadId, d.name, d.csv, "ai");
        if (onCsvDraftOpened) onCsvDraftOpened(d.name);
        else router.push(csvDraftHref(threadId, d.name));
      },
      onDocumentRef: (raw) => {
        updateStream((s) => (s ? { ...s, documentRefs: [...s.documentRefs, raw] } : s));
        const doc = parseDocumentRef(raw);
        // 遷移するのは**新規作成**のときだけ（編集で遷移すると会話中のユーザーを勝手に
        // 画面外へ連れて行くことになる）。作成は承認済み＝ユーザーが意図した操作。
        if (!doc || !doc.created) return;
        const href = documentRefHref(doc);
        if (!href) return; // 専用エディタが無い種別（カードのみ）。
        // 最後に作られたものを開く（複数作った場合の直観に合わせる）。
        pendingOpenRef.current = href;
      },
      // --- 自律エージェント（Phase 5・Task 5.11） ---
      onRunId: (runId) => updateStream((s) => (s ? { ...s, runId } : s)),
      onAssistantMessageId: (assistantMessageId) =>
        updateStream((s) => (s ? { ...s, assistantMessageId } : s)),
      onPlan: (subtasks) =>
        updateStream((s) => (s ? { ...s, plan: mergePlan(s.plan, subtasks) } : s)),
      onBudgetWarning: (b) => updateStream((s) => (s ? { ...s, budget: b } : s)),
      onApprovalRequested: (req) =>
        updateStream((s) => (s ? { ...s, approval: req, approvalPending: false } : s)),
      onApprovalResolved: (res) =>
        updateStream((s) =>
          s && s.approval?.tool_call_id === res.tool_call_id
            ? { ...s, approval: null, approvalPending: false }
            : s,
        ),
      // 承認モードのクランプ（org 禁止・他人による緩和）は明示的に知らせる（黙って降格しない・#350）。
      onFailureRecovery: (r) => {
        if (r.action === "mode_clamped") setNotice(r.detail);
      },
      onStatus: (status: RunStatus) => {
        if (status === "cancelled") setError("生成をキャンセルしました。");
        if (status === "failed") setError("生成に失敗しました。");
        // 中断した run の保留遷移は捨てる（次の run の完了時に横取りされないように）。
        if (status === "cancelled" || status === "failed") pendingOpenRef.current = null;
      },
      onDone: () => {
        // generative UI を含む応答はサーバ確定の message id が要る（UI アクションの照合先）
        // ため、ローカル確定ではなく再読込で置き換える。
        const hadUi = (streamRef.current?.uiSpecs.length ?? 0) > 0;
        flushStream();
        cancelRef.current = null;
        notifyThreadsChanged();
        // 生成中に押されたカード操作を、保存済みになった今この瞬間に流す（押した順）。
        const actions = queuedActionsRef.current.splice(0);
        if (actions.length > 0) {
          void (async () => {
            for (const a of actions) {
              try {
                await invokeChatUiAction(threadId, a.messageId, a.actionId, a.params);
              } catch (e) {
                // 既に送信済み（二重に積まれた・別タブから押した）はエラーにしない。
                // 再読込でカードが送信済み表示になるので、それが答えになる（#410）。
                if (e instanceof UiActionAlreadyInvoked) continue;
                setError(e instanceof Error ? e.message : "回答の送信に失敗しました");
              }
            }
            setReloadKey((k) => k + 1);
          })();
          return;
        }
        // 順番待ちがあるなら読み直す: サーバは次の run を「いま映すべき run」として返すので、
        // 再ロードがそのまま次への購読の張り直しになる（クライアントで順序を持たない）。
        if (hadUi || queuedCountRef.current > 0) setReloadKey((k) => k + 1);
        // 作成した文書は run が終わってから開く（途中で遷移すると SSE が切れる・#381）。
        const href = pendingOpenRef.current;
        pendingOpenRef.current = null;
        if (!href) return;
        if (onDocumentCreated) onDocumentCreated(href);
        else router.push(href);
      },
      onError: (msg) => {
        setError(msg);
        // 失敗した run の途中成果物へ勝手に飛ばさない（会話に留めてユーザーに判断させる）。
        pendingOpenRef.current = null;
        streamRef.current = null;
        setStream(null);
        cancelRef.current = null;
      },
    };
  }, [
    flushStream,
    updateStream,
    threadId,
    onNoteDraftOpened,
    onSlideDraftOpened,
    onCsvDraftOpened,
    onDocumentCreated,
    router,
  ]);

  const send = React.useCallback(
    (
      text: string,
      attachments: Attachment[],
      autonomousOverride?: boolean,
      // エディタの選択コンテキスト（選択→AI 指示・Task 11.10）。
      context?: SelectionContext,
      // この発話にだけ適用する skill（スラッシュコマンド・#387）。
      onceSkills?: { artifactId: string; version?: number | null }[],
    ) => {
      setError(null);
      setNotice(null);
      // ホームからの初回メッセージは選択時点の値を明示指定する（state 初期化のタイミングに依存しない）。
      const runAutonomous = autonomousOverride ?? autonomous;
      // 楽観的にユーザーメッセージを表示。
      const userBlocks: ContentBlock[] = [
        ...(context ? [{ type: "selection_context" as const, context }] : []),
        ...attachments.map((a) => ({ type: "file_ref" as const, node_id: a.node_id, name: a.name })),
        { type: "text" as const, text },
      ];
      // 生成中でも送れる（サーバが直列化する）。購読はスレッドで 1 本なので、走っている run が
      // ある間は SSE を張らずに「順番待ち」へ積み、その run が終わってから張り直す。
      if (streamRef.current) {
        const key = newId();
        setQueued((prev) => [...prev, { key, blocks: userBlocks, runId: null, error: null }]);
        void postMessage(threadId, text, attachments, {
          agentMode: runAutonomous,
          autonomous: runAutonomous,
          context,
          skills: onceSkills,
        })
          .then((posted) =>
            setQueued((prev) =>
              prev.map((q) => (q.key === key ? { ...q, runId: posted.runId } : q)),
            ),
          )
          .catch((e) =>
            setQueued((prev) =>
              prev.map((q) =>
                q.key === key
                  ? { ...q, error: e instanceof Error ? e.message : "送信に失敗しました" }
                  : q,
              ),
            ),
          );
        return;
      }
      setMessages((prev) => [
        ...prev,
        { id: newId(), role: "user", content: userBlocks, createdAt: new Date().toISOString() },
      ]);
      streamRef.current = { ...EMPTY_STREAM };
      setStream(streamRef.current);
      cancelRef.current = streamMessage(
        threadId,
        text,
        attachments,
        makeHandlers(),
        runAutonomous,
        runAutonomous,
        context,
        onceSkills,
      );
    },
    [threadId, makeHandlers, autonomous],
  );

  /// 生成中に押された genui アクションを積む（生成完了時に onDone が流す）。
  ///
  /// 送り先は**この run の生成先 assistant メッセージ**。ストリーム中に id が分かっている
  /// （POST 応答 or 再訪時の active_assistant_message_id）ので、確定を待つのは保存だけ。
  const onQueueUiAction = React.useCallback((actionId: string, params: unknown) => {
    const messageId = streamRef.current?.assistantMessageId;
    if (!messageId) {
      setError("いまは回答を受け付けられませんでした。生成が終わってからもう一度お試しください。");
      return;
    }
    queuedActionsRef.current.push({ messageId, actionId, params });
  }, []);

  /// 順番待ちを取り消す（サーバの run もキャンセルする＝離脱後に走り出さない）。
  const cancelQueued = React.useCallback(
    (item: QueuedMessage) => {
      setQueued((prev) => prev.filter((q) => q.key !== item.key));
      if (item.runId) void cancelRun(threadId, item.runId);
    },
    [threadId],
  );

  /// コンポーザからの送信。スラッシュコマンド確定時は **その発話にだけ** skill を適用し、
  /// 長ホライズン（エージェントモード）で送る（#387）。
  ///
  /// ピンはしない: ピンは「最初からロード済み」の永続設定で、コマンドのたびに積み上がると
  /// instructions がコンテキストを食い、指示同士が矛盾する（human 判断・2026-07-29）。
  /// 通常チャットは `max_steps=6` なので、質問→計画→調査のような多段の作法は自律で走らせる。
  const submitFromComposer = React.useCallback(
    (
      text: string,
      attachments: Attachment[],
      context?: SelectionContext,
      command?: ActiveCommand,
    ) => {
      if (!command) {
        send(text, attachments, undefined, context);
        return;
      }
      send(text, attachments, true, context, [
        { artifactId: command.skillId, version: command.skillVersion },
      ]);
    },
    [send],
  );


  // 承認/却下を送る（自律エージェントのブロックを解く・Task 5.6）。
  const decideApproval = React.useCallback(
    (approved: boolean) => {
      const s = streamRef.current;
      if (!s?.approval || !s.runId) return;
      const { tool_call_id, name } = s.approval;
      updateStream((prev) => (prev ? { ...prev, approvalPending: true } : prev));
      void submitApproval(threadId, s.runId, {
        toolCallId: tool_call_id,
        toolName: name,
        approved,
      }).catch((e) => setError(e instanceof Error ? e.message : "承認の送信に失敗しました"));
    },
    [threadId, updateStream],
  );

  // 生成を停止する（明示停止＝サーバ側もキャンセル）。中断時点までを確定メッセージに残す。
  const stop = React.useCallback(() => {
    cancelRef.current?.({ cancelServer: true });
    cancelRef.current = null;
    flushStream();
    notifyThreadsChanged();
  }, [flushStream]);

  // 初回ロード: 既存メッセージを取得し、進行中生成があれば復元購読する。
  // エージェントモードのトグルは thread.agent_mode から復元しない — 旧「エージェント」（Chat）
  // トグルで作られたスレッドは agent_mode=true でも自律ではないため、復元すると誤って自律へ
  // 昇格してしまう（agent_mode と autonomous は別物・Codex 指摘）。既定 OFF で始め、ホーム由来の
  // 初回メッセージのみ pending の値で送る。
  // 承認モードの復元（#350）。メッセージ取得と独立に引く（失敗してもチャットは使える）。
  React.useEffect(() => {
    let active = true;
    getAutonomousMode(threadId)
      .then((r) => {
        if (!active) return;
        setApprovalModeState(r.mode);
        setBypassAllowed(r.bypassAllowed);
      })
      .catch(() => {
        /* セレクタ非表示のまま（chat 無効・権限なし等） */
      });
    return () => {
      active = false;
    };
  }, [threadId]);

  React.useEffect(() => {
    let active = true;
    getThreadMessages(threadId)
      .then(
        ({
          messages: msgs,
          activeRunId,
          activeRunAutonomous,
          activeAssistantMessageId,
          queuedRuns,
        }) => {
        if (!active) return;
        // 空の assistant は**生成先のプレースホルダ**（進行中・順番待ち・中断のいずれか）。
        // 描画するものが無く、順番待ちがあると末尾以外にも現れるので一律に落とす。
        const shown = msgs.filter((m) => !(m.role === "assistant" && isEmptyContent(m.content)));
        // 順番待ちの発話はストリーム行の下へ回す（会話の並びとして「AI が答えている最中に
        // 積んだ次の発話」が後ろに来るのが自然）。
        const queuedRunOf = new Map(queuedRuns.map((q) => [q.userMessageId, q.runId]));
        setQueued(
          shown
            .filter((m) => queuedRunOf.has(m.id))
            .map((m) => ({
              key: m.id,
              blocks: m.content,
              runId: queuedRunOf.get(m.id) ?? null,
              error: null,
            })),
        );
        setMessages(shown.filter((m) => !queuedRunOf.has(m.id)));
        const resuming = activeRunId !== null;
        if (resuming) {
          // 進行中 run の id を復元し、承認待ちなら承認/却下を送れるようにする（Task 5.6）。
          // 自律 run なら再訪時もエージェントモード UI（承認モードセレクタ含む）を復元する
          // （承認待ちの run に対して実行中トグルを見えるようにする・#350）。
          if (activeRunAutonomous) setAutonomous(true);
          streamRef.current = {
            ...EMPTY_STREAM,
            runId: activeRunId,
            assistantMessageId: activeAssistantMessageId,
          };
          setStream(streamRef.current);
          cancelRef.current = resumeMessage(threadId, makeHandlers());
          return;
        }
        // 末尾が user で未応答なら（=新規スレッド直後）ホームからの pending を送る。
        if (!sentPending.current) {
          sentPending.current = true;
          const pending = popPending(threadId);
          if (pending && msgs.length === 0) {
            // ホームで選んだエージェントモードを初回メッセージへ反映し、トグル表示も合わせる。
            if (pending.autonomous) setAutonomous(true);
            send(
              pending.text,
              pending.attachments,
              pending.autonomous ?? false,
              undefined,
              pending.skills,
            );
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
  }, [threadId, reloadKey]);

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
      {/* page: 統一ヘッダスロットへタイトル＋共有/設定を注入（横バー二重を解消）。
          panel: 注入しない（分割ビューは自前ヘッダを持つ・null を返すだけ）。 */}
      {!isPanel ? <ChatPageHeaderSlot title="会話" onShare={openShare} /> : null}
      <ThreadShareDialog open={shareOpen} onOpenChange={setShareOpen} threadId={threadId} />

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div
          className={cn(
            "mx-auto flex w-full flex-col gap-6 px-4 py-8",
            isPanel ? "max-w-none px-4 py-5" : "max-w-3xl",
          )}
        >
          {/* パネルの空会話は「何ができるか」を軽く案内する（空白のままにしない）。 */}
          {isPanel && messages.length === 0 && !stream && !notFound && !error ? (
            <div
              className="flex flex-col items-center gap-2 px-6 py-14 text-center"
              data-testid="panel-empty-hint"
            >
              <Sparkles className="size-6 text-muted-foreground/70" aria-hidden />
              <p className="text-sm font-medium text-foreground">
                このドキュメントについて AI に相談できます
              </p>
              <p className="max-w-xs text-xs leading-relaxed text-muted-foreground">
                質問・要約・編集の依頼ができます。本文を選択して「AI
                に依頼」を押すと、選択箇所を指定して指示できます。
              </p>
            </div>
          ) : null}
          {messages.map((m) =>
            m.role === "user" ? (
              <UserRow key={m.id} blocks={m.content} />
            ) : (
              <AssistantRow
                key={m.id}
                threadId={threadId}
                messageId={m.id}
                blocks={m.content}
                invokedActions={m.invokedActions}
                onUiAction={() => setReloadKey((k) => k + 1)}
              />
            ),
          )}
          {stream ? (
            <StreamingRow
              stream={stream}
              onApproval={decideApproval}
              threadId={threadId}
              onQueueUiAction={onQueueUiAction}
            />
          ) : null}
          {/* 順番待ちは「AI が答えている最中に積んだ次の発話」なので応答の後ろに置く。 */}
          {queued.map((q) => (
            <QueuedRow key={q.key} item={q} onCancel={() => cancelQueued(q)} />
          ))}
          {notice ? (
            <div
              className="rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-sm text-amber-700 dark:text-amber-400"
              data-testid="mode-clamp-notice"
            >
              {notice}
            </div>
          ) : null}
          {error ? (
            <div
              // e2e が「異常終了なのに緑」を見逃さないための足がかり（実測で 429 の run が
              // 完走扱いになっていた）。role="alert" は読み上げにも要る。
              data-testid="conversation-error"
              role="alert"
              className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive"
            >
              {error}
            </div>
          ) : null}
          <div ref={bottomRef} />
        </div>
      </div>

      <div className="bg-background">
        <div className={cn("mx-auto w-full px-4 py-4", isPanel ? "max-w-none pb-3" : "max-w-3xl")}>
          <Composer
            onSubmit={(text, attachments, context, command) =>
              void submitFromComposer(text, attachments, context, command)
            }
            onStop={stop}
            streaming={stream !== null}
            autonomous={autonomous}
            onAutonomousChange={setAutonomous}
            approvalMode={approvalMode}
            onApprovalModeChange={changeApprovalMode}
            bypassAllowed={bypassAllowed}
            threadId={threadId}
            autoFocus
          />
          <p className="mt-2 text-center text-xs text-muted-foreground">
            {isPanel
              ? "誤りが含まれる場合があります。"
              : "Shiki は社内文書を参照して回答します。誤りが含まれる場合があります。"}
          </p>
        </div>
      </div>
    </div>
  );
}

/// ツール実行を挟んだところで段落を切る（末尾が既に空行なら何もしない）。
function paragraphBreak(text: string): string {
  if (!text.trim()) return text;
  return text.endsWith("\n\n") ? text : `${text.replace(/\s+$/, "")}\n\n`;
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
  for (const t of s.tools) {
    blocks.push({ type: "tool_call", id: t.id, name: t.name, input: t.input, step: t.step });
    // 成否と観測テキストも残す（履歴でも失敗と結果要約が見えるようにする・#358/#386）。
    if (t.ok !== undefined) {
      blocks.push({ type: "tool_result", tool_call_id: t.id, content: t.result ?? "", ok: t.ok });
    }
  }
  if (s.text.trim()) blocks.push({ type: "text", text: s.text });
  for (const c of s.citations) blocks.push(c);
  // ツール成果物（保存済みファイル）も確定メッセージへ残す。
  for (const f of s.files) blocks.push({ type: "file_ref", node_id: f.node_id, name: f.name });
  // 検証済み generative UI ブロック（アクションは確定 id で再読込後に有効化される）。
  for (const spec of s.uiSpecs) blocks.push({ type: "generative_ui", spec });
  // 保存済みワークフロー参照カード（Task 10.13）。
  for (const workflow of s.workflowRefs) blocks.push({ type: "workflow_ref", workflow });
  // 保存済みノート参照カード（Task 11P.5）。
  for (const note of s.noteRefs) blocks.push({ type: "note_ref", note });
  // 未保存の下書きノートカード（issue #282）。履歴からも下書きへ辿れるよう残す。
  for (const draft of s.noteDrafts) blocks.push({ type: "note_draft", draft });
  // 未保存の下書きスライドカード（Task 11.3）。
  for (const draft of s.slideDrafts) blocks.push({ type: "slide_draft", draft });
  // 未保存の下書き CSV カード（Task 11.11）。
  for (const draft of s.csvDrafts) blocks.push({ type: "csv_draft", draft });
  // AI が作成/編集した文書への参照カード（#381）。
  for (const document of s.documentRefs) blocks.push({ type: "document_ref", document });
  if (blocks.length === 0) return;
  setMessages((prev) => [
    ...prev,
    { id: newId(), role: "assistant", content: blocks, createdAt: new Date().toISOString() },
  ]);
}

// ── 行レンダリング ───────────────────────────────────────────────────

function UserRow({ blocks, footer }: { blocks: ContentBlock[]; footer?: React.ReactNode }) {
  const text = blocks
    .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
    .map((b) => b.text)
    .join("\n");
  const files = blocks.filter((b): b is Extract<ContentBlock, { type: "file_ref" }> => b.type === "file_ref");
  const selections = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "selection_context" }> =>
      b.type === "selection_context",
  );
  return (
    <Message className="justify-end">
      <div className="flex max-w-[85%] flex-col items-end gap-1.5">
        {selections.map((s, i) => (
          <span
            key={i}
            data-testid="message-selection-chip"
            className="inline-flex max-w-full items-center gap-1 truncate rounded-full border border-border bg-card px-2.5 py-1 text-[12px] text-foreground/80"
          >
            {selectionKindLabel(s.context.kind)}: {s.context.excerpt.slice(0, 80)}
          </span>
        ))}
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
        {footer}
      </div>
    </Message>
  );
}

/// 順番待ちの発話 1 件。吹き出しは送信済みと同じで、下に控えめな状態と取り消しだけ添える
/// （サーバは受理済み＝本当に送れているので、見た目を弱めて「未送信」に見せない）。
function QueuedRow({ item, onCancel }: { item: QueuedMessage; onCancel: () => void }) {
  return (
    <UserRow
      blocks={item.blocks}
      footer={
        <span
          data-testid="queued-message-note"
          className={cn(
            "inline-flex items-center gap-1.5 text-[11px]",
            item.error ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {item.error ? (
            <>
              <AlertTriangle className="size-3 shrink-0" aria-hidden />
              {item.error}
            </>
          ) : (
            <>
              <Clock3 className="size-3 shrink-0" aria-hidden />
              順番待ち
              <span aria-hidden className="text-muted-foreground/40">
                ・
              </span>
              <button
                type="button"
                onClick={onCancel}
                className="rounded underline-offset-2 transition-colors hover:text-foreground hover:underline"
              >
                取り消す
              </button>
            </>
          )}
        </span>
      }
    />
  );
}

function AssistantRow({
  threadId,
  messageId,
  blocks,
  invokedActions,
  onUiAction,
}: {
  threadId: string;
  messageId: string;
  blocks: ContentBlock[];
  /// このメッセージで実行済みの単発 UI アクション（サーバ記録・#410）。
  invokedActions?: readonly string[];
  onUiAction: () => void;
}) {
  const thinking = blocks
    .filter((b): b is Extract<ContentBlock, { type: "thinking" }> => b.type === "thinking")
    .map((b) => b.text)
    .join("");
  // text ブロックは**ツールを挟むたびに切れる**（間に tool_call ブロックが入る）。
  // 連結ではなく段落として繋ぐ（ライブ表示と揃える）。ベタ連結だと
  // 「〜します。〜しました。〜します。」が 1 段落に連なって読めない。
  const text = blocks
    .filter((b): b is Extract<ContentBlock, { type: "text" }> => b.type === "text")
    .map((b) => b.text.trim())
    .filter(Boolean)
    .join("\n\n");
  // ツール結果を tool_call_id で引く（#358/#386）。**同じ id が複数回現れ得る**ため
  // （ループで再利用される呼び出し ID）、id ごとに出現順のキューとして持ち、
  // 呼び出しへ順番に対応付ける（最後の結果を全行へ適用しない）。
  const toolResults = new Map<string, Extract<ContentBlock, { type: "tool_result" }>[]>();
  for (const b of blocks) {
    if (b.type !== "tool_result") continue;
    const q = toolResults.get(b.tool_call_id);
    if (q) q.push(b);
    else toolResults.set(b.tool_call_id, [b]);
  }
  const tools: ToolActivityItem[] = blocks
    .filter((b): b is Extract<ContentBlock, { type: "tool_call" }> => b.type === "tool_call")
    .map((b, i) => {
      const res = toolResults.get(b.id)?.shift();
      return {
        // 確定メッセージ内での出現位置は安定（再レンダーで並びが変わらない）。
        key: `${b.id}-${i}`,
        id: b.id,
        name: b.name,
        running: false,
        input: b.input,
        // 生成型は Option<u32> を `number | null` にする。UI は「不明」を undefined で扱う。
        step: b.step ?? undefined,
        ok: res?.ok ?? undefined,
        result: res?.content,
      };
    });
  const citations = blocks.filter((b): b is Citation => b.type === "citation");
  const files = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "file_ref" }> => b.type === "file_ref",
  );
  const uiSpecs = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "generative_ui" }> => b.type === "generative_ui",
  );
  const workflowRefs = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "workflow_ref" }> => b.type === "workflow_ref",
  );
  const noteRefs = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "note_ref" }> => b.type === "note_ref",
  );
  const noteDrafts = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "note_draft" }> => b.type === "note_draft",
  );
  const slideDrafts = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "slide_draft" }> => b.type === "slide_draft",
  );
  const csvDrafts = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "csv_draft" }> => b.type === "csv_draft",
  );
  const documentRefs = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "document_ref" }> => b.type === "document_ref",
  );
  // レガシー下書き（#381 で廃止）。過去スレッドを黙って空にしないため読み取り専用で残す。
  const legacyDocumentDrafts = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "document_draft" }> => b.type === "document_draft",
  );

  return (
    <Message className="group justify-start">
      <div className="w-full min-w-0">
        <ChainOfThought thinking={thinking} tools={tools} citations={citations} />
        {text ? <Markdown>{linkifyCitations(text, citations)}</Markdown> : null}
        {uiSpecs.length > 0 ? (
          <ChatGenUiProvider
            threadId={threadId}
            messageId={messageId}
            invokedActions={invokedActions}
            onActionCompleted={(result) => {
              // chat.submit は新しい発話と生成を作るため会話を再読込する。
              if (result.result.kind === "handler") onUiAction();
            }}
          >
            {uiSpecs.map((b, i) => (
              <GenUiBlock key={i} spec={b.spec} />
            ))}
          </ChatGenUiProvider>
        ) : null}
        {workflowRefs.map((b, i) => (
          <WorkflowRefCard key={i} raw={b.workflow} />
        ))}
        {noteRefs.map((b, i) => (
          <NoteRefCard key={i} raw={b.note} />
        ))}
        {noteDrafts.map((b, i) => (
          <NoteDraftCard key={i} raw={b.draft} threadId={threadId} />
        ))}
        {slideDrafts.map((b, i) => (
          <SlideDraftCard key={i} raw={b.draft} threadId={threadId} />
        ))}
        {csvDrafts.map((b, i) => (
          <CsvDraftCard key={i} raw={b.draft} threadId={threadId} />
        ))}
        {documentRefs.map((b, i) => (
          <DocumentRefCard key={i} raw={b.document} />
        ))}
        {legacyDocumentDrafts.map((b, i) => (
          <LegacyDocumentDraftCard key={i} raw={b.draft} />
        ))}
        <ArtifactFiles files={files} />
        <Sources citations={citations} />
        {text ? <MessageFooter text={text} /> : null}
      </div>
    </Message>
  );
}

/// generative UI ブロック＝描画＋「アプリとして保存」導線（Phase 6 UX）。
/// chat.submit（チャット専用アクション）を含むスペックはアプリにできないため非活性にする。
function GenUiBlock({ spec }: { spec: unknown }) {
  const [saveOpen, setSaveOpen] = React.useState(false);
  const chatOnly = React.useMemo(() => specHasChatOnlyAction(spec), [spec]);
  return (
    <div className="group/gui relative">
      <SpecRenderer spec={spec} />
      <div className="mt-1 flex justify-end">
        <button
          type="button"
          disabled={chatOnly}
          onClick={() => setSaveOpen(true)}
          title={
            chatOnly
              ? "このUIはチャット専用アクションを含むためアプリにできません"
              : "この画面をアプリとして保存する"
          }
          className="inline-flex items-center gap-1.5 rounded-full border border-border px-2.5 py-1 text-[12px] text-foreground/70 transition-colors hover:border-primary/40 hover:bg-secondary hover:text-foreground disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-border disabled:hover:bg-transparent"
        >
          <LayoutGrid className="size-3.5" aria-hidden />
          アプリとして保存
        </button>
      </div>
      {!chatOnly ? (
        <SaveAsAppDialog open={saveOpen} onOpenChange={setSaveOpen} spec={spec} />
      ) : null}
    </div>
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
  threadId,
  onQueueUiAction,
}: {
  stream: StreamState;
  onApproval: (approved: boolean) => void;
  threadId: string;
  /// 生成中に押されたカード操作の受け皿（生成完了後に実行する）。
  onQueueUiAction: (actionId: string, params: unknown) => void;
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
        {/* streaming は「生成中か」であって「本文が出ていないか」ではない。旧実装は
            `!stream.text` を渡していたため、本文が 1 文字出た瞬間にツール表示が畳まれ、
            その後に走るツール（本文 → ツール → 本文の往復）が見えなくなっていた（#386）。 */}
        <ChainOfThought
          thinking={stream.thinking}
          tools={stream.tools}
          citations={stream.citations}
          streaming
          phase={runningSubtask(stream.plan)}
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
        {stream.uiSpecs.length > 0 ? (
          // messageId は null（＝まだ保存されていないので即時実行できない）。押された操作は
          // onQueue で受け取り、生成完了時に確定した assistant メッセージへ流す。
          <ChatGenUiProvider threadId={threadId} messageId={null} onQueue={onQueueUiAction}>
            {stream.uiSpecs.map((spec, i) => (
              <SpecRenderer key={i} spec={spec} />
            ))}
          </ChatGenUiProvider>
        ) : null}
        {stream.workflowRefs.map((workflow, i) => (
          <WorkflowRefCard key={i} raw={workflow} />
        ))}
        {stream.noteDrafts.map((draft, i) => (
          <NoteDraftCard key={i} raw={draft} threadId={threadId} />
        ))}
        {stream.slideDrafts.map((draft, i) => (
          <SlideDraftCard key={i} raw={draft} threadId={threadId} />
        ))}
        {stream.csvDrafts.map((draft, i) => (
          <CsvDraftCard key={i} raw={draft} threadId={threadId} />
        ))}
        {stream.documentRefs.map((document, i) => (
          <DocumentRefCard key={i} raw={document} />
        ))}
        <ArtifactFiles files={stream.files} />
      </div>
    </Message>
  );
}

/// skill ツール入力からスキル名を取り出す（バックエンドと同じく trim して突き合わせる）。
function skillNameOf(input: unknown): string | null {
  const name = (input as { name?: unknown } | undefined)?.name;
  return typeof name === "string" ? name.trim() || null : null;
}

/// 実行中のサブタスク名（自律 run の計画）。ツール実行のフェーズ行に使う。
/// 計画があるときは「検索しています」より「市場規模を調べています」の方が情報量が多い。
function runningSubtask(plan: PlanSubtask[]): string | null {
  const doing = plan.find((s) => s.status === "doing");
  const title = doing?.title.trim();
  return title ? `${title}` : null;
}

/// 計画イベントを蓄積する。フル計画（全 title 非空）は置換、単一の空 title は id で status 更新。
function mergePlan(prev: PlanSubtask[], incoming: PlanSubtask[]): PlanSubtask[] {
  const isStatusOnly = incoming.length === 1 && incoming[0].title === "";
  if (!isStatusOnly) return incoming;
  const upd = incoming[0];
  return prev.map((s) => (s.id === upd.id ? { ...s, status: upd.status } : s));
}
