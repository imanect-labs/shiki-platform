"use client";

/// 発言の入力欄。`@` でメンション候補を出し、ドライブの文書を `file_ref` として添付する。
/// 送信時に平文を content blocks（text / mention / file_ref）へ組み替える。

import * as React from "react";
import { Paperclip, SendHorizontal, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import {
  FILES,
  MEMBERS,
  canRead,
  findFile,
  type Member,
  type MessageBlock,
} from "@/lib/messages-mock";
import { FileKindIcon, MemberAvatar } from "./primitives";

/// 平文をブロック列へ。`@氏名` は登録利用者に一致した場合だけ mention ブロックにする。
export function toBlocks(text: string, fileIds: string[]): MessageBlock[] {
  const blocks: MessageBlock[] = [];
  let buf = "";
  let i = 0;
  const names = MEMBERS.map((m) => m.name).sort((a, b) => b.length - a.length);

  while (i < text.length) {
    if (text[i] === "@") {
      const hit = names.find((n) => text.startsWith(n, i + 1));
      if (hit) {
        if (buf) {
          blocks.push({ type: "text", text: buf });
          buf = "";
        }
        const member = MEMBERS.find((m) => m.name === hit);
        if (member) blocks.push({ type: "mention", member_id: member.id });
        i += hit.length + 1;
        continue;
      }
    }
    buf += text[i];
    i += 1;
  }
  if (buf) blocks.push({ type: "text", text: buf });
  for (const id of fileIds) blocks.push({ type: "file_ref", node_id: id });
  return blocks;
}

/// カーソル直前の `@` から始まる未確定の入力を取り出す（メンション補完のトリガ）。
function mentionQuery(text: string, caret: number): { start: number; query: string } | null {
  const head = text.slice(0, caret);
  const at = head.lastIndexOf("@");
  if (at < 0) return null;
  const q = head.slice(at + 1);
  // 空白や改行を跨いだら補完を閉じる（文中の `@` を誤検出しない）。
  if (/[\s\n]/.test(q)) return null;
  if (at > 0 && !/[\s\n(（]/.test(head[at - 1])) return null;
  return { start: at, query: q };
}

export function Composer({
  placeholder,
  channelMemberIds,
  viewerId,
  onSend,
  autoFocus,
  hideHint,
}: {
  placeholder: string;
  channelMemberIds: string[];
  /// 添付候補を閲覧権限で絞るために要る（自分が読めない文書は名前も出さない）。
  viewerId: string;
  onSend: (blocks: MessageBlock[]) => void;
  autoFocus?: boolean;
  /// スレッドなど、同じ画面に既に注記が出ている場所では下の注記を省く。
  hideHint?: boolean;
}) {
  const [text, setText] = React.useState("");
  const [files, setFiles] = React.useState<string[]>([]);
  const [attachOpen, setAttachOpen] = React.useState(false);
  const [mention, setMention] = React.useState<{ start: number; query: string } | null>(null);
  const [highlight, setHighlight] = React.useState(0);
  const ref = React.useRef<HTMLTextAreaElement>(null);

  // 自分が読めない文書は候補に出さない。本文側で「参照できない添付」と伏せているのに、
  // 添付候補から名前と所在が漏れては意味がない。
  const attachable = React.useMemo(() => FILES.filter((f) => canRead(f, viewerId)), [viewerId]);

  const candidates: Member[] = React.useMemo(() => {
    if (!mention) return [];
    const q = mention.query;
    return MEMBERS.filter((m) => m.id !== "shiki")
      .filter((m) => channelMemberIds.includes(m.id))
      .filter((m) => !q || m.name.includes(q) || m.dept.includes(q));
  }, [mention, channelMemberIds]);

  React.useEffect(() => setHighlight(0), [mention?.query]);

  // 入力に追随して高さを伸ばす（最大 8 行相当まで）。
  React.useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 168)}px`;
  }, [text]);

  const sync = (value: string, caret: number) => {
    setText(value);
    setMention(mentionQuery(value, caret));
  };

  const pick = (member: Member) => {
    if (!mention) return;
    const el = ref.current;
    const caret = el?.selectionStart ?? text.length;
    const next = `${text.slice(0, mention.start)}@${member.name} ${text.slice(caret)}`;
    setText(next);
    setMention(null);
    requestAnimationFrame(() => {
      const pos = mention.start + member.name.length + 2;
      el?.focus();
      el?.setSelectionRange(pos, pos);
    });
  };

  const send = () => {
    const blocks = toBlocks(text.trim(), files);
    if (blocks.length === 0) return;
    onSend(blocks);
    setText("");
    setFiles([]);
    setMention(null);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // IME 変換中のキーは IME のもの。奪うと確定文字が二重に入る／変換が壊れる。
    if (e.nativeEvent.isComposing) return;
    if (mention && candidates.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setHighlight((h) => (h + 1) % candidates.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setHighlight((h) => (h - 1 + candidates.length) % candidates.length);
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        pick(candidates[highlight]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMention(null);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  const canSend = text.trim().length > 0 || files.length > 0;

  return (
    <div className="relative px-4 pb-4 pt-1">
      {/* メンション候補（キーボードで選べる・Enter/Tab で確定）。 */}
      {mention && candidates.length > 0 ? (
        <div
          role="listbox"
          aria-label="メンション候補"
          className="absolute bottom-full left-4 z-20 mb-1 w-[264px] overflow-hidden rounded-xl border border-border bg-popover p-1 shadow-md"
        >
          <p className="px-2 py-1 text-[10.5px] font-semibold uppercase tracking-wide text-muted-foreground">
            このチャンネルの参加者
          </p>
          {candidates.map((m, i) => (
            <button
              key={m.id}
              type="button"
              role="option"
              aria-selected={i === highlight}
              onMouseEnter={() => setHighlight(i)}
              onClick={() => pick(m)}
              className={cn(
                "flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left outline-none transition-colors",
                i === highlight ? "bg-accent" : "hover:bg-accent/60",
              )}
            >
              <MemberAvatar memberId={m.id} size="sm" />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13px] font-medium text-foreground">
                  {m.name}
                </span>
                <span className="block truncate text-[11px] text-muted-foreground">{m.dept}</span>
              </span>
            </button>
          ))}
        </div>
      ) : null}

      {/* 添付候補（ドライブの文書を file_ref として貼る）。 */}
      {attachOpen ? (
        <div className="absolute bottom-full left-4 z-20 mb-1 w-[300px] overflow-hidden rounded-xl border border-border bg-popover p-1 shadow-md">
          <p className="px-2 py-1 text-[10.5px] font-semibold uppercase tracking-wide text-muted-foreground">
            ドライブから共有
          </p>
          {attachable.map((f) => (
            <button
              key={f.id}
              type="button"
              onClick={() => {
                setFiles((prev) => (prev.includes(f.id) ? prev : [...prev, f.id]));
                setAttachOpen(false);
                ref.current?.focus();
              }}
              className="flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-left outline-none transition-colors hover:bg-accent"
            >
              <FileKindIcon kind={f.kind} />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13px] font-medium text-foreground">
                  {f.name}
                </span>
                <span className="block truncate text-[11px] text-muted-foreground">
                  {f.location}
                </span>
              </span>
            </button>
          ))}
          {attachable.length === 0 ? (
            <p className="px-2 py-2 text-[12px] text-muted-foreground">
              共有できる文書がありません。
            </p>
          ) : null}
          <p className="shiki-dash-top mt-1 px-2 pb-1 pt-2 text-[11px] leading-snug text-muted-foreground">
            共有しても本文は複製されません。閲覧可否は文書側の権限に従うため、
            相手によっては「参照できない添付」として表示されます。
          </p>
        </div>
      ) : null}

      <div className="rounded-xl border border-border/70 bg-card shadow-xs transition-colors focus-within:border-ring/60 focus-within:shadow-sm">
        {files.length > 0 ? (
          <div className="flex flex-wrap gap-1.5 px-3 pt-2.5">
            {files.map((id) => {
              const f = findFile(id);
              if (!f) return null;
              return (
                <span
                  key={id}
                  className="flex items-center gap-1.5 rounded-full border border-border/60 bg-accent/50 py-1 pl-2.5 pr-1 text-[11.5px] text-foreground"
                >
                  {f.name}
                  <button
                    type="button"
                    onClick={() => setFiles((prev) => prev.filter((x) => x !== id))}
                    aria-label={`${f.name} の添付を外す`}
                    className="flex size-4 items-center justify-center rounded-full text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <X className="size-3" aria-hidden />
                  </button>
                </span>
              );
            })}
          </div>
        ) : null}

        <textarea
          ref={ref}
          rows={1}
          autoFocus={autoFocus}
          value={text}
          placeholder={placeholder}
          onChange={(e) => sync(e.target.value, e.target.selectionStart)}
          onKeyDown={onKeyDown}
          onClick={(e) => setMention(mentionQuery(text, e.currentTarget.selectionStart))}
          onBlur={() => setTimeout(() => setMention(null), 120)}
          className="scrollbar-subtle block max-h-[168px] w-full resize-none bg-transparent px-3 py-2.5 text-[13.5px] leading-[1.6] text-foreground outline-none placeholder:text-muted-foreground/70 focus-visible:ring-0 focus-visible:ring-offset-0"
        />

        <div className="flex items-center gap-1 px-2 pb-2">
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={() => setAttachOpen((v) => !v)}
                aria-label="ドライブの文書を共有"
                aria-expanded={attachOpen}
                className={cn(
                  "flex size-8 items-center justify-center rounded-lg outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                  attachOpen
                    ? "bg-accent text-foreground"
                    : "text-muted-foreground hover:bg-accent hover:text-foreground",
                )}
              >
                <Paperclip className="size-4" aria-hidden />
              </button>
            </TooltipTrigger>
            <TooltipContent side="top">ドライブの文書を共有</TooltipContent>
          </Tooltip>
          <span className="flex-1" />
          <span className="mr-1 hidden text-[10.5px] text-muted-foreground sm:inline">
            Enter で送信 / Shift+Enter で改行
          </span>
          <button
            type="button"
            onClick={send}
            disabled={!canSend}
            aria-label="送信"
            className={cn(
              "flex size-8 items-center justify-center rounded-lg outline-none transition-[background-color,transform] duration-[var(--duration-fast)] ease-[var(--ease-standard)] focus-visible:ring-2 focus-visible:ring-ring active:scale-95",
              canSend
                ? "bg-primary text-primary-foreground hover:bg-primary/90"
                : "cursor-not-allowed bg-muted text-muted-foreground/60",
            )}
          >
            <SendHorizontal className="size-4" aria-hidden />
          </button>
        </div>
      </div>

      {hideHint ? null : (
        <p className="mt-1.5 px-1 text-[10.5px] text-muted-foreground/80">
          <span style={{ color: seasonVar(0) }}>●</span>{" "}
          発言は監査対象です。共有した文書は相手の権限に従って表示されます。
        </p>
      )}
    </div>
  );
}
