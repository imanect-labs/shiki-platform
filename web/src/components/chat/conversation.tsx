"use client";

import * as React from "react";

import Link from "next/link";
import { useRouter } from "next/navigation";
import {
  AlertTriangle,
  ArrowDown,
  ChevronDown,
  ChevronUp,
  Clock3,
  FileDown,
} from "lucide-react";

import {
  cancelRun,
  getAutonomousMode,
  getThread,
  getThreadMessages,
  postMessage,
  isEmptyContent,
  notifyThreadsChanged,
  resumeMessage,
  setAutonomousMode,
  streamMessage,
  submitApproval,
  subscribeThreadsChanged,
  ThreadNotFound,
  type ApprovalRequest,
  type AutonomousMode,
  type Attachment,
  type Citation,
  type ContentBlock,
  type ContextUsage,
  type Message as ChatMessageT,
  type PlanSubtask,
  type RunStatus,
  type StreamHandlers,
  type UiActionRun,
} from "@/lib/chat-api";
import { popPending } from "@/lib/pending-message";
import { readModelChoice } from "@/lib/model-choice";
import { readToolChoice, settleToolChoice } from "@/lib/tool-choice";
import { triggerDownload } from "@/lib/storage";
import { newId } from "@/lib/chat-store";
import { selectionKindLabel, type SelectionContext } from "@/lib/selection-context";
import type { ActiveCommand } from "@/lib/slash-command";
import { Message, MessageContent } from "@/components/prompt-kit/message";
import { invokeChatYuiAction } from "@/lib/artifact-api";
import { SaveAsAppDialog, type SaveAsAppTarget } from "@/components/artifacts/save-as-app-dialog";
import { Loader } from "@/components/prompt-kit/loader";
import {
  hasOpenRuns,
  hasYuiBlock,
  YuiActionContext,
  type YuiActionHost,
} from "@/components/prompt-kit/yui-actions";
import { useMediaQuery } from "@/hooks/use-media-query";
import { useStickToBottom } from "@/hooks/use-stick-to-bottom";
import { useTurnJump } from "@/hooks/use-turn-jump";
import { MessageFeedbackButtons, ThreadFeedbackProvider } from "./message-feedback";
import { MessageFooter } from "./message-footer";
import { MessageCitationsProvider, SourcePanelProvider } from "./citations/citation-context";
import { CitationSources } from "./citations/citation-sources";
import { EvidenceTable, EvidenceToggle } from "./citations/evidence-table";
import { SourcePanelAside, SourcePanelSheet } from "./citations/source-panel";
import { type ToolActivityItem } from "./tool-activity";
import { AttachmentThumb, isImageAttachment } from "./attachment-thumb";
import { StreamWaitingLine, useStreamWaiting } from "./stream-waiting";
import {
  appendStreamText,
  appendStreamThinking,
  appendStreamTool,
  MessageTimeline,
  resolveStreamSegments,
  segmentsFromBlocks,
  type StreamSegment,
} from "./message-timeline";
import { Composer } from "./composer";
import { PanelEmptyHint } from "./doc-chat-draft";
import { WorkflowRefCard } from "./workflow-ref-card";
import { NoteRefCard } from "./note-ref-card";
import { NoteDraftCard } from "./note-draft-card";
import { SlideDraftCard } from "./slide-draft-card";
import { CsvDraftCard } from "./csv-draft-card";
import { SkillDraftCard } from "./skill-draft-card";
import { DocumentRefCard, LegacyDocumentDraftCard, parseDocumentRef, documentRefHref } from "./document-ref-card";
import { upsertDraft, parseNoteDraft } from "@/lib/notes/draft-store";
import { draftHref } from "@/lib/notes/draft-nav";
import { parseSlideDraft, slideDraftHref, slideDraftStore } from "@/lib/slides/draft";
import { csvDraftHref, csvDraftStore, parseCsvDraft } from "@/lib/csv/draft";
import {
  mergeAiSkill,
  parseSkillDraft,
  parseStoredSkillBody,
  skillDraftHref,
  skillDraftStore,
  type AiSkillDraft,
} from "@/lib/skills/skill-draft";
import { ThreadShareDialog } from "./share-dialog";
import { ChatPageHeaderSlot } from "./chat-header-actions";
import { ContextUsageDialog } from "./context-meter";
import { ApprovalCard, BudgetBanner, PlanPanel } from "./agent-progress";
import { cn } from "@/lib/utils";

