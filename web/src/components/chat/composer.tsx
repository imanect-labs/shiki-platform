"use client";

import * as React from "react";
import { ArrowUp, Bot, HardDrive, Loader2, Paperclip, Sparkles, Square, TextSelect, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { uploadNewFile, type NodeResponse } from "@/lib/storage";
import type { Attachment, AutonomousMode, WorkspaceChoice } from "@/lib/chat-api";
import { HOME_SCOPE, useModelChoice } from "@/lib/model-choice";
import { selectableTool, useToolChoice } from "@/lib/tool-choice";
import { previewKind } from "@/lib/file-preview";
import {
  readScoped,
  removeScoped,
  storageScopeGeneration,
  subscribeStorageScope,
  useStorageScopeReady,
  writeScoped,
} from "@/lib/user-storage";
import { useMediaQuery } from "@/hooks/use-media-query";
import { ApprovalModeSelect, approvalModeDef } from "./approval-mode-select";
import { AttachmentThumb, isImageAttachment } from "./attachment-thumb";
import { ComposerModeButton } from "./composer-mode";
import { PlusMenu } from "./composer-plus-menu";
import { ComposerTokens, TOKEN_GAP, type ComposerToken } from "./composer-tokens";
import { ModelButton } from "./model-picker";
import {
  clearPendingSelection,
  selectionKindLabel,
  takePendingSelection,
  usePendingSelection,
  type SelectionContext,
} from "@/lib/selection-context";
import { FolderPicker } from "@/components/artifacts/folder-picker";
import { PromptInput, PromptInputTextarea } from "@/components/prompt-kit/prompt-input";
import {
  SlashCommandMenu,
  slashOptionId,
  SLASH_LISTBOX_ID,
} from "./slash-command-menu";
import {
  composeText,
  fetchSkillCatalog,
  matchSuggestions,
  resolveTypedCommand,
  toSuggestions,
  type ActiveCommand,
  type SlashSuggestion,
} from "@/lib/slash-command";
import { toast } from "@/components/ui/use-toast";
import { DrivePicker } from "./drive-picker";

/// 小さな × の押せる範囲を、見た目を変えずに周囲へ広げる（指で外せるように）。
const HIT_AREA = "relative after:absolute after:-inset-2";

/// アップロード中のファイル。画像なら端末のファイルから作った object URL を持つ。
type Uploading = { name: string; fraction: number; preview?: string };
/// 入力欄の添付。端末から選んだ画像は、アップロード後も object URL でサムネイルを出す（送信時に外す）。
type ComposerAttachment = Attachment & { preview?: string };

/// 補完候補を確定済みコマンドへ写す。
function toActiveCommand(s: SlashSuggestion): ActiveCommand {
  return {
    token: s.token,
    command: s.command,
    args: s.args,
    skillName: s.skillName,
    skillId: s.skillId,
    skillVersion: s.skillVersion,
    hint: s.hint,
  };
}

/// 書きかけの保存を待つ時間（打鍵が止まってから書く）。
const DRAFT_SAVE_DELAY_MS = 400;
/// 保存する書きかけの上限（文字数）。超える本文は保存しない（Web Storage の容量を食い潰さない）。
const DRAFT_MAX_CHARS = 100_000;

/// 書きかけを保存する。空・上限超過は保存値を消す（古い書きかけを後で戻さないため）。
/// `generation` は復元したときの世代。その後にログアウトがあれば書かない（次の人のキーへ移さない）。
function saveDraft(key: string, value: string, generation: number): void {
  if (generation !== storageScopeGeneration()) return;
  try {
    if (value && value.length <= DRAFT_MAX_CHARS) writeScoped(key, value);
    else removeScoped(key);
  } catch {
    /* 容量超過・プライベートモード等は保存しない（入力はそのまま続けられる） */
  }
}

/// チャット入力。ローカル/ドライブからの添付、送信を担う。
/// 添付ファイルはアップロード後に自動でベクトル化され、doc_search の対象になる。
///
/// レイアウトは 1 つの DOM をグリッドで組み替える（PC とスマホでメニューや状態を二重に持たない）:
/// - PC・スマホとも 2 段: 入力欄の下に操作の段（左に +〔PC はエージェントモード・ワークスペース・
///   承認モードも〕、右にモデル ／〔スマホは実行モード〕／ 送信）。ChatGPT の入力欄と同じ並び（#768）。
/// - 選んだツールとスキル（スラッシュコマンド）は、本文の先頭に入る語として見せる（composer-tokens）。
/// - モデルは送信の左に常に出す（既定は「Auto」）。
///
/// 会話のトークンごとに親が再描画されても描き直さないよう memo する（入力中の状態は自前で持つ）。
export const Composer = React.memo(function Composer({
  onSubmit,
  onStop,
  placeholder,
  autoFocus = false,
  disabled = false,
  streaming = false,
  autonomous = false,
  onAutonomousChange,
  workspace = null,
  onWorkspaceChange,
  approvalMode = null,
  onApprovalModeChange,
  bypassAllowed = true,
  threadId,
  scope,
  draftKey,
  onHasInputChange,
  restoreOnError = false,
  className,
}: {
  onSubmit: (
    text: string,
    attachments: Attachment[],
    /// エディタの選択コンテキスト（選択→AI 指示・Task 11.10。無ければ undefined）。
    context?: SelectionContext,
    /// 確定中のスラッシュコマンド（#387）。呼び出し側が skill のピンとモードを整える。
    command?: ActiveCommand,
  ) => void | Promise<void>;
  /// 指定すると、`onSubmit` が返した Promise が reject したときに送った入力（本文・添付・コマンド）を
  /// 入力欄へ戻す（送れなかった発話を打ち直させない）。
  restoreOnError?: boolean;
  /// 生成中に停止する（指定時は送信ボタンが停止ボタンに変わる）。
  onStop?: () => void;
  placeholder?: string;
  autoFocus?: boolean;
  /// 入力自体を不可にするハード無効化（生成中とは別。生成中も入力にフォーカス・タイプできる）。
  disabled?: boolean;
  /// 生成中フラグ。入力は可能なまま、送信はできず停止ボタンを出す。
  streaming?: boolean;
  /// エージェントモード（既定 OFF＝通常チャット。ON＝ワークスペース＋計画＋承認の長ホライズン）。
  /// 通常チャットでもモデルはツールを裁量発火する（issue #102）ため「自動」トグルは無い。
  autonomous?: boolean;
  /// エージェントモードのトグル（未指定ならトグル UI を出さない）。
  onAutonomousChange?: (v: boolean) => void;
  /// エージェントモードのワークスペース作成場所（未選択は Drive 直下）。
  workspace?: WorkspaceChoice | null;
  /// ワークスペース選択のハンドラ（未指定ならチップを出さない）。
  onWorkspaceChange?: (w: WorkspaceChoice | null) => void;
  /// 自律 run の承認モード（承認必須/オート/全自動・#350）。null は未ロード（セレクタ非表示）。
  approvalMode?: AutonomousMode | null;
  /// 承認モード変更のハンドラ（未指定ならセレクタを出さない）。生成中も切替可（実行中トグル）。
  onApprovalModeChange?: (m: AutonomousMode) => void;
  /// org 管理者ポリシで bypass（全自動）を選べるか（false なら選択肢を無効化・#350）。
  bypassAllowed?: boolean;
  /// スレッド内のコンポーザなら thread id。カタログにスレッドのピンを統合するのに使う
  /// （モデルが見ている集合と補完候補を一致させる・#387）。ホームでは未指定。
  threadId?: string;
  /// モデル・ツールの選択の保存先。未指定ならスレッド（無ければホーム）。まだスレッドの無い
  /// 入力欄（文書のアシスタントパネル等）がホームの選択を共有しないように分ける。
  scope?: string;
  /// 指定すると、書きかけの本文をユーザー単位で保存し、次にこの入力欄を開いたときに戻す
  /// （ホームの新規チャット: 入力 → 別画面 → 戻ると続きから書ける）。送信で消える。
  draftKey?: string;
  /// 送れる入力（本文・確定済みのコマンド・添付）があるかが変わったときに呼ぶ
  /// （入力途中かどうかを親が知る必要があるとき）。
  onHasInputChange?: (hasInput: boolean) => void;
  className?: string;
}) {
  const [value, setValue] = React.useState("");
  const [attachments, setAttachments] = React.useState<ComposerAttachment[]>([]);
  const [uploading, setUploading] = React.useState<Uploading | null>(null);
  const attachmentsRef = React.useRef(attachments);
  attachmentsRef.current = attachments;
  // 作った object URL。画面を離れたら、アップロード中のものも含めて手放す。
  const previewUrls = React.useRef(new Set<string>());
  React.useEffect(() => {
    const urls = previewUrls.current;
    return () => {
      urls.forEach((u) => URL.revokeObjectURL(u));
      urls.clear();
    };
  }, []);
  const revokePreview = (url?: string) => {
    if (!url) return;
    URL.revokeObjectURL(url);
    previewUrls.current.delete(url);
  };
  const [menuOpen, setMenuOpen] = React.useState(false);
  const desktop = useMediaQuery("(min-width: 768px)");
  const modelScope = scope ?? threadId ?? HOME_SCOPE;
  // スマホでモデルを選んでいない（Auto・組織の既定の）ときは、入力欄にモデルを出さずに 1 行の
  // バーにする（「＋」のシートから選ぶ）。PC は常に 2 段。初回描画とずれないよう切替は max-md: で行う。
  const [modelChoice] = useModelChoice(modelScope);
  const oneLine = modelChoice === null;
  const [modelOpen, setModelOpen] = React.useState(false);
  // この発話に使うツール（#734）。送信時に会話側が読み出して空に戻す。
  const toolChoice = useToolChoice(modelScope);
  const [pickerOpen, setPickerOpen] = React.useState(false);
  const [wsPickerOpen, setWsPickerOpen] = React.useState(false);
  const fileInputRef = React.useRef<HTMLInputElement | null>(null);

  // 書きかけの保存と復元（draftKey 指定時）。保存先はユーザー単位（ログアウトで消える・#817）
  // なので、ユーザーが確定してから読む。保存は復元の後から（復元前の空で保存値を消さない）。
  const scopeReady = useStorageScopeReady();
  const [draftRestored, setDraftRestored] = React.useState(false);
  // 復元したときの世代。ログアウト（他タブを含む）で世代が進んだら、前の人の書きかけを
  // 入力欄から捨て、次の人のキーへ書かない（#817）。
  const draftGeneration = React.useRef(0);
  React.useEffect(() => {
    if (!draftKey) return;
    return subscribeStorageScope(() => {
      if (storageScopeGeneration() === draftGeneration.current) return;
      setValue("");
      setDraftRestored(false);
    });
  }, [draftKey]);
  React.useEffect(() => {
    if (!draftKey || !scopeReady || draftRestored) return;
    draftGeneration.current = storageScopeGeneration();
    const saved = readScoped(draftKey);
    // 確定を待つ間に打ち始めていたら、そちらを優先する（復元の完了で保存される）。
    if (saved) setValue((v) => v || saved);
    setDraftRestored(true);
  }, [draftKey, scopeReady, draftRestored]);
  // 保存は打鍵ごとではなく、手が止まってからまとめて書く（長文の貼り付け後も入力を重くしない）。
  // 画面を離れる（アンマウント・リロードやタブを閉じる）ときは待たずに書く（直後に離れても取りこぼさない）。
  const latestValue = React.useRef(value);
  // コミットした値だけを持つ（破棄されたレンダーの値を保存しない）。保存の effect より先に置く。
  React.useEffect(() => {
    latestValue.current = value;
  }, [value]);
  React.useEffect(() => {
    if (!draftKey || !draftRestored) return;
    const generation = draftGeneration.current;
    const t = window.setTimeout(() => saveDraft(draftKey, value, generation), DRAFT_SAVE_DELAY_MS);
    return () => window.clearTimeout(t);
  }, [draftKey, draftRestored, value]);
  React.useEffect(() => {
    if (!draftKey || !draftRestored) return;
    const generation = draftGeneration.current;
    const flush = () => saveDraft(draftKey, latestValue.current, generation);
    window.addEventListener("pagehide", flush);
    return () => {
      window.removeEventListener("pagehide", flush);
      flush();
    };
  }, [draftKey, draftRestored]);

  // スラッシュコマンド（#387）。候補はカタログ（モデルが見ているものと同一）由来。
  const [suggestions, setSuggestions] = React.useState<SlashSuggestion[]>([]);
  const [command, setCommand] = React.useState<ActiveCommand | null>(null);
  const [slashIndex, setSlashIndex] = React.useState(0);
  const [slashDismissed, setSlashDismissed] = React.useState(false);

  // コマンドを確定すると本文は空になるので、本文だけでなくコマンド・添付（アップロード中を含む）も
  // 入力として数える。
  const hasInput =
    uploading !== null || value.trim().length > 0 || command !== null || attachments.length > 0;
  React.useEffect(() => {
    onHasInputChange?.(hasInput);
  }, [onHasInputChange, hasInput]);

  React.useEffect(() => {
    let active = true;
    // カタログが引けない環境（chat 無効等）でもコンポーザは壊さない＝候補ゼロで動く。
    fetchSkillCatalog(threadId)
      .then((items) => active && setSuggestions(toSuggestions(items)))
      .catch(() => active && setSuggestions([]));
    return () => {
      active = false;
    };
  }, [threadId]);

  // 確定済みコマンドがある間は補完を出さない（ピルが既に状態を示している）。
  const slashHits = command || slashDismissed ? null : matchSuggestions(value, suggestions);
  const slashOpen = (slashHits?.length ?? 0) > 0;
  const activeSlash = slashOpen ? slashHits![Math.min(slashIndex, slashHits!.length - 1)] : null;

  // エディタの選択コンテキスト（選択→AI 指示・Task 11.10）。チップ表示し送信時に消費する。
  const selection = usePendingSelection();

  // コマンド確定後は本文が空でも送れる（コマンド自体が指示になる）。
  // 生成中でも送信できる（サーバが 1 スレッド 1 本ずつ直列化し、順番待ちとして受け付ける）。
  // 「返答を待たないと次を書けない」体験を作らないための土台。
  const canSend = (value.trim().length > 0 || command !== null) && !disabled && !uploading;

  const pickSuggestion = (s: SlashSuggestion) => {
    setCommand(toActiveCommand(s));
    // コマンド部分は入力欄から取り除き、以降はピルが持つ（本文だけを打てる状態にする）。
    setValue("");
    setSlashIndex(0);
  };

  const submit = () => {
    if (disabled || uploading) return;
    // 補完で確定していなくても、既知のコマンドが直接打たれていれば拾う（#387・Codex P2）。
    // 拾わないと skill のピン処理を通らず、コマンドがただのテキストとして流れる。
    const typed = command ? null : resolveTypedCommand(value, suggestions);
    const active = command ?? (typed ? toActiveCommand(typed.suggestion) : null);
    const body = typed ? typed.rest : value;
    const text = composeText(active, body);
    if (!text) return;
    // 送るのは node と名前だけ（サムネイルの object URL は入力欄の中だけで使い、送ったら手放す）。
    const sentAttachments = attachments.map(({ node_id, name }) => ({ node_id, name }));
    const result = onSubmit(text, sentAttachments, takePendingSelection() ?? undefined, active ?? undefined);
    setValue("");
    attachments.forEach((a) => revokePreview(a.preview));
    setAttachments([]);
    setCommand(null);
    setSlashDismissed(false);
    if (restoreOnError && result) {
      result.catch(() => {
        // 失敗を待つ間に打ち直していたら、そちらを優先する。
        setValue((v) => v || body);
        setAttachments((a) => (a.length > 0 ? a : sentAttachments));
        setCommand((c) => c ?? active);
      });
    }
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // 本文の先頭での Backspace は、見えている最後の語を外す（ChatGPT と同じ操作感）。
    const el = e.currentTarget;
    const last = tokens[tokens.length - 1];
    if (
      last &&
      e.key === "Backspace" &&
      !e.nativeEvent.isComposing &&
      e.keyCode !== 229 &&
      el.selectionStart === 0 &&
      el.selectionEnd === 0
    ) {
      e.preventDefault();
      last.onRemove();
      return;
    }
    if (!slashOpen) {
      // `/` を打ち直したら再び候補を出す。
      if (e.key !== "Escape") setSlashDismissed(false);
      return;
    }
    const len = slashHits!.length;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSlashIndex((i) => (i + 1) % len);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSlashIndex((i) => (i - 1 + len) % len);
    } else if (
      (e.key === "Enter" && !e.nativeEvent.isComposing && e.keyCode !== 229) ||
      e.key === "Tab"
    ) {
      e.preventDefault();
      if (activeSlash) pickSuggestion(activeSlash);
    } else if (e.key === "Escape") {
      e.preventDefault();
      setSlashDismissed(true);
    }
  };

  const addAttachment = (node: NodeResponse, preview?: string) => {
    if (attachmentsRef.current.some((a) => a.node_id === node.id)) {
      revokePreview(preview);
      return;
    }
    setAttachments((prev) => [...prev, { node_id: node.id, name: node.name, preview }]);
  };

  const removeAttachment = (a: ComposerAttachment) => {
    revokePreview(a.preview);
    setAttachments((prev) => prev.filter((x) => x.node_id !== a.node_id));
  };

  const onPickLocal = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    // スマホの「+」のシートはファイルを選ぶまで開いておき、選んだら閉じる。
    setMenuOpen(false);
    // 画像はアップロードを待たずに、端末のファイルからサムネイルを出す。
    let preview: string | undefined;
    if (previewKind({ name: file.name }) === "image") {
      preview = URL.createObjectURL(file);
      previewUrls.current.add(preview);
    }
    setUploading({ name: file.name, fraction: 0, preview });
    try {
      // 同じ名前があれば連番名で置く（同じ写真をもう一度添付すると 409 で失敗していた・#880）。
      const node = await uploadNewFile({
        file,
        onProgress: (fraction) => setUploading({ name: file.name, fraction, preview }),
      });
      addAttachment(node, preview);
      toast({ description: `${node.name} をアップロードしました（自動でベクトル化されます）` });
    } catch {
      revokePreview(preview);
      toast({ description: `${file.name} のアップロードに失敗しました` });
    } finally {
      setUploading(null);
    }
  };


  // プレースホルダで「いま何に頼むか」を伝える（スマホは状態を出すボタンが小さいので特に）。
  // 1 行のバーに収まる長さにする（折り返すと入力欄が 2 行に伸びる）。
  const approvalLabel =
    autonomous && approvalMode ? approvalModeDef(approvalMode).label : null;
  const effectivePlaceholder = command
    ? (command.hint ?? `/${command.token} に依頼`)
    : onAutonomousChange && autonomous
      ? desktop
        ? `エージェントに依頼${approvalLabel ? `（${approvalLabel}）` : ""}`
        : `エージェント${approvalLabel ? `・${approvalLabel}` : "に依頼"}`
      : (placeholder ?? (desktop ? "何でも尋ねて、社内文書も検索" : "Shiki に質問・依頼"));

  // 本文の先頭に入る語（選んだスキル・ツール）。押すか、本文の先頭で Backspace を押すと外れる。
  const tokens: ComposerToken[] = [];
  if (command) {
    tokens.push({
      key: "command",
      label: `/${command.token}`,
      icon: Sparkles,
      removeLabel: "コマンドを外す",
      onRemove: () => setCommand(null),
      testId: "slash-command-pill",
    });
  }
  for (const name of toolChoice.tools) {
    const tool = selectableTool(name);
    if (!tool) continue;
    tokens.push({
      key: name,
      label: tool.label,
      icon: tool.icon,
      removeLabel: `${tool.label}を外す`,
      onRemove: () => toolChoice.clear(name),
      testId: "composer-tool-chip",
    });
  }
  const [tokensWidth, setTokensWidth] = React.useState(0);
  const [textScroll, setTextScroll] = React.useState(0);

  return (
    <div className={cn("flex flex-col gap-2", className)}>
      {/* 選択コンテキストチップ（選択→AI 指示・Task 11.10） */}
      {selection ? (
        <div
          className="flex items-center gap-2 rounded-lg border border-border/60 bg-card/40 px-3 py-1.5 text-xs"
          data-testid="selection-chip"
        >
          <TextSelect className="size-3.5 shrink-0 text-primary" aria-hidden />
          <span className="font-medium">{selectionKindLabel(selection.kind)}</span>
          <span className="min-w-0 flex-1 truncate text-muted-foreground">
            {selection.excerpt.slice(0, 120)}
          </span>
          <button
            type="button"
            onClick={clearPendingSelection}
            aria-label="選択コンテキストを外す"
            className={cn(
              "flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
              HIT_AREA,
            )}
          >
            <X className="size-3.5" aria-hidden />
          </button>
        </div>
      ) : null}
      {/* 添付チップ */}
      {(attachments.length > 0 || uploading) && (
        <div className="flex flex-wrap items-center gap-2.5 px-1 pt-1">
          {attachments.map((a) =>
            isImageAttachment(a) ? (
              <AttachmentThumb
                key={a.node_id}
                nodeId={a.node_id}
                name={a.name}
                src={a.preview}
              >
                <button
                  type="button"
                  onClick={() => removeAttachment(a)}
                  aria-label={`${a.name} の添付を外す`}
                  // 押せる範囲は隣の添付に食い込まない広さにとどめる（並びの間隔 10px）。
                  className="absolute -right-1.5 -top-1.5 flex size-5 items-center justify-center rounded-full border-2 border-background bg-foreground text-background shadow-sm transition-colors after:absolute after:-inset-1 hover:bg-foreground/80"
                >
                  <X className="size-3" strokeWidth={2.5} />
                </button>
              </AttachmentThumb>
            ) : (
              <span
                key={a.node_id}
                className="inline-flex min-w-0 max-w-full items-center gap-1.5 rounded-full border border-border bg-card py-1 pl-2.5 pr-1.5 text-[13px] text-foreground/85"
              >
                <Paperclip className="size-3.5 shrink-0 text-muted-foreground" />
                <span className="min-w-0 max-w-[160px] truncate">{a.name}</span>
                <button
                  type="button"
                  onClick={() => removeAttachment(a)}
                  aria-label={`${a.name} の添付を外す`}
                  className={cn(
                    "flex size-5 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-accent hover:text-foreground",
                    HIT_AREA,
                  )}
                >
                  <X className="size-3.5" />
                </button>
              </span>
            ),
          )}
          {uploading?.preview ? (
            <AttachmentThumb name={uploading.name} src={uploading.preview}>
              <span
                role="status"
                aria-busy="true"
                aria-label={`${uploading.name} をアップロード中`}
                className="absolute inset-0 flex flex-col items-center justify-center gap-0.5 rounded-[inherit] bg-background/45 text-[10px] font-medium tabular-nums text-foreground"
              >
                <Loader2 className="size-4 animate-spin" aria-hidden />
                {/* 進み具合は見た目だけ（変わるたびに読み上げない） */}
                <span aria-hidden>{Math.round(uploading.fraction * 100)}%</span>
              </span>
            </AttachmentThumb>
          ) : uploading ? (
            <span className="inline-flex items-center gap-1.5 rounded-full border border-border bg-card px-2.5 py-1 text-[13px] text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" />
              <span className="max-w-[160px] truncate">{uploading.name}</span>
              <span>{Math.round(uploading.fraction * 100)}%</span>
            </span>
          ) : null}
        </div>
      )}

      <PromptInput
        value={value}
        onValueChange={setValue}
        onSubmit={submit}
        disabled={disabled}
        isLoading={streaming}
        // スマホは 5 行（16px × 行高 24px ＋ 上下の余白）まで、PC は 200px まで伸ばす。
        maxHeight={desktop ? 200 : 136}
        data-testid="composer"
        className={cn(
          // 本文の上に重ねるので面を少し透かし、背後はぼかす。
          "relative rounded-[26px] border-border bg-card/85 shadow-sm backdrop-blur-xl transition-shadow",
          "focus-within:border-ring/25 focus-within:shadow-md focus-within:ring-4 focus-within:ring-ring/10",
          // 入力欄 → 操作の段 の 2 段（左に +、右にモデルと送信）。
          "grid grid-cols-[minmax(0,1fr)_auto] items-end gap-x-2 gap-y-1 p-2",
          // スマホで既定のモデルのときは 1 行の丸いバー（+・入力欄・送信）。伸びてもボタンは下端に揃える。
          oneLine && "max-md:grid-cols-[auto_minmax(0,1fr)_auto] max-md:gap-x-1 max-md:p-1.5",
        )}
      >
        {slashOpen ? (
          <SlashCommandMenu
            suggestions={slashHits!}
            activeIndex={Math.min(slashIndex, slashHits!.length - 1)}
            onPick={pickSuggestion}
            onHover={setSlashIndex}
          />
        ) : null}

        {/* 入力欄（DOM では先に置き、Tab の順を見た目の上→下に揃える）。 */}
        <div
          className={cn(
            "relative col-span-2 col-start-1 row-start-1 min-w-0 overflow-hidden px-1.5 pt-1 md:px-2",
            oneLine && "max-md:col-span-1 max-md:col-start-2 max-md:px-1 max-md:pt-0",
          )}
        >
          {tokens.length > 0 ? (
            // 1 行目の先頭に重ねる（textarea の上の余白・行高と揃える）。
            <ComposerTokens
              tokens={tokens}
              onWidthChange={setTokensWidth}
              scrollTop={textScroll}
              className={cn(
                "left-1.5 top-[calc(0.25rem+0.5rem)] h-6 text-base md:left-2 md:top-1 md:h-[1.625em] md:text-[15px]",
                oneLine && "max-md:left-1 max-md:top-2",
              )}
            />
          ) : null}
          {/* 補完中は combobox として関連付ける（Codex P2）。フォーカスは textarea に
              残るため、aria-activedescendant が無いとスクリーンリーダーへ選択が伝わらない。 */}
          <PromptInputTextarea
            placeholder={effectivePlaceholder}
            autoFocus={autoFocus}
            aria-label="メッセージを入力"
            role={slashOpen ? "combobox" : undefined}
            aria-expanded={slashOpen || undefined}
            aria-controls={slashOpen ? SLASH_LISTBOX_ID : undefined}
            aria-autocomplete={slashOpen ? "list" : undefined}
            aria-activedescendant={
              slashOpen ? slashOptionId(Math.min(slashIndex, slashHits!.length - 1)) : undefined
            }
            onKeyDown={onKeyDown}
            onScroll={(e) => setTextScroll(e.currentTarget.scrollTop)}
            // 先頭の語の幅だけ 1 行目を下げる（続けて打った文字は語の右から、2 行目以降は左端から）。
            style={tokens.length > 0 ? { textIndent: tokensWidth + TOKEN_GAP } : undefined}
            className={cn(
              "min-h-10 min-w-0 px-0 py-2 text-base leading-6 placeholder:text-muted-foreground/70",
              // 長いプレースホルダは折り返さずに切る（入力欄を 2 行に押し広げない）。
              "placeholder-shown:overflow-hidden placeholder-shown:text-ellipsis placeholder-shown:whitespace-nowrap",
              "md:min-h-[44px] md:pb-1 md:pt-0 md:text-[15px] md:leading-relaxed",
            )}
          />
        </div>

        {/* 左: 「+」（PC はエージェントモード等も同じ段に並べる）。 */}
        <div
          className={cn(
            "col-start-1 row-start-2 flex min-w-0 items-center gap-1.5 pb-0.5 pl-0.5",
            oneLine && "max-md:row-start-1 max-md:p-0",
          )}
        >
          <PlusMenu
            open={menuOpen}
            onOpenChange={setMenuOpen}
            onUploadLocal={() => fileInputRef.current?.click()}
            onOpenDrive={() => setPickerOpen(true)}
            skills={suggestions}
            onPickSkill={pickSuggestion}
            // ワークスペース（作業フォルダ）はエージェントモード ON のときだけ意味を持つ。
            // OFF のときは残存する workspace 選択を無視し、既定（マイドライブ直下）へ作成する。
            createParentId={autonomous ? (workspace?.folderId ?? null) : null}
            modelScope={modelScope}
            onOpenModel={oneLine ? () => setModelOpen(true) : undefined}
          />
          {onAutonomousChange ? (
            <div className="hidden min-w-0 items-center gap-1.5 md:flex">
              <button
                type="button"
                role="switch"
                aria-checked={autonomous}
                aria-label="エージェントモード"
                title={
                  autonomous
                    ? "エージェントモード: ON（ワークスペースで計画・実行・承認つきに目標を達成）"
                    : "エージェントモード: OFF（通常チャット・必要に応じてツールは自動で使われます）"
                }
                onClick={() => onAutonomousChange(!autonomous)}
                className={cn(
                  "inline-flex h-9 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border px-3 text-[13px] font-medium transition-colors",
                  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
                  autonomous
                    ? "border-primary/40 bg-primary/10 text-primary"
                    : "border-border text-foreground/70 hover:bg-secondary hover:text-foreground",
                )}
              >
                <Bot className="size-[15px]" aria-hidden />
                エージェントモード
              </button>
              {/* エージェントモード ON 時のみ: ワークスペースの作成場所を選べる */}
              {autonomous && onWorkspaceChange ? (
                <button
                  type="button"
                  onClick={() => setWsPickerOpen(true)}
                  title="エージェントが作業するワークスペースの場所を選ぶ"
                  className="inline-flex h-9 min-w-0 items-center gap-1.5 whitespace-nowrap rounded-full border border-border px-3 text-[13px] text-foreground/70 transition-colors hover:bg-secondary hover:text-foreground"
                >
                  <HardDrive className="size-[15px] shrink-0" aria-hidden />
                  <span className="max-w-[160px] truncate">
                    {workspace ? workspace.folderName : "マイドライブ"}
                  </span>
                </button>
              ) : null}
              {/* エージェントモード ON 時のみ: 承認モード（承認必須/オート/全自動・#350）。
                  生成中も切替可（承認待ちカードはモード緩和で自動解決される）。 */}
              {autonomous && approvalMode && onApprovalModeChange ? (
                <ApprovalModeSelect
                  mode={approvalMode}
                  onChange={onApprovalModeChange}
                  bypassAllowed={bypassAllowed}
                />
              ) : null}
            </div>
          ) : null}
        </div>

        {/* 右: スマホは実行モード → 送信。生成中は停止を出し、書きかけがあれば送信（＝順番待ちへ
            積む）も並べる。書きかけが無いのに無効な送信ボタンを置くと、押せないボタンが常駐して邪魔。 */}
        <div
          className={cn(
            "col-start-2 row-start-2 flex min-w-0 items-center justify-end gap-1 pb-0.5 pr-0.5 md:gap-1.5",
            oneLine && "max-md:col-start-3 max-md:row-start-1 max-md:p-0",
          )}
        >
          <ModelButton
            scope={modelScope}
            className={cn("max-w-[11rem] md:max-w-[16rem]", oneLine && "max-md:hidden")}
            open={modelOpen}
            onOpenChange={setModelOpen}
            sheetOnly={oneLine && !desktop}
          />
          {onAutonomousChange ? (
            <ComposerModeButton
              className="md:hidden"
              autonomous={autonomous}
              onAutonomousChange={onAutonomousChange}
              approvalMode={approvalMode}
              onApprovalModeChange={onApprovalModeChange}
              bypassAllowed={bypassAllowed}
              workspace={workspace}
              onPickWorkspace={onWorkspaceChange ? () => setWsPickerOpen(true) : undefined}
            />
          ) : null}
          {streaming ? (
            <button
              type="button"
              onClick={onStop}
              aria-label="生成を停止"
              title="生成を停止"
              className={cn(
                "flex size-10 shrink-0 items-center justify-center rounded-full bg-foreground text-background transition-colors hover:bg-foreground/85 md:size-9",
                "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
              )}
            >
              <Square className="size-3.5 fill-current" aria-hidden />
            </button>
          ) : null}
          {!streaming || canSend ? (
            <button
              type="button"
              onClick={submit}
              disabled={!canSend}
              aria-label={streaming ? "順番待ちに追加" : "送信"}
              title={streaming ? "生成が終わったら自動で送信します" : undefined}
              className={cn(
                "flex size-10 shrink-0 items-center justify-center rounded-full transition-colors md:size-9",
                "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
                canSend
                  ? "bg-primary text-primary-foreground hover:bg-primary/90"
                  : "bg-muted text-muted-foreground",
              )}
            >
              <ArrowUp className="size-5 md:size-[18px]" aria-hidden />
            </button>
          ) : null}
        </div>
      </PromptInput>

      <input
        ref={fileInputRef}
        type="file"
        className="hidden"
        onChange={onPickLocal}
        aria-hidden
      />
      <DrivePicker open={pickerOpen} onOpenChange={setPickerOpen} onSelect={(node) => addAttachment(node)} />
      {onWorkspaceChange ? (
        <FolderPicker
          open={wsPickerOpen}
          onOpenChange={setWsPickerOpen}
          purpose="workspace"
          onSelect={(f) =>
            onWorkspaceChange({ mode: f.mode, folderId: f.id, folderName: f.name })
          }
        />
      ) : null}
    </div>
  );
});
