"use client";

/// 会話の分岐の部品（#1166）。
///
/// - 発話の下: コピー・編集・枝の切り替え（「< 2/3 >」）。編集して送り直すと、元の発話と
///   その続きは消えずに別の枝として残り、ここで切り替えられる。
/// - 回答の下の「…」: 新しいチャットに分岐（その回答までを新しいスレッドへ複製する）。

import { Check, ChevronLeft, ChevronRight, Copy, GitBranch, Loader2, MoreHorizontal, Pencil } from "lucide-react";
import * as React from "react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { toast } from "@/components/ui/use-toast";
import { cn } from "@/lib/utils";
import { FooterButton, footerButtonClass } from "./message-footer";

/// 同じ位置の枝の切り替え（「< 2/3 >」）。枝が 1 つなら何も出さない。
export function BranchSwitcher({
  siblingIds,
  currentId,
  disabled = false,
  onSelect,
}: {
  siblingIds: readonly string[];
  currentId: string;
  /// 生成中・切り替え中は押せない（表示中の枝と生成先がずれないように）。
  disabled?: boolean;
  onSelect: (id: string) => void;
}) {
  const index = siblingIds.indexOf(currentId);
  if (siblingIds.length < 2 || index < 0) return null;
  const prev = siblingIds[index - 1];
  const next = siblingIds[index + 1];
  return (
    <div className="flex items-center text-muted-foreground" data-testid="branch-switcher">
      <button
        type="button"
        onClick={() => prev && onSelect(prev)}
        disabled={disabled || !prev}
        aria-label="前の版"
        title="前の版"
        className={footerButtonClass()}
      >
        <ChevronLeft className="size-4" aria-hidden />
      </button>
      <span
        className="min-w-[2.75rem] text-center text-[12px] tabular-nums"
        aria-live="polite"
        data-testid="branch-position"
      >
        {index + 1} / {siblingIds.length}
      </span>
      <button
        type="button"
        onClick={() => next && onSelect(next)}
        disabled={disabled || !next}
        aria-label="次の版"
        title="次の版"
        className={footerButtonClass()}
      >
        <ChevronRight className="size-4" aria-hidden />
      </button>
    </div>
  );
}

/// 発話の下のアクション（コピー・編集・枝の切り替え）。
///
/// コピーと編集はマウスの端末ではホバーで出す（回答のフッターと同じ）。枝の切り替えは
/// 「別の版がある」こと自体が情報なので常に出す。
export function UserMessageActions({
  text,
  onEdit,
  editDisabled = false,
  branch,
}: {
  text: string;
  /// 編集を始める（未指定なら編集ボタンを出さない＝閲覧のみ・未保存の発話）。
  onEdit?: () => void;
  editDisabled?: boolean;
  branch?: React.ReactNode;
}) {
  const [copied, setCopied] = React.useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      toast({ description: "コピーに失敗しました" });
    }
  };
  return (
    <div className="flex items-center justify-end gap-0.5 pointer-coarse:gap-1" data-testid="user-message-actions">
      <div className="flex items-center gap-0.5 transition-opacity pointer-coarse:gap-1 pointer-fine:opacity-0 pointer-fine:focus-within:opacity-100 pointer-fine:group-hover/user:opacity-100">
        {text ? (
          <FooterButton label={copied ? "コピー済み" : "コピー"} onClick={copy}>
            {copied ? <Check className="size-3.5" /> : <Copy className="size-3.5" />}
          </FooterButton>
        ) : null}
        {onEdit ? (
          <button
            type="button"
            onClick={onEdit}
            disabled={editDisabled}
            aria-label="編集"
            title={editDisabled ? "回答の生成中は編集できません" : "編集"}
            className={footerButtonClass()}
            data-testid="edit-message"
          >
            <Pencil className="size-3.5" aria-hidden />
          </button>
        ) : null}
      </div>
      {branch}
    </div>
  );
}

/// 発話の編集欄。送信で新しい枝として送り直す（元の発話は別の版として残る）。
///
/// Enter は改行（長い発話を直すことが多い）。⌘/Ctrl+Enter で送信、Esc で取り消す。
export function UserMessageEditor({
  initialText,
  onCancel,
  onSubmit,
}: {
  initialText: string;
  onCancel: () => void;
  onSubmit: (text: string) => void;
}) {
  const [text, setText] = React.useState(initialText);
  const ref = React.useRef<HTMLTextAreaElement>(null);
  // 開いたら末尾にカーソルを置く（直す箇所は末尾のことが多い）。
  React.useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  }, []);
  // 内容に合わせて高さを伸ばす（上限を超えたらスクロール）。
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 320)}px`;
  }, [text]);
  const trimmed = text.trim();
  const changed = trimmed !== initialText.trim();
  const submit = () => {
    if (trimmed && changed) onSubmit(trimmed);
  };
  return (
    <div
      className="w-full rounded-2xl border border-border bg-secondary/60 p-2 transition-colors focus-within:border-foreground/25"
      data-testid="message-editor"
    >
      <textarea
        ref={ref}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
          } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && !e.nativeEvent.isComposing) {
            e.preventDefault();
            submit();
          }
        }}
        aria-label="発話を編集"
        rows={1}
        className="block w-full resize-none border-0 bg-transparent px-2 py-1.5 text-[15px] leading-relaxed text-foreground shadow-none outline-none ring-0 focus:outline-none focus:ring-0 focus-visible:outline-none focus-visible:ring-0"
      />
      <div className="mt-1 flex items-center justify-end gap-2">
        <span className="mr-auto hidden px-2 text-[11.5px] text-muted-foreground sm:inline">
          元の発話と続きは別の版として残ります
        </span>
        <Button type="button" variant="ghost" size="sm" onClick={onCancel}>
          キャンセル
        </Button>
        <Button type="button" size="sm" onClick={submit} disabled={!trimmed || !changed} data-testid="message-editor-submit">
          送信
        </Button>
      </div>
    </div>
  );
}

/// 回答の下の「…」（新しいチャットに分岐）。
export function MessageMoreMenu({ onFork }: { onFork: () => Promise<void> }) {
  const [busy, setBusy] = React.useState(false);
  const fork = async () => {
    setBusy(true);
    try {
      await onFork();
    } catch (e) {
      toast({ description: e instanceof Error ? e.message : "新しいチャットへの分岐に失敗しました" });
    } finally {
      setBusy(false);
    }
  };
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label="その他の操作"
          title="その他の操作"
          className={cn(footerButtonClass())}
          disabled={busy}
          data-testid="message-more"
        >
          {busy ? <Loader2 className="size-3.5 animate-spin" aria-hidden /> : <MoreHorizontal className="size-3.5" aria-hidden />}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-56 p-1.5">
        <DropdownMenuItem onSelect={() => void fork()} data-testid="fork-thread">
          <GitBranch className="size-4" aria-hidden />
          新しいチャットに分岐
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