/// ストリーミング中のアシスタント応答の蓄積状態。
type StreamState = {
  text: string;
  /// 本文と操作のまとまり（思考・ツール）を起きた順に並べたもの（`message-timeline.tsx`）。
  segments: StreamSegment[];
  thinking: string;
  tools: ToolActivityItem[];
  citations: Citation[];
  /// ツール成果物（code_interpreter が保存したファイル参照）。
  files: Attachment[];
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
  /// 未保存の下書きスキル（#919・save_skill の下書き確定型）。
  skillDrafts: unknown[];
  /// AI が作成/編集した文書の参照（#381・save_document / save_sheet / 各編集ツール）。
  documentRefs: unknown[];
  /// 自律エージェント（Phase 5）: 計画・承認要求・予算警告。
  plan: PlanSubtask[];
  approval: ApprovalRequest | null;
  budget: { kind: string; used: number; limit: number } | null;
  runId: string | null;
  /// 生成先の assistant メッセージ id。run 完了までは**保存されていない**ので、図の
  /// ボタンはまだ押せない（確定後に読み直して有効になる）。
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
  segments: [],
  thinking: "",
  tools: [],
  citations: [],
  files: [],
  workflowRefs: [],
  noteRefs: [],
  noteDrafts: [],
  slideDrafts: [],
  csvDrafts: [],
  skillDrafts: [],
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
  onSkillDraftOpened,
  adoptSkillDraftOnLoad = false,
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
  /// save_skill の下書き（skill_draft）を受けたときの導線（#919）。渡されない場合は run の
  /// 完了後にスキルエディタへ遷移する（主線）。スキルエディタ自身はその場で内容を流し込む。
  /// `key` はその下書きのイベントを指す（応答のメッセージ id ＋ 応答の中の何番目か）。パネルの
  /// 開き直しでの再生と読み直しで同じ値になり、同じ内容を言い直した別の下書きとは区別できる。
  onSkillDraftOpened?: (draft: AiSkillDraft, key: string | null) => void;
  /// 読み込んだ時点で最後の発話への応答が済んでいたら、その応答の下書きスキルも
  /// `onSkillDraftOpened` へ渡す。この画面で会話を作って最初の発話を送った直後に使う
  /// （応答が速いと購読より先に終わり、下書きのイベントを受け取れないため・#919）。
  adoptSkillDraftOnLoad?: boolean;
  /// AI が Office 文書を**新規作成**したときの導線（#381）。渡されない場合は
  /// 作成された文書（Collabora）へ遷移する（主線）。編集の参照では発火しない。
  onDocumentCreated?: (href: string) => void;
}) {
  const isPanel = variant === "panel";
  const router = useRouter();
  // 出典パネルを右カラムで出せる幅か（サイドバー＋本文＋パネルが並ぶ幅）。
  const wide = useMediaQuery("(min-width: 1200px)");
  const sourceAside = !isPanel && wide;
  const [messages, setMessages] = React.useState<ChatMessageT[]>([]);
  const [stream, setStream] = React.useState<StreamState | null>(null);
  const [notFound, setNotFound] = React.useState(false);
  // 作成した文書の遷移先。**run 完了後**に遷移する（#381）: document_ref はツール結果直後に
  // 届くため、その場で router.push すると Conversation がアンマウントされて SSE 購読が閉じ、
  // 複合依頼の後続ツールの承認カードが出せず run が承認待ちで止まる。
  const pendingOpenRef = React.useRef<string | null>(null);
  // 読み込み時に下書きスキルを渡したか（`adoptSkillDraftOnLoad`・1 回だけ）。
  const adoptedDraftRef = React.useRef(false);
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
  /// 直前の LLM リクエストのコンテキスト使用量（⋯ メニューの「コンテキスト」）。読み込みと SSE で更新する。
  const [contextUsage, setContextUsage] = React.useState<ContextUsage | null>(null);
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
  // コンテキストの使用量（⋯ メニューから開く）。
  const [contextOpen, setContextOpen] = React.useState(false);
  const openContext = React.useCallback(() => setContextOpen(true), []);
  // UI アクション（chat.submit 等）成功後に会話を再読込するためのキー。
  const [reloadKey, setReloadKey] = React.useState(0);
  // 確定行（memo）へ渡すので安定した参照にする（トークンごとに過去の行を描き直さない）。
  const reload = React.useCallback(() => setReloadKey((k) => k + 1), []);
  // 回答の ```ui app を「アプリとして保存」する対象（#604）。
  const [saveTarget, setSaveTarget] = React.useState<SaveAsAppTarget | null>(null);
  // スレッド名（クロームのタイトル・⋯ メニューの見出し）。page のときだけ引く。
  const threadTitle = useThreadTitle(isPanel ? null : threadId);
  // 追従スクロール（下端付近にいる間だけ追う・離れたら「最新へ」）。
  const { scrollerRef, contentRef, scroller, content, showJump, scrollToBottom } =
    useStickToBottom();
  // 読み返し中の「一つ前／一つ後の自分の発話へ」。
  const turnJump = useTurnJump(scroller, content);
  // 入力欄は本文の上に重ねる（背後を透かす）。その高さだけ本文の末尾と「最新へ」を持ち上げる。
  const [composerBox, setComposerBox] = React.useState<HTMLDivElement | null>(null);
  const [composerHeight, setComposerHeight] = React.useState(0);
  React.useEffect(() => {
    if (!composerBox || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => setComposerHeight(composerBox.offsetHeight));
    ro.observe(composerBox);
    return () => ro.disconnect();
  }, [composerBox]);
  // 順番待ちの発話（サーバ受理済み・生成未開始）。ストリーム行の下に並べる。
  const [queued, setQueued] = React.useState<QueuedMessage[]>([]);
  // onDone は makeHandlers の中で閉じ込められるため、最新の件数を ref で見る。
  const queuedCountRef = React.useRef(0);
  React.useEffect(() => {
    queuedCountRef.current = queued.length;
  }, [queued]);
  // 停止関数。`cancelServer` でサーバ側もキャンセル（明示停止）。離脱は継続（呼ばない）。
  const cancelRef = React.useRef<((opts?: { cancelServer?: boolean }) => void) | null>(null);
  const sentPending = React.useRef(false);
  // stream の正は ref に置き、state は描画用の写しとする。setState updater 経由の
  // 遅延同期（useEffect）だと、SSE が同一マイクロタスクで連続到達したとき ref が
  // 1 レンダー分古いまま onDone に入り、最後のバッチ（下書き・参照など）が確定から
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
  const flushStream = React.useCallback((provisional: "reloading" | "stopped") => {
    const s = streamRef.current;
    if (s) finalizeStream(s, setMessages, provisional);
    streamRef.current = null;
    setStream(null);
  }, []);

  // 送信/復元で共通の SSE ハンドラ。蓄積を stream state に反映し、端末で確定する。
  const makeHandlers = React.useCallback((): StreamHandlers => {
    return {
      onThinking: (t) =>
        updateStream((s) =>
          s
            ? { ...s, thinking: s.thinking + t, segments: appendStreamThinking(s.segments, t) }
            : s,
        ),
      onToken: (t) =>
        updateStream((s) =>
          s ? { ...s, text: s.text + t, segments: appendStreamText(s.segments, t) } : s,
        ),
      onToolCall: (call) =>
        updateStream((s) => {
          if (!s) return s;
          // ステップの区切りで段落を落とす。ツールを挟んだ発話は別の思考なので、
          // 繋げて書くと「〜します。〜しました。〜します。」が 1 段落に連なって読めない。
          const text = paragraphBreak(s.text);
          // この出現の一意キー（呼び出し ID はループで再利用され得る）。
          const key = newId();
          return {
            ...s,
            text,
            segments: appendStreamTool(s.segments, key),
            tools: [
              ...s.tools,
              {
                key,
                id: call.id,
                name: call.name,
                running: true,
                input: call.input,
                step: call.step,
                viaSubagent: call.viaSubagent,
              },
            ],
          };
        }),
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
      onSkillDraft: (raw) => {
        // note_draft と同型: ストリームに残しつつ下書きストアへ upsert（source=ai＝流し込み）。
        const cur = streamRef.current;
        const owner = cur?.assistantMessageId ?? null;
        const key = cur && owner ? `${owner}:${cur.skillDrafts.length}` : null;
        updateStream((s) => (s ? { ...s, skillDrafts: [...s.skillDrafts, raw] } : s));
        const d = parseSkillDraft(raw);
        if (!d) return;
        // 渡した項目だけを重ねる（直すたびに他の項目を消さない）。
        const prev = parseStoredSkillBody(skillDraftStore.get(threadId, d.name)?.content ?? "");
        skillDraftStore.upsert(threadId, d.name, JSON.stringify(mergeAiSkill(prev, d)), "ai");
        if (onSkillDraftOpened) onSkillDraftOpened(d, key);
        // 遷移は run の完了後（途中で遷移すると SSE が切れ、後続の操作が止まる・#381 と同じ）。
        else pendingOpenRef.current = skillDraftHref(threadId, d.name);
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
      onContextUsage: setContextUsage,
      onApprovalRequested: (req) =>
        updateStream((s) => (s ? { ...s, approval: req, approvalPending: false } : s)),
      onApprovalResolved: (res) =>
        updateStream((s) => {
          if (!s) return s;
          // 却下した操作は失敗ではなく「却下しました」と出す（3-A-8）。
          const tools = res.approved
            ? s.tools
            : s.tools.map((t) => (t.id === res.tool_call_id ? { ...t, rejected: true } : t));
          return s.approval?.tool_call_id === res.tool_call_id
            ? { ...s, tools, approval: null, approvalPending: false }
            : { ...s, tools };
        }),
      // 承認モードのクランプ（org 禁止・他人による緩和）は明示的に知らせる（黙って降格しない・#350）。
      // 承認待ちが多すぎて操作を実行しなかったことも同じ枠で知らせる（ツールの表示は「却下」になるため）。
      onFailureRecovery: (r) => {
        if (r.action === "mode_clamped" || r.action === "approval_overloaded") setNotice(r.detail);
      },
      onStatus: (status: RunStatus) => {
        if (status === "cancelled") setError("生成をキャンセルしました。");
        if (status === "failed") setError("生成に失敗しました。");
        // 中断した run の保留遷移は捨てる（次の run の完了時に横取りされないように）。
        if (status === "cancelled" || status === "failed") pendingOpenRef.current = null;
      },
      onDone: () => {
        // 本文の ```ui の図を含む応答はサーバ確定の message id が要る（ボタンの照合先・
        // 束縛はサーバーが回答の確定時に解決する・#604）ため、ローカル確定ではなく再読込で置き換える。
        const hadUi = hasYuiBlock(streamRef.current?.text ?? "");
        flushStream("reloading");
        cancelRef.current = null;
        notifyThreadsChanged();
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
    onSkillDraftOpened,
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
      // 自分の発話は必ず見える位置へ（読み返し中に送っても、送ったものと返答を追う）。
      scrollToBottom();
      // ホームからの初回メッセージは選択時点の値を明示指定する（state 初期化のタイミングに依存しない）。
      const runAutonomous = autonomousOverride ?? autonomous;
      // 入力欄で選んだモデル（#553）。未選択は Auto（組織の既定）。
      const model = readModelChoice(threadId);
      // 入力欄で選んだツール（#734）。この発話にだけ効くが、空に戻すのは**受理されてから**
      // （400・通信失敗で選択が消えると、送り直すときに選び直しになる）。
      const tools = readToolChoice(threadId);
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
          model,
          tools,
        })
          .then((posted) => {
            settleToolChoice(threadId, tools);
            setQueued((prev) =>
              prev.map((q) => (q.key === key ? { ...q, runId: posted.runId } : q)),
            );
          })
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
      const handlers = makeHandlers();
      cancelRef.current = streamMessage(
        threadId,
        text,
        attachments,
        {
          ...handlers,
          // run_id が来た＝POST が受理された。ここで初めて選択を空に戻す。
          onRunId: (runId) => {
            settleToolChoice(threadId, tools);
            handlers.onRunId?.(runId);
          },
        },
        runAutonomous,
        runAutonomous,
        context,
        onceSkills,
        model,
        tools,
      );
    },
    [threadId, makeHandlers, autonomous, scrollToBottom],
  );

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
    flushStream("stopped");
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
          contextUsage: usage,
        }) => {
        if (!active) return;
        setContextUsage(usage);
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
        // 初回の読み込みでだけ（その後の読み直しで、手で直した内容を上書きしない）。
        if (adoptSkillDraftOnLoad && onSkillDraftOpened && !resuming && !adoptedDraftRef.current) {
          adoptedDraftRef.current = true;
          const lastUser = shown.map((m) => m.role).lastIndexOf("user");
          // 最後の下書きと、それが応答の中で何番目か（ストリームで受けたときと同じ key にする）。
          const found = shown
            .slice(lastUser + 1)
            .flatMap((m) =>
              m.content
                .filter((b): b is Extract<ContentBlock, { type: "skill_draft" }> => b.type === "skill_draft")
                .map((b, i) => ({ draft: b.draft, key: `${m.id}:${i}` })),
            )
            .at(-1);
          const latest = found ? parseSkillDraft(found.draft) : null;
          if (found && latest) onSkillDraftOpened(latest, found.key);
        }
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

  // 免責文とアクションの常時表示は「最新の回答」にだけ付ける（生成中は新しい回答を待つ）。
  const latestAssistantId = stream
    ? null
    : ([...messages].reverse().find((m) => m.role === "assistant")?.id ?? null);

  if (notFound) {
    return (
      <div className="flex h-full items-center justify-center px-4">
        <p className="text-sm text-muted-foreground">この会話は見つかりませんでした。</p>
      </div>
    );
  }

  return (
    <SourcePanelProvider key={threadId}>
      <ThreadFeedbackProvider threadId={threadId}>
      <div className="flex h-full min-h-0">
        <div className="flex h-full min-h-0 min-w-0 flex-1 flex-col">
          {/* page: 統一ヘッダスロットへタイトル＋共有/設定を注入（横バー二重を解消）。
              panel: 注入しない（分割ビューは自前ヘッダを持つ・null を返すだけ）。 */}
          {!isPanel ? (
            <ChatPageHeaderSlot
              title={threadTitle || "新しいチャット"}
              onShare={openShare}
              contextUsage={contextUsage}
              onOpenContext={openContext}
            />
          ) : null}
          <ThreadShareDialog open={shareOpen} onOpenChange={setShareOpen} threadId={threadId} />
          <ContextUsageDialog usage={contextUsage} open={contextOpen} onOpenChange={setContextOpen} />
          <SaveAsAppDialog
            open={saveTarget != null}
            onOpenChange={(open) => !open && setSaveTarget(null)}
            target={saveTarget}
          />

          <div className="relative flex min-h-0 flex-1 flex-col">
            <div
              ref={scrollerRef}
              className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden overscroll-y-contain"
              data-testid="conversation-scroller"
            >
              <div
                ref={contentRef}
                className={cn(
                  "mx-auto flex w-full min-w-0 flex-col gap-6 px-4 py-8",
                  // スマホ: 上端の浮いたボタン（メニュー・新しいチャット・⋯）の下に本文が潜り込めるよう空ける。
                  isPanel ? "max-w-none px-4 py-5" : "max-w-3xl max-md:pt-[calc(4.5rem+env(safe-area-inset-top))]",
                )}
                // 下端は重ねた入力欄の下に潜らないよう、その高さぶん空ける（py の下側を置き換える）。
                style={{ paddingBottom: composerHeight + (isPanel ? 20 : 32) }}
              >
                {/* パネルの空会話は「何ができるか」を軽く案内する（空白のままにしない）。 */}
                {isPanel && messages.length === 0 && !stream && !notFound && !error ? (
                  <PanelEmptyHint />
                ) : null}
                {messages.map((m) =>
                  m.role === "user" ? (
                    <UserRow key={m.id} blocks={m.content} />
                  ) : (
                    <AssistantRow
                      key={m.id}
                      threadId={threadId}
                      messageId={m.id}
                      savedMessageId={savedMessageId(m)}
                      blocks={m.content}
                      invokedActions={m.invokedActions}
                      boundActions={m.boundActions}
                      uiRuns={m.uiRuns}
                      provisional={m.provisional}
                      onUiAction={reload}
                      onSaveAsApp={setSaveTarget}
                      latest={m.id === latestAssistantId}
                      compactDisclaimer={isPanel}
                    />
                  ),
                )}
                {stream ? (
                  <StreamingRow
                    stream={stream}
                    onApproval={decideApproval}
                    threadId={threadId}
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
              </div>
            </div>
            {/* 読み返し中は、コンポーザの上中央に「最新へ」を浮かせ、PC ではその左右に「一つ前／一つ後の
                自分の発話へ」を添える（長い回答を挟んだ往復を発話単位で辿る）。スマホは本文を隠すので
                「最新へ」だけにする。 */}
            {showJump ? (
              <div
                style={{ bottom: composerHeight + 12 }}
                className="absolute left-1/2 z-10 flex -translate-x-1/2 items-center gap-2"
              >
                <button
                  type="button"
                  onClick={turnJump.jumpPrev}
                  disabled={!turnJump.canPrev}
                  aria-label="一つ前の自分の発言へ移動"
                  data-testid="jump-to-prev-turn"
                  className={cn(JUMP_PILL, "pl-2.5 pr-3.5 max-md:hidden")}
                >
                  <ChevronUp className="size-4" aria-hidden />
                  一つ前
                </button>
                <button
                  type="button"
                  onClick={() => scrollToBottom("smooth")}
                  aria-label="最新のメッセージへ移動"
                  data-testid="jump-to-latest"
                  className={cn(JUMP_PILL, "size-10 justify-center")}
                >
                  <ArrowDown className="size-[18px]" aria-hidden />
                </button>
                <button
                  type="button"
                  onClick={turnJump.jumpNext}
                  disabled={!turnJump.canNext}
                  aria-label="一つ後の自分の発言へ移動"
                  data-testid="jump-to-next-turn"
                  className={cn(JUMP_PILL, "pl-3.5 pr-2.5 max-md:hidden")}
                >
                  一つ後
                  <ChevronDown className="size-4" aria-hidden />
                </button>
              </div>
            ) : null}

            {/* 入力欄は帯で区切らず本文の上に浮かせる。入力欄の下半分から下端だけ背景色へ
                ぼかし、潜り込んだ本文が入力欄の下の隙間でちらつかないようにする。入力欄の上へ開く
                「/」の補完が「一つ前・最新へ・一つ後」より前に出るよう、こちらを上の層にする。 */}
            <div ref={setComposerBox} className="pointer-events-none absolute inset-x-0 bottom-0 z-20">
              <div
                aria-hidden
                className="absolute inset-x-0 bottom-0 h-1/2 bg-gradient-to-t from-background from-40% to-transparent"
              />
              <div
                className={cn(
                  "pointer-events-auto relative mx-auto w-full px-4 pt-2 pb-[max(0.75rem,env(safe-area-inset-bottom))] md:pb-4 md:pt-3",
                  isPanel ? "max-w-none md:pb-3" : "max-w-3xl",
                )}
              >
                <Composer
                  onSubmit={submitFromComposer}
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
              </div>
            </div>
          </div>
        </div>
        {/* 出典パネル: 広い画面のチャットページは右カラム、それ以外（狭い画面・分割ビュー）はシート。 */}
        {sourceAside ? <SourcePanelAside /> : null}
      </div>
      {!sourceAside ? <SourcePanelSheet /> : null}
      </ThreadFeedbackProvider>
    </SourcePanelProvider>
  );
}

/// ツール実行を挟んだところで段落を切る（末尾が既に空行なら何もしない）。
function paragraphBreak(text: string): string {
  if (!text.trim()) return text;
  return text.endsWith("\n\n") ? text : `${text.replace(/\s+$/, "")}\n\n`;
}

/// 保存済みの回答の ID。読み込んだ行は `id` そのもの、生成直後の仮の行は `serverId`
/// （ストリームで ID を受け取れなかったときは `null`＝評価などは読み直した後に出す）。
function savedMessageId(m: ChatMessageT): string | null {
  return m.provisional ? (m.serverId ?? null) : m.id;
}

/// ストリーミング完了時に蓄積を確定メッセージへ変換して追加する。
function finalizeStream(
  s: StreamState,
  setMessages: React.Dispatch<React.SetStateAction<ChatMessageT[]>>,
  provisional: NonNullable<ChatMessageT["provisional"]>,
) {
  const blocks: ContentBlock[] = [];
  // 本文・思考・ツールは起きた順に残す（サーバの projection と同じ並び）。生成中と同じ配置で
  // 描けるので、確定で表示が組み替わらない。ツール実行履歴が無いと done 後に履歴が消える。
  for (const seg of resolveStreamSegments(s.segments, s.tools)) {
    if (seg.kind === "text") {
      if (seg.text.trim()) blocks.push({ type: "text", text: seg.text });
      continue;
    }
    if (seg.thinking.trim()) blocks.push({ type: "thinking", text: seg.thinking });
    for (const t of seg.tools) {
      blocks.push({ type: "tool_call", id: t.id, name: t.name, input: t.input, step: t.step });
      // 成否と観測テキストも残す（履歴でも失敗と結果要約が見えるようにする・#358/#386）。
      if (t.ok !== undefined) {
        blocks.push({ type: "tool_result", tool_call_id: t.id, content: t.result ?? "", ok: t.ok });
      }
    }
  }
  for (const c of s.citations) blocks.push(c);
  // ツール成果物（保存済みファイル）も確定メッセージへ残す。
  for (const f of s.files) blocks.push({ type: "file_ref", node_id: f.node_id, name: f.name });
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
  // 未保存の下書きスキルカード（#919）。
  for (const draft of s.skillDrafts) blocks.push({ type: "skill_draft", draft });
  // AI が作成/編集した文書への参照カード（#381）。
  for (const document of s.documentRefs) blocks.push({ type: "document_ref", document });
  if (blocks.length === 0) return;
  setMessages((prev) => [
    ...prev,
    {
      id: newId(),
      role: "assistant",
      content: blocks,
      provisional,
      createdAt: new Date().toISOString(),
      serverId: s.assistantMessageId ?? undefined,
    },
  ]);
}

// ── 行レンダリング ───────────────────────────────────────────────────

/// 会話画面のスレッド名。作成直後の自動命名・改名に追従するため、一覧の変更通知で読み直す。
function useThreadTitle(threadId: string | null): string | null {
  const [title, setTitle] = React.useState<string | null>(null);
  React.useEffect(() => {
    setTitle(null);
    if (!threadId) return;
    let active = true;
    const load = () =>
      getThread(threadId)
        .then((t) => active && setTitle(t.title.trim() || null))
        .catch(() => {
          /* 読めなければ「新しいチャット」のまま（会話そのものは別に読み込む） */
        });
    void load();
    const unsubscribe = subscribeThreadsChanged(() => void load());
    return () => {
      active = false;
      unsubscribe();
    };
  }, [threadId]);
  return title;
}

/// 読み返し中に浮かせる移動ボタン（最新へ・一つ前・一つ後）の共通の面。
const JUMP_PILL =
  "flex h-10 items-center gap-1 whitespace-nowrap rounded-full border border-border bg-background/95 text-[13px] font-medium text-foreground/80 shadow-md backdrop-blur transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-45";

/// 確定したユーザー発話。memo して、生成中のトークンで過去の行を描き直さない。
const UserRow = React.memo(function UserRow({
  blocks,
  footer,
}: {
  blocks: ContentBlock[];
  footer?: React.ReactNode;
}) {
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
    // data-user-turn: 「一つ前／一つ後」の移動先（use-turn-jump の USER_TURN_ATTR）。
    <Message className="justify-end" data-user-turn="">
      <div className="flex min-w-0 max-w-[85%] flex-col items-end gap-1.5">
        {selections.map((s, i) => (
          <span
            key={i}
            data-testid="message-selection-chip"
            className="inline-flex max-w-full items-center gap-1 truncate rounded-full border border-border bg-card px-2.5 py-1 text-[12px] text-foreground/80"
          >
            {selectionKindLabel(s.context.kind)}:{" "}
            {/* 編集中のスキルは本文が JSON なので、名前だけを見せる（#919）。 */}
            {s.context.kind === "skill_draft"
              ? s.context.draft_name || "名前未設定"
              : s.context.excerpt.slice(0, 80)}
          </span>
        ))}
        {files.length > 0 ? (
          <div className="flex flex-wrap items-center justify-end gap-1.5">
            {files.map((f) =>
              // 画像は小さい四角で中身を見せ、押すとファイルのビューアで開く。
              isImageAttachment(f) ? (
                <Link
                  key={f.node_id}
                  href={`/file/${f.node_id}`}
                  aria-label={`${f.name} を開く`}
                  className="flex rounded-xl outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <AttachmentThumb nodeId={f.node_id} name={f.name} className="size-16" />
                </Link>
              ) : (
                <span
                  key={f.node_id}
                  className="inline-flex min-w-0 max-w-full items-center gap-1 rounded-full border border-border bg-card px-2.5 py-1 text-[12px] text-foreground/80 [overflow-wrap:anywhere]"
                >
                  📎 {f.name}
                </span>
              ),
            )}
          </div>
        ) : null}
        {text ? (
          <MessageContent className="max-w-full rounded-2xl bg-secondary px-4 py-2.5 text-[15px] leading-relaxed text-secondary-foreground [overflow-wrap:anywhere]">
            {text}
          </MessageContent>
        ) : null}
        {footer}
      </div>
    </Message>
  );
});

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
                className="relative rounded underline-offset-2 transition-colors after:absolute after:-inset-x-2 after:-inset-y-3 hover:text-foreground hover:underline pointer-coarse:underline"
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

/// 確定したアシスタントの回答。memo して、生成中のトークンで過去の行を描き直さない
/// （props は確定メッセージの blocks と安定したコールバックだけ）。
const AssistantRow = React.memo(function AssistantRow({
  threadId,
  messageId,
  savedMessageId,
  blocks,
  invokedActions,
  boundActions,
  uiRuns,
  provisional,
  onUiAction,
  onSaveAsApp,
  latest = false,
  compactDisclaimer = false,
}: {
  threadId: string;
  messageId: string;
  /// 保存済みの回答の ID（評価に使う・まだ分からなければ null）。
  savedMessageId: string | null;
  blocks: ContentBlock[];
  /// このメッセージで実行済みの単発 UI アクション（サーバ記録・#410）。
  invokedActions?: readonly string[];
  /// 本文の図のワークフローのボタンのうち押せるもの・起動の記録（#604）。
  boundActions?: readonly string[];
  uiRuns?: readonly UiActionRun[];
  /// 生成直後の仮のメッセージ（読み直すまで図のボタンは押せない）。
  provisional?: ChatMessageT["provisional"];
  onUiAction: () => void;
  /// 図を「アプリとして保存」するダイアログを開く。
  onSaveAsApp: (target: SaveAsAppTarget) => void;
  /// 最新の回答（アクションを常に出し、下に免責文を 1 回だけ添える）。
  latest?: boolean;
  /// 免責文を短くする（分割ビューのパネル）。
  compactDisclaimer?: boolean;
}) {
  // コールバックが変わっても図・タイマーを作り直さないよう、ref で最新を呼ぶ。
  const reload = React.useRef(onUiAction);
  reload.current = onUiAction;
  const onChanged = React.useCallback(() => reload.current(), []);
  const yuiHost = React.useMemo<YuiActionHost>(
    () => ({
      invoke: (key, values, idempotencyKey) =>
        invokeChatYuiAction(threadId, messageId, key, values, idempotencyKey),
      isBound: (key) => (boundActions ?? []).includes(key),
      invokedActions: invokedActions ?? [],
      uiRuns: uiRuns ?? [],
      unsaved: provisional,
      onChanged,
      saveAsApp: (block, title) => onSaveAsApp({ threadId, messageId, block, title }),
    }),
    [threadId, messageId, boundActions, invokedActions, uiRuns, provisional, onChanged, onSaveAsApp],
  );
  // 図のボタンで起動したワークフローが終わるまで、会話を少しずつ読み直して状態を出す。
  const openRuns = hasOpenRuns(uiRuns);
  React.useEffect(() => {
    if (!openRuns) return;
    const t = window.setTimeout(onChanged, 4000);
    return () => window.clearTimeout(t);
  }, [openRuns, uiRuns, onChanged]);
  // 本文と操作のまとまりを起きた順に（生成中と同じ配置）。
  const segments = React.useMemo(() => segmentsFromBlocks(blocks), [blocks]);
  // コピー・引用の対応付け用の本文全体。区切りは段落として繋ぐ（ライブ表示と揃える）。
  const text = segments
    .flatMap((seg) => (seg.kind === "text" ? [seg.text] : []))
    .join("\n\n");
  const citations = React.useMemo(
    () => blocks.filter((b): b is Citation => b.type === "citation"),
    [blocks],
  );
  const [evidenceOpen, setEvidenceOpen] = React.useState(false);
  const files = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "file_ref" }> => b.type === "file_ref",
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
  const skillDrafts = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "skill_draft" }> => b.type === "skill_draft",
  );
  const documentRefs = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "document_ref" }> => b.type === "document_ref",
  );
  // レガシー下書き（#381 で廃止）。過去スレッドを黙って空にしないため読み取り専用で残す。
  const legacyDocumentDrafts = blocks.filter(
    (b): b is Extract<ContentBlock, { type: "document_draft" }> => b.type === "document_draft",
  );

  return (
    <MessageCitationsProvider messageKey={messageId} citations={citations} text={text}>
      <Message className="group justify-start">
        <div className="w-full min-w-0">
          <YuiActionContext.Provider value={yuiHost}>
            <div className="space-y-2">
              <MessageTimeline segments={segments} citations={citations} />
            </div>
          </YuiActionContext.Provider>
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
          {skillDrafts.map((b, i) => (
            <SkillDraftCard key={i} raw={b.draft} threadId={threadId} />
          ))}
          {documentRefs.map((b, i) => (
            <DocumentRefCard key={i} raw={b.document} />
          ))}
          {legacyDocumentDrafts.map((b, i) => (
            <LegacyDocumentDraftCard key={i} raw={b.draft} />
          ))}
          <ArtifactFiles files={files} />
          <CitationSources />
          {evidenceOpen ? <EvidenceTable /> : null}
          {text ? (
            <MessageFooter text={text} pinned={latest}>
              {savedMessageId ? <MessageFeedbackButtons messageId={savedMessageId} /> : null}
              <EvidenceToggle open={evidenceOpen} onToggle={() => setEvidenceOpen((v) => !v)} />
            </MessageFooter>
          ) : null}
          {latest ? (
            <p className="mt-3 text-center text-[11.5px] leading-relaxed text-muted-foreground" data-testid="chat-disclaimer">
              {compactDisclaimer ? null : (
                <span className="hidden sm:inline">Shiki は社内文書を参照して回答します。</span>
              )}
              誤りが含まれる場合があります。
            </p>
          ) : null}
        </div>
      </Message>
    </MessageCitationsProvider>
  );
});

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
          className="inline-flex min-w-0 max-w-full items-center gap-1.5 rounded-full border border-border bg-card px-2.5 py-1 text-[12px] text-foreground/80 transition-colors hover:border-primary/40 hover:bg-secondary hover:text-foreground pointer-coarse:min-h-9 [overflow-wrap:anywhere]"
        >
          <FileDown className="size-3.5 shrink-0 text-primary" aria-hidden />
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
}: {
  stream: StreamState;
  onApproval: (approved: boolean) => void;
  threadId: string;
}) {
  const showLoader =
    !stream.text &&
    !stream.thinking &&
    stream.tools.length === 0 &&
    stream.plan.length === 0 &&
    !stream.approval;
  const segments = resolveStreamSegments(stream.segments, stream.tools);
  const last = segments[segments.length - 1];
  const waiting = useStreamWaiting({
    text: stream.text,
    thinking: stream.thinking,
    tools: stream.tools,
    tail: !last ? null : last.kind === "text" ? last : { kind: "work", hasTools: last.tools.length > 0 },
    awaitingUser: stream.approval !== null,
  });
  return (
    <MessageCitationsProvider messageKey="streaming" citations={stream.citations} text={stream.text}>
      <Message className="justify-start">
        <div className="w-full min-w-0 space-y-2">
          {stream.plan.length > 0 ? <PlanPanel subtasks={stream.plan} /> : null}
          {/* 本文と操作のまとまりを起きた順に並べ、生成中の表示は末尾に置く（往復する回答で
              「いま何をしているか」が先頭に取り残されない）。 */}
          <MessageTimeline
            segments={segments}
            citations={stream.citations}
            streaming
            phase={runningSubtask(stream.plan)}
            waitingLabel={waiting.label}
          />
          {showLoader ? (
            <MessageContent className="py-1">
              <Loader variant="typing" />
            </MessageContent>
          ) : null}
          {waiting.atBottom ? <StreamWaitingLine /> : null}
          {stream.budget ? <BudgetBanner {...stream.budget} /> : null}
          {stream.approval ? (
            <ApprovalCard
              request={stream.approval}
              pending={stream.approvalPending}
              onDecision={onApproval}
            />
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
          {stream.skillDrafts.map((draft, i) => (
            <SkillDraftCard key={i} raw={draft} threadId={threadId} />
          ))}
          {stream.documentRefs.map((document, i) => (
            <DocumentRefCard key={i} raw={document} />
          ))}
          <ArtifactFiles files={stream.files} />
        </div>
      </Message>
    </MessageCitationsProvider>
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
