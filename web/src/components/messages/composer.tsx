"use client";

/// 発言の入力欄。`@` でメンション候補（参加者・ロール・`@channel`・自分のエージェント）を出し、
/// ドライブの文書を `file_ref` として添付する。送信時に平文を content blocks へ組み替える。
/// 編集（自分の発言）にも同じ部品を使う（`initial` と `submitLabel`）。

import * as React from "react";
import { AtSign, Bot, Paperclip, SendHorizontal, Users, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { seasonVar } from "@/lib/season";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { DrivePicker } from "@/components/chat/drive-picker";
import {
  MAX_ATTACHMENTS,
  MAX_TEXT_CHARS,
  searchRoles,
  type MemberView,
  type Mention,
  type MessageBlock,
} from "@/lib/messaging-api";
import { rememberRoleName } from "@/lib/role-names";
import { AgentAvatar, UserAvatar, useMessagingDisplay } from "./primitives";

/// 入力中に確定したメンション（本文中の `@label` と宛先の対応）。
type Token = { label: string; mention: Mention };
type Attachment = { node_id: string; name: string };

/// 下書き（編集の初期値）。
export type Draft = { text: string; tokens: Token[]; files: Attachment[] };

/// 平文をブロック列へ。確定したメンション（`@label`）だけを mention ブロックにする。
export function toBlocks(text: string, tokens: Token[], files: Attachment[]): MessageBlock[] {
  const blocks: MessageBlock[] = [];
  const sorted = [...tokens].sort((a, b) => b.label.length - a.label.length);
  let buf = "";
  let i = 0;
  while (i < text.length) {
    if (text[i] === "@") {
      const hit = sorted.find((t) => text.startsWith(t.label, i + 1));
      if (hit) {
        if (buf) blocks.push({ type: "text", text: buf });
        buf = "";
        blocks.push({ type: "mention", mention: hit.mention });
        i += hit.label.length + 1;
        continue;
      }
    }
    buf += text[i];
    i += 1;
  }
  if (buf) blocks.push({ type: "text", text: buf });
  for (const f of files) blocks.push({ type: "file_ref", node_id: f.node_id, name: "" });
  return blocks;
}

/// 既存の発言を編集用の下書きに戻す（メンションは表記と宛先の対応として持ち直す）。
export function draftOf(
  blocks: readonly MessageBlock[] | readonly { type: string }[],
  files: readonly { block_index: number; name?: string | null; available: boolean }[],
  label: (m: Mention) => string,
): Draft {
  const list = blocks as MessageBlock[];
  const attachments: Attachment[] = [];
  list.forEach((b, i) => {
    if (b.type === "file_ref") {
      const f = files.find((x) => x.block_index === i);
      attachments.push({ node_id: b.node_id, name: f?.available && f.name ? f.name : "参照できない添付" });
    }
  });
  // 保存し直したときに同じ本文・同じ宛先へ戻ることを確かめ、戻らなければラベルの番号をずらして組み直す
  // （同じ表示名の別の宛先や、メンションの直後の文字列・本文中の `@名前` と表記が重なる場合）。
  const expected = JSON.stringify(withoutFiles(list));
  let draft = draftText(list, label, 0);
  for (let shift = 1; shift <= 50 && JSON.stringify(toBlocks(draft.text, draft.tokens, [])) !== expected; shift += 1) {
    draft = draftText(list, label, shift);
  }
  return { ...draft, files: attachments };
}

/// 添付を除き、隣り合う本文をまとめたブロック列（`toBlocks` の出力と比べる形）。
function withoutFiles(blocks: readonly MessageBlock[]): MessageBlock[] {
  const out: MessageBlock[] = [];
  for (const b of blocks) {
    if (b.type === "text") {
      if (!b.text) continue;
      const last = out[out.length - 1];
      if (last?.type === "text") out[out.length - 1] = { type: "text", text: last.text + b.text };
      else out.push({ type: "text", text: b.text });
    } else if (b.type === "mention") out.push({ type: "mention", mention: b.mention });
  }
  return out;
}

/// 本文とメンションの表記を組む。同じ表示名でも宛先が違えば別のラベルにする。
/// `shift` が 0 なら最初の宛先は番号なし、それ以外は全員に `shift` だけずらした番号を付ける。
function draftText(
  blocks: readonly MessageBlock[],
  label: (m: Mention) => string,
  shift: number,
): { text: string; tokens: Token[] } {
  let text = "";
  const tokens: Token[] = [];
  const seen = new Map<string, string[]>();
  for (const b of blocks) {
    if (b.type === "text") text += b.text;
    else if (b.type === "mention") {
      const base = label(b.mention).replace(/^@/, "");
      const key = JSON.stringify(b.mention);
      const keys = seen.get(base) ?? [];
      let j = keys.indexOf(key);
      if (j < 0) {
        j = keys.length;
        keys.push(key);
        seen.set(base, keys);
      }
      const n = j + 1 + shift;
      const l = n === 1 ? base : `${base}(${n})`;
      text += `@${l}`;
      if (!tokens.some((t) => t.label === l)) tokens.push({ label: l, mention: b.mention });
    }
  }
  return { text, tokens };
}

/// カーソル直前の `@` から始まる未確定の入力を取り出す（メンション補完のトリガ）。
function mentionQuery(text: string, caret: number): { start: number; query: string } | null {
  const head = text.slice(0, caret);
  const at = head.lastIndexOf("@");
  if (at < 0) return null;
  const q = head.slice(at + 1);
  // 空白や改行を跨いだら補完を閉じる（文中の `@` を誤検出しない）。
  if (/\s/.test(q)) return null;
  if (at > 0 && !/[\s(（]/.test(head[at - 1])) return null;
  return { start: at, query: q };
}

type Candidate = {
  key: string;
  label: string;
  sub: string;
  mention: Mention;
  icon: React.ReactNode;
};

export function Composer({
  placeholder,
  members,
  allowChannelMention,
  onSubmit,
  initial,
  submitLabel = "送信",
  onCancel,
  autoFocus,
  hint,
  disabledReason,
}: {
  placeholder: string;
  /// メンションの候補（そのチャンネルの参加者だけ）。
  members: MemberView[];
  /// `@channel` を候補に出すか（DM では出さない）。
  allowChannelMention: boolean;
  /// 送る。成功したら true（失敗したら下書きを残す）。
  onSubmit: (blocks: MessageBlock[], opts: { agentPrivate: boolean }) => Promise<boolean>;
  initial?: Draft;
  submitLabel?: string;
  /// 編集の取りやめ（Escape でも呼ぶ）。
  onCancel?: () => void;
  autoFocus?: boolean;
  /// 欄の下の注記（省略で出さない）。
  hint?: React.ReactNode;
  /// 投稿できない理由（アーカイブ済み・アナウンス専用など）。あれば入力欄の代わりに出す。
  disabledReason?: string | null;
}) {
  const { me, agentName, agentHasIcon } = useMessagingDisplay();
  const [text, setText] = React.useState(initial?.text ?? "");
  const [tokens, setTokens] = React.useState<Token[]>(initial?.tokens ?? []);
  const [files, setFiles] = React.useState<Attachment[]>(initial?.files ?? []);
  const [agentPrivate, setAgentPrivate] = React.useState(false);
  const [pickerOpen, setPickerOpen] = React.useState(false);
  const [mention, setMention] = React.useState<{ start: number; query: string } | null>(null);
  const [roles, setRoles] = React.useState<Candidate[]>([]);
  const [highlight, setHighlight] = React.useState(0);
  const [sending, setSending] = React.useState(false);
  const ref = React.useRef<HTMLTextAreaElement>(null);
  const listboxId = React.useId();

  // ロールは候補を打ち始めてから引く（部署は多くない・入力のたびに投げすぎないよう少し待つ）。
  React.useEffect(() => {
    if (!mention || mention.query.length === 0) {
      setRoles([]);
      return;
    }
    let active = true;
    const t = setTimeout(() => {
      searchRoles(mention.query, 5)
        .then((rs) => {
          if (!active) return;
          setRoles(
            rs.map((r) => {
              rememberRoleName(r.id, r.display_name);
              return {
                key: `role:${r.id}`,
                label: r.display_name,
                sub: "ロール（このチャンネルの参加者だけに通知）",
                mention: { kind: "role", role_id: r.id },
                icon: (
                  <span className="flex size-7 items-center justify-center rounded-[8px] bg-muted text-muted-foreground">
                    <Users className="size-4" aria-hidden />
                  </span>
                ),
              };
            }),
          );
        })
        .catch(() => active && setRoles([]));
    }, 180);
    return () => {
      active = false;
      clearTimeout(t);
    };
  }, [mention]);

  const candidates: Candidate[] = React.useMemo(() => {
    if (!mention) return [];
    const q = mention.query.toLowerCase();
    const hit = (s: string) => !q || s.toLowerCase().includes(q);
    const out: Candidate[] = [];
    if (hit(agentName) || hit("agent") || hit("エージェント")) {
      out.push({
        key: "agent",
        label: agentName,
        sub: "自分のエージェント（自分の権限で答えます）",
        mention: { kind: "agent" },
        icon: <AgentAvatar ownerId={me} hasIcon={agentHasIcon} size="sm" />,
      });
    }
    for (const m of members) {
      if (!m.active || m.user_id === me) continue;
      if (!hit(m.display_name)) continue;
      out.push({
        key: `user:${m.user_id}`,
        label: m.display_name,
        sub: m.is_admin ? "チャンネルの管理者" : "参加者",
        mention: { kind: "user", user_id: m.user_id },
        icon: <UserAvatar userId={m.user_id} name={m.display_name} size="sm" />,
      });
      if (out.length >= 8) break;
    }
    if (allowChannelMention && hit("channel")) {
      out.push({
        key: "channel",
        label: "channel",
        sub: "このチャンネルの全員に通知",
        mention: { kind: "channel" },
        icon: (
          <span className="flex size-7 items-center justify-center rounded-[8px] bg-muted text-muted-foreground">
            <AtSign className="size-4" aria-hidden />
          </span>
        ),
      });
    }
    return [...out, ...roles.filter((r) => !out.some((o) => o.key === r.key))];
  }, [mention, members, allowChannelMention, agentName, agentHasIcon, me, roles]);

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

  const pick = (c: Candidate) => {
    if (!mention) return;
    const el = ref.current;
    const caret = el?.selectionStart ?? text.length;
    // 同じ表示名でも宛先が違えば別のラベルにする（同姓同名・利用者名とロール名の衝突で宛先を取り違えない）。
    const same = (t: Token) => JSON.stringify(t.mention) === JSON.stringify(c.mention);
    let label = c.label;
    for (let n = 2; tokens.some((t) => t.label === label && !same(t)); n += 1) label = `${c.label}(${n})`;
    const next = `${text.slice(0, mention.start)}@${label} ${text.slice(caret)}`;
    setText(next);
    setTokens((prev) => (prev.some((t) => t.label === label) ? prev : [...prev, { label, mention: c.mention }]));
    setMention(null);
    requestAnimationFrame(() => {
      const pos = mention.start + label.length + 2;
      el?.focus();
      el?.setSelectionRange(pos, pos);
    });
  };

  // 本文に残っている確定メンションだけを使う（消したメンションを送らない）。
  const liveTokens = tokens.filter((t) => text.includes(`@${t.label}`));
  const callsAgent = liveTokens.some((t) => t.mention.kind === "agent");
  const length = Array.from(text).length;
  const tooLong = length > MAX_TEXT_CHARS;
  // 送信の応答を待つ間に書き足されたかを、応答の後に確かめるための最新の本文。
  const textRef = React.useRef(text);
  textRef.current = text;
  const filesRef = React.useRef(files);
  filesRef.current = files;

  const canSend = !sending && !tooLong && (text.trim().length > 0 || files.length > 0);

  const submit = async () => {
    if (!canSend) return;
    const blocks = toBlocks(text.trim(), liveTokens, files);
    if (blocks.length === 0) return;
    setSending(true);
    const sentText = text;
    const sentFiles = files;
    const ok = await onSubmit(blocks, { agentPrivate: callsAgent && agentPrivate });
    setSending(false);
    if (ok && !initial) {
      // 送信を待つ間に下書き（本文か添付のどちらか）を変えたなら、下書き全体を残す。
      // 本文と添付を別々に比べると、本文だけ書き足したときに添付だけが消えて下書きが崩れる。
      if (textRef.current === sentText && filesRef.current === sentFiles) {
        setText("");
        setFiles([]);
        setTokens([]);
        setMention(null);
      }
      requestAnimationFrame(() => ref.current?.focus());
    }
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // IME 変換中のキーは IME のもの。奪うと確定文字が二重に入る／変換が壊れる。
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
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
        pick(candidates[Math.min(highlight, candidates.length - 1)]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMention(null);
        return;
      }
    }
    if (e.key === "Escape" && onCancel) {
      e.preventDefault();
      onCancel();
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void submit();
    }
  };

  if (disabledReason) {
    return (
      <div className="px-4 pb-4 pt-1">
        <p className="rounded-xl border border-dashed border-border bg-muted/30 px-4 py-3 text-center text-[12.5px] text-muted-foreground">
          {disabledReason}
        </p>
      </div>
    );
  }

  const showList = mention !== null && candidates.length > 0;

  return (
    <div className={cn("relative", initial ? "py-1" : "px-4 pb-4 pt-1")}>
      {/* メンション候補（キーボードで選べる・Enter/Tab で確定）。 */}
      {showList ? (
        <div
          id={listboxId}
          role="listbox"
          aria-label="メンション候補"
          className="absolute bottom-full left-4 z-20 mb-1 max-h-[300px] w-[300px] max-w-[calc(100%-2rem)] overflow-y-auto rounded-xl border border-border bg-popover p-1 shadow-md"
        >
          {candidates.map((c, i) => (
            <button
              key={c.key}
              id={`${listboxId}-${i}`}
              type="button"
              role="option"
              aria-selected={i === highlight}
              tabIndex={-1}
              onMouseEnter={() => setHighlight(i)}
              // フォーカスを入力欄から奪わない（blur で候補が閉じる前に確定させる）。
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => pick(c)}
              className={cn(
                "flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left outline-none transition-colors",
                i === highlight ? "bg-accent" : "hover:bg-accent/60",
              )}
            >
              {c.icon}
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[13px] font-medium text-foreground">@{c.label}</span>
                <span className="block truncate text-[11px] text-muted-foreground">{c.sub}</span>
              </span>
            </button>
          ))}
        </div>
      ) : null}

      <div className="rounded-xl border border-border/70 bg-card shadow-xs transition-colors focus-within:border-ring/60 focus-within:shadow-sm">
        {files.length > 0 ? (
          <div className="flex flex-wrap gap-1.5 px-3 pt-2.5">
            {files.map((f) => (
              <span
                key={f.node_id}
                className="flex max-w-full items-center gap-1.5 rounded-full border border-border/60 bg-accent/50 py-1 pl-2.5 pr-1 text-[11.5px] text-foreground"
              >
                <span className="truncate">{f.name}</span>
                <button
                  type="button"
                  onClick={() => setFiles((prev) => prev.filter((x) => x.node_id !== f.node_id))}
                  aria-label={`${f.name} の添付を外す`}
                  className="flex size-4 shrink-0 items-center justify-center rounded-full text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <X className="size-3" aria-hidden />
                </button>
              </span>
            ))}
          </div>
        ) : null}

        <textarea
          ref={ref}
          rows={1}
          autoFocus={autoFocus}
          value={text}
          placeholder={placeholder}
          aria-label={placeholder}
          role="combobox"
          aria-expanded={showList}
          aria-controls={showList ? listboxId : undefined}
          aria-autocomplete="list"
          aria-activedescendant={showList ? `${listboxId}-${highlight}` : undefined}
          onChange={(e) => sync(e.target.value, e.target.selectionStart)}
          onKeyDown={onKeyDown}
          onClick={(e) => setMention(mentionQuery(text, e.currentTarget.selectionStart))}
          onBlur={() => setMention(null)}
          className="scrollbar-subtle block max-h-[168px] w-full resize-none bg-transparent px-3 py-2.5 text-[13.5px] leading-[1.6] text-foreground outline-none placeholder:text-muted-foreground/70 focus-visible:ring-0 focus-visible:ring-offset-0"
        />

        <div className="flex flex-wrap items-center gap-1 px-2 pb-2">
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                onClick={() => setPickerOpen(true)}
                disabled={files.length >= MAX_ATTACHMENTS}
                aria-label="ドライブの文書を共有"
                className="flex size-8 items-center justify-center rounded-lg text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
              >
                <Paperclip className="size-4" aria-hidden />
              </button>
            </TooltipTrigger>
            <TooltipContent side="top">
              {files.length >= MAX_ATTACHMENTS ? `添付は ${MAX_ATTACHMENTS} 件までです` : "ドライブの文書を共有"}
            </TooltipContent>
          </Tooltip>
          {callsAgent ? (
            <label className="flex min-w-0 items-center gap-1.5 rounded-lg px-1.5 py-1 text-[11.5px] text-muted-foreground">
              <Switch checked={agentPrivate} onCheckedChange={setAgentPrivate} aria-label="自分だけに表示（自分の権限で回答）" />
              <Bot className="size-3.5 shrink-0" aria-hidden />
              <span className="truncate">自分だけに表示（自分の権限で回答）</span>
            </label>
          ) : null}
          <span className="flex-1" />
          {length > MAX_TEXT_CHARS - 500 ? (
            <span
              className={cn("mr-1 text-[10.5px] tabular-nums", tooLong ? "font-semibold text-destructive" : "text-muted-foreground")}
              aria-live="polite"
            >
              {length.toLocaleString("ja-JP")} / {MAX_TEXT_CHARS.toLocaleString("ja-JP")}
            </span>
          ) : (
            <span className="mr-1 hidden text-[10.5px] text-muted-foreground sm:inline">
              Enter で{initial ? "保存" : "送信"} / Shift+Enter で改行
            </span>
          )}
          {onCancel ? (
            <button
              type="button"
              onClick={onCancel}
              className="h-8 rounded-lg px-2.5 text-[12.5px] font-medium text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
            >
              キャンセル
            </button>
          ) : null}
          <button
            type="button"
            onClick={() => void submit()}
            disabled={!canSend}
            aria-label={submitLabel}
            className={cn(
              "flex h-8 min-w-8 items-center justify-center gap-1 rounded-lg px-2 text-[12.5px] font-medium outline-none transition-[background-color,transform] duration-[var(--duration-fast)] ease-[var(--ease-standard)] focus-visible:ring-2 focus-visible:ring-ring active:scale-95",
              canSend
                ? "bg-primary text-primary-foreground hover:bg-primary/90"
                : "cursor-not-allowed bg-muted text-muted-foreground/60",
            )}
          >
            {initial ? submitLabel : <SendHorizontal className="size-4" aria-hidden />}
          </button>
        </div>
      </div>

      {hint ? (
        <p className="mt-1.5 px-1 text-[10.5px] text-muted-foreground/80">
          <span style={{ color: seasonVar(0) }}>●</span> {hint}
        </p>
      ) : null}

      <DrivePicker
        open={pickerOpen}
        onOpenChange={setPickerOpen}
        title="ドライブの文書を共有"
        description="共有しても本文は複製されません。閲覧できるかは文書側の権限に従い、読めない人には「参照できない添付」と表示されます。"
        onSelect={(node) => {
          setFiles((prev) =>
            prev.some((f) => f.node_id === node.id) || prev.length >= MAX_ATTACHMENTS
              ? prev
              : [...prev, { node_id: node.id, name: node.name }],
          );
          setPickerOpen(false);
          requestAnimationFrame(() => ref.current?.focus());
        }}
      />
    </div>
  );
}
