"use client";

import * as React from "react";
import {
  ArrowUp,
  FilePlus2,
  FileSpreadsheet,
  FileText,
  Bot,
  Loader2,
  HardDrive,
  NotebookPen,
  Paperclip,
  Plus,
  Presentation,
  Sparkles,
  Square,
  TextSelect,
  Upload,
  X,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { uploadFile, type NodeResponse } from "@/lib/storage";
import type { Attachment, AutonomousMode, WorkspaceChoice } from "@/lib/chat-api";
import { ApprovalModeSelect } from "./approval-mode-select";
import {
  clearPendingSelection,
  selectionKindLabel,
  takePendingSelection,
  usePendingSelection,
  type SelectionContext,
} from "@/lib/selection-context";
import { FolderPicker } from "@/components/artifacts/folder-picker";
import {
  PromptInput,
  PromptInputActions,
  PromptInputTextarea,
} from "@/components/prompt-kit/prompt-input";
import {
  SlashCommandMenu,
  SlashCommandPill,
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
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { toast } from "@/components/ui/use-toast";
import { useCreateContent } from "@/hooks/use-create-content";
import { DrivePicker } from "./drive-picker";

type Uploading = { name: string; fraction: number };

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

/// 「+」メニューに出すスキルコマンドの最大件数（応急処置・#390 で出し方を再設計する）。
/// 絞り込み手段が無いメニューに全件を並べると、本来の「添付」「作成」が押し出される。
const MAX_MENU_SKILLS = 5;

/// チャット入力。ローカル/ドライブからの添付、送信を担う。
/// 添付ファイルはアップロード後に自動でベクトル化され、doc_search の対象になる。
export function Composer({
  onSubmit,
  onStop,
  placeholder = "何でも尋ねて、社内文書も検索",
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
  className,
}: {
  onSubmit: (
    text: string,
    attachments: Attachment[],
    /// エディタの選択コンテキスト（選択→AI 指示・Task 11.10。無ければ undefined）。
    context?: SelectionContext,
    /// 確定中のスラッシュコマンド（#387）。呼び出し側が skill のピンとモードを整える。
    command?: ActiveCommand,
  ) => void;
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
  className?: string;
}) {
  const [value, setValue] = React.useState("");
  const [attachments, setAttachments] = React.useState<Attachment[]>([]);
  const [uploading, setUploading] = React.useState<Uploading | null>(null);
  const [menuOpen, setMenuOpen] = React.useState(false);
  const [pickerOpen, setPickerOpen] = React.useState(false);
  const [wsPickerOpen, setWsPickerOpen] = React.useState(false);
  const fileInputRef = React.useRef<HTMLInputElement | null>(null);

  // スラッシュコマンド（#387）。候補はカタログ（モデルが見ているものと同一）由来。
  const [suggestions, setSuggestions] = React.useState<SlashSuggestion[]>([]);
  const [command, setCommand] = React.useState<ActiveCommand | null>(null);
  const [slashIndex, setSlashIndex] = React.useState(0);
  const [slashDismissed, setSlashDismissed] = React.useState(false);

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
  const canSend = (value.trim().length > 0 || command !== null) && !disabled && !uploading && !streaming;

  const pickSuggestion = (s: SlashSuggestion) => {
    setCommand(toActiveCommand(s));
    // コマンド部分は入力欄から取り除き、以降はピルが持つ（本文だけを打てる状態にする）。
    setValue("");
    setSlashIndex(0);
  };

  const submit = () => {
    if (disabled || uploading || streaming) return;
    // 補完で確定していなくても、既知のコマンドが直接打たれていれば拾う（#387・Codex P2）。
    // 拾わないと skill のピン処理を通らず、コマンドがただのテキストとして流れる。
    const typed = command ? null : resolveTypedCommand(value, suggestions);
    const active = command ?? (typed ? toActiveCommand(typed.suggestion) : null);
    const body = typed ? typed.rest : value;
    const text = composeText(active, body);
    if (!text) return;
    onSubmit(text, attachments, takePendingSelection() ?? undefined, active ?? undefined);
    setValue("");
    setAttachments([]);
    setCommand(null);
    setSlashDismissed(false);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // キャレット先頭での Backspace はピルを外す（ChatGPT と同じ操作感）。
    if (e.key === "Backspace" && command && value.length === 0) {
      e.preventDefault();
      setCommand(null);
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
    } else if ((e.key === "Enter" && !e.nativeEvent.isComposing) || e.key === "Tab") {
      e.preventDefault();
      if (activeSlash) pickSuggestion(activeSlash);
    } else if (e.key === "Escape") {
      e.preventDefault();
      setSlashDismissed(true);
    }
  };

  const addAttachment = (node: NodeResponse) => {
    setAttachments((prev) =>
      prev.some((a) => a.node_id === node.id) ? prev : [...prev, { node_id: node.id, name: node.name }],
    );
  };

  const onPickLocal = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    setUploading({ name: file.name, fraction: 0 });
    try {
      const node = await uploadFile({
        file,
        onProgress: (fraction) => setUploading({ name: file.name, fraction }),
      });
      addAttachment(node);
      toast({ description: `${file.name} をアップロードしました（自動でベクトル化されます）` });
    } catch {
      toast({ description: `${file.name} のアップロードに失敗しました` });
    } finally {
      setUploading(null);
    }
  };

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
            className="flex size-5 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            <X className="size-3.5" aria-hidden />
          </button>
        </div>
      ) : null}
      {/* 添付チップ */}
      {(attachments.length > 0 || uploading) && (
        <div className="flex flex-wrap gap-1.5 px-1">
          {attachments.map((a) => (
            <span
              key={a.node_id}
              className="inline-flex items-center gap-1.5 rounded-full border border-border bg-card px-2.5 py-1 text-[13px] text-foreground/85"
            >
              <Paperclip className="size-3.5 text-muted-foreground" />
              <span className="max-w-[160px] truncate">{a.name}</span>
              <button
                type="button"
                onClick={() => setAttachments((prev) => prev.filter((x) => x.node_id !== a.node_id))}
                aria-label="添付を外す"
                className="text-muted-foreground hover:text-foreground"
              >
                <X className="size-3.5" />
              </button>
            </span>
          ))}
          {uploading ? (
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
        maxHeight={200}
        className={cn(
          "relative rounded-[26px] border-border bg-card shadow-sm transition-shadow",
          "focus-within:border-ring/25 focus-within:shadow-md focus-within:ring-4 focus-within:ring-ring/10",
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
        {/* 確定したコマンドはピルで示す（入力欄からは消え、以降は本文だけを打つ）。 */}
        <div className="flex items-start gap-2 px-3 pt-2">
          {command ? (
            <SlashCommandPill label={command.token} onClear={() => setCommand(null)} />
          ) : null}
          {/* 補完中は combobox として関連付ける（Codex P2）。フォーカスは textarea に
              残るため、aria-activedescendant が無いとスクリーンリーダーへ選択が伝わらない。 */}
          <PromptInputTextarea
            placeholder={command?.hint ?? placeholder}
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
            className="min-w-0 flex-1 px-0 pt-0 pb-1 text-[15px] leading-relaxed placeholder:text-muted-foreground/70"
          />
        </div>

        <PromptInputActions className="justify-between px-1 pb-1">
          {/* 左下: 「+」一つに集約（添付）。狭い列でも潰れない。 */}
          <div className="flex min-w-0 items-center gap-1.5">
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
            />
            {onAutonomousChange ? (
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
                  "inline-flex h-9 items-center gap-1.5 rounded-full border px-3 text-[13px] font-medium transition-colors",
                  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
                  autonomous
                    ? "border-primary/40 bg-primary/10 text-primary"
                    : "border-border text-foreground/70 hover:bg-secondary hover:text-foreground",
                )}
              >
                <Bot className="size-[15px]" aria-hidden />
                エージェントモード
              </button>
            ) : null}
            {/* エージェントモード ON 時のみ: ワークスペースの作成場所を選べる */}
            {onAutonomousChange && autonomous && onWorkspaceChange ? (
              <button
                type="button"
                onClick={() => setWsPickerOpen(true)}
                title="エージェントが作業するワークスペースの場所を選ぶ"
                className="inline-flex h-9 min-w-0 items-center gap-1.5 rounded-full border border-border px-3 text-[13px] text-foreground/70 transition-colors hover:bg-secondary hover:text-foreground"
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

          <div className="flex items-center gap-1.5">
            {streaming ? (
              <button
                type="button"
                onClick={onStop}
                aria-label="生成を停止"
                title="生成を停止"
                className={cn(
                  "flex size-9 items-center justify-center rounded-full bg-foreground text-background transition-colors hover:bg-foreground/85",
                  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
                )}
              >
                <Square className="size-3.5 fill-current" aria-hidden />
              </button>
            ) : (
              <button
                type="button"
                onClick={submit}
                disabled={!canSend}
                aria-label="送信"
                className={cn(
                  "flex size-9 items-center justify-center rounded-full transition-colors",
                  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
                  canSend
                    ? "bg-primary text-primary-foreground hover:bg-primary/90"
                    : "bg-muted text-muted-foreground",
                )}
              >
                <ArrowUp className="size-[18px]" aria-hidden />
              </button>
            )}
          </div>
        </PromptInputActions>
      </PromptInput>

      <input
        ref={fileInputRef}
        type="file"
        className="hidden"
        onChange={onPickLocal}
        aria-hidden
      />
      <DrivePicker open={pickerOpen} onOpenChange={setPickerOpen} onSelect={addAttachment} />
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
}

/// 左下「+」の統合メニュー。添付（ローカル/ドライブ）と「作成」サブメニュー（#333）を集約する。
/// Radix DropdownMenu でポータル描画＋衝突回避するため、画面中央のコンポーザ（ホーム）でも
/// メニューがビューポート上端に食い込まず、収まらない高さは内部スクロールに落ちる。
function PlusMenu({
  open,
  onOpenChange,
  onUploadLocal,
  onOpenDrive,
  createParentId = null,
  skills = [],
  onPickSkill,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  onUploadLocal: () => void;
  onOpenDrive: () => void;
  /// 「作成」の保存先フォルダ（エージェントモードのワークスペース選択時はそれ・既定はマイドライブ直下）。
  createParentId?: string | null;
  /// スラッシュコマンドを持つスキル（#387）。選ぶと入力欄へコマンドが打ち込まれる。
  skills?: SlashSuggestion[];
  onPickSkill?: (s: SlashSuggestion) => void;
}) {
  // 作成ロジックはドライブの「新規作成」と共通（use-create-content・重複実装しない）。
  const {
    createNoteAndOpen,
    createDocumentAndOpen,
    createSheetAndOpen,
    createSlideAndOpen,
    createCsvAndOpen,
  } =
    useCreateContent({ parentId: createParentId });
  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label="追加メニューを開く"
          title="追加（添付・作成）"
          className={cn(
            "flex size-9 items-center justify-center rounded-full border transition-colors",
            open
              ? "border-foreground/30 bg-secondary text-foreground"
              : "border-border text-foreground/70 hover:bg-secondary hover:text-foreground",
          )}
        >
          <Plus className={cn("size-[18px] transition-transform", open && "rotate-45")} aria-hidden />
        </button>
      </DropdownMenuTrigger>

      <DropdownMenuContent
        side="top"
        align="start"
        sideOffset={8}
        collisionPadding={12}
        className="max-h-[var(--radix-dropdown-menu-content-available-height)] w-72 overflow-y-auto p-1.5"
      >
        {/* 添付 */}
        <DropdownMenuLabel className="uppercase tracking-wide">添付</DropdownMenuLabel>
        <DropdownMenuItem className="gap-2.5 px-2.5 py-2" onSelect={onUploadLocal}>
          <Upload className="text-muted-foreground" />
          ローカルからアップロード
        </DropdownMenuItem>
        <DropdownMenuItem className="gap-2.5 px-2.5 py-2" onSelect={onOpenDrive}>
          <HardDrive className="text-muted-foreground" />
          ドライブから選択
        </DropdownMenuItem>

        <DropdownMenuSeparator />

        {/* 作成（ノート/ドキュメント/スライド/CSV・#333）。ドライブの「新規作成」と同じ
            作成関数を共用し、作成後は対応エディタへ遷移する。 */}
        <DropdownMenuSub>
          <DropdownMenuSubTrigger
            className="gap-2.5 px-2.5 py-2"
            data-testid="composer-create-menu"
          >
            <FilePlus2 className="text-muted-foreground" />
            作成
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent className="w-56 p-1.5">
            <DropdownMenuItem
              className="gap-2.5 px-2.5 py-2"
              onSelect={() => void createNoteAndOpen()}
              data-testid="composer-create-note"
            >
              <NotebookPen className="text-primary" aria-hidden />
              ノート
            </DropdownMenuItem>
            <DropdownMenuItem
              className="gap-2.5 px-2.5 py-2"
              onSelect={() => void createDocumentAndOpen()}
              data-testid="composer-create-document"
            >
              <FileText className="text-blue-600" aria-hidden />
              ドキュメント（Word）
            </DropdownMenuItem>
            <DropdownMenuItem
              className="gap-2.5 px-2.5 py-2"
              onSelect={() => void createSheetAndOpen()}
              data-testid="composer-create-sheet"
            >
              <FileSpreadsheet className="text-emerald-600" aria-hidden />
              スプレッドシート（Excel）
            </DropdownMenuItem>
            <DropdownMenuItem
              className="gap-2.5 px-2.5 py-2"
              onSelect={() => void createSlideAndOpen()}
              data-testid="composer-create-slide"
            >
              <Presentation className="text-orange-500" aria-hidden />
              スライド
            </DropdownMenuItem>
            <DropdownMenuItem
              className="gap-2.5 px-2.5 py-2"
              onSelect={() => void createCsvAndOpen()}
              data-testid="composer-create-csv"
            >
              <FileSpreadsheet className="text-green-600" aria-hidden />
              CSV（表・SQL 分析）
            </DropdownMenuItem>
          </DropdownMenuSubContent>
        </DropdownMenuSub>

        {/* スキル（#387）。選ぶと入力欄へスラッシュコマンドが打ち込まれる
            （打鍵で呼べることを学べるよう、実行ではなくコマンド確定にする）。

            ⚠️ 応急処置: スキルは多数ある前提の基盤なので、全件フラット列挙は破綻する
            （「添付」「作成」という本来の主目的が押し出される）。出し方の再設計は #390。
            ここでは先頭 N 件に絞り、溢れは件数を明記する（silent truncation にしない）。 */}
        {skills.length > 0 && onPickSkill ? (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuLabel className="uppercase tracking-wide">スキル</DropdownMenuLabel>
            {skills.slice(0, MAX_MENU_SKILLS).map((s) => (
              <DropdownMenuItem
                key={s.key}
                className="gap-2.5 px-2.5 py-2"
                data-testid="composer-skill-command"
                onSelect={() => onPickSkill(s)}
              >
                <Sparkles className="text-primary" aria-hidden />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm">/{s.token}</span>
                  <span className="mt-0.5 block truncate text-[12px] text-muted-foreground">
                    {s.summary}
                  </span>
                </span>
              </DropdownMenuItem>
            ))}
            {skills.length > MAX_MENU_SKILLS ? (
              <div className="px-2.5 py-1.5 text-[12px] text-muted-foreground">
                ほか {skills.length - MAX_MENU_SKILLS} 件（入力欄で「/」を打つと絞り込めます）
              </div>
            ) : null}
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
