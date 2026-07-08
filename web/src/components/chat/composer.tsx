"use client";

import * as React from "react";
import {
  ArrowUp,
  Globe,
  Bot,
  Loader2,
  HardDrive,
  Paperclip,
  Plus,
  Sparkles,
  Square,
  Upload,
  X,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { uploadFile, type NodeResponse } from "@/lib/storage";
import type { Attachment } from "@/lib/chat-api";
import {
  PromptInput,
  PromptInputActions,
  PromptInputTextarea,
} from "@/components/prompt-kit/prompt-input";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { toast } from "@/components/ui/use-toast";
import { DrivePicker } from "./drive-picker";

type Uploading = { name: string; fraction: number };

/// チャット入力。ローカル/ドライブからの添付、送信を担う。
/// 添付ファイルはアップロード後に自動でベクトル化され、doc_search の対象になる。
export function Composer({
  onSubmit,
  onStop,
  placeholder = "何でも尋ねて、社内文書も検索",
  autoFocus = false,
  disabled = false,
  streaming = false,
  agentMode = false,
  onAgentModeChange,
  autonomous = false,
  onAutonomousChange,
  className,
}: {
  onSubmit: (text: string, attachments: Attachment[]) => void;
  /// 生成中に停止する（指定時は送信ボタンが停止ボタンに変わる）。
  onStop?: () => void;
  placeholder?: string;
  autoFocus?: boolean;
  /// 入力自体を不可にするハード無効化（生成中とは別。生成中も入力にフォーカス・タイプできる）。
  disabled?: boolean;
  /// 生成中フラグ。入力は可能なまま、送信はできず停止ボタンを出す。
  streaming?: boolean;
  /// エージェントモード（既定 OFF＝通常チャット。ON＝ツールを自律実行）。
  agentMode?: boolean;
  /// エージェントモードのトグル（未指定ならトグル UI を出さない）。
  onAgentModeChange?: (v: boolean) => void;
  /// 自律モード（既定 OFF。ON＝長ホライズン・フルツール・計画・承認・Task 5.1）。
  autonomous?: boolean;
  /// 自律モードのトグル（未指定ならトグル UI を出さない）。
  onAutonomousChange?: (v: boolean) => void;
  className?: string;
}) {
  const [value, setValue] = React.useState("");
  const [attachments, setAttachments] = React.useState<Attachment[]>([]);
  const [uploading, setUploading] = React.useState<Uploading | null>(null);
  const [menuOpen, setMenuOpen] = React.useState(false);
  const [pickerOpen, setPickerOpen] = React.useState(false);
  const fileInputRef = React.useRef<HTMLInputElement | null>(null);

  const canSend = value.trim().length > 0 && !disabled && !uploading && !streaming;

  const submit = () => {
    const text = value.trim();
    if (!text || disabled || uploading || streaming) return;
    onSubmit(text, attachments);
    setValue("");
    setAttachments([]);
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
          "rounded-[26px] border-border bg-card shadow-sm transition-shadow",
          "focus-within:border-ring/25 focus-within:shadow-md focus-within:ring-4 focus-within:ring-ring/10",
        )}
      >
        <PromptInputTextarea
          placeholder={placeholder}
          autoFocus={autoFocus}
          aria-label="メッセージを入力"
          className="px-3 pt-2 pb-1 text-[15px] leading-relaxed placeholder:text-muted-foreground/70"
        />

        <PromptInputActions className="justify-between px-1 pb-1">
          {/* 左下: 「+」一つに集約（添付）。狭い列でも潰れない。 */}
          <div className="flex min-w-0 items-center gap-1.5">
            <PlusMenu
              open={menuOpen}
              onOpenChange={setMenuOpen}
              onUploadLocal={() => fileInputRef.current?.click()}
              onOpenDrive={() => setPickerOpen(true)}
            />
            {onAgentModeChange ? (
              <button
                type="button"
                role="switch"
                aria-checked={agentMode}
                aria-label="エージェントモード"
                title={
                  agentMode
                    ? "エージェントモード: ON（ツールを自律実行）"
                    : "エージェントモード: OFF（通常チャット）"
                }
                onClick={() => onAgentModeChange(!agentMode)}
                className={cn(
                  "inline-flex h-9 items-center gap-1.5 rounded-full border px-3 text-[13px] font-medium transition-colors",
                  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
                  agentMode
                    ? "border-primary/40 bg-primary/10 text-primary"
                    : "border-border text-foreground/70 hover:bg-secondary hover:text-foreground",
                )}
              >
                <Sparkles className="size-[15px]" aria-hidden />
                エージェント
              </button>
            ) : null}
            {onAutonomousChange ? (
              <button
                type="button"
                role="switch"
                aria-checked={autonomous}
                aria-label="自律モード"
                title={
                  autonomous
                    ? "自律モード: ON（計画・フルツール・承認つきで目標を達成）"
                    : "自律モード: OFF"
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
                自律
              </button>
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
    </div>
  );
}

/// 左下「+」の統合メニュー。添付（ローカル/ドライブ）を 1 つに集約する。
/// Radix DropdownMenu でポータル描画＋衝突回避するため、画面中央のコンポーザ（ホーム）でも
/// メニューがビューポート上端に食い込まず、収まらない高さは内部スクロールに落ちる。
function PlusMenu({
  open,
  onOpenChange,
  onUploadLocal,
  onOpenDrive,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  onUploadLocal: () => void;
  onOpenDrive: () => void;
}) {
  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label="追加メニューを開く"
          title="追加（添付）"
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

        {/* Web 検索（近日対応） */}
        <DropdownMenuItem disabled className="gap-2.5 px-2.5 py-2">
          <Globe className="text-muted-foreground" />
          Web 検索（近日対応）
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
