"use client";

/// 監査ログ › メッセージ（監査担当者・`organization#audit_viewer`・Task 14.15・design §4.14 eDiscovery）。
///
/// 通常の読取経路とは別の `/org-audit/messaging/*` を使い、閲覧・エクスポートはすべて監査ログに残る。
/// 公開チャンネルは承認なしで読め、DM と非公開チャンネルは承認レコードの期間内だけ読める（それ以外は 403）。
/// 読まれた本人には通知されない。エクスポートは期限付きのダウンロードで、ドライブには保存しない。

import * as React from "react";
import { Download, FileText, History, Loader2, Lock, Search, ShieldAlert, Trash2 } from "lucide-react";

import { cn } from "@/lib/utils";
import { formatDateTime } from "@/lib/format";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { toast } from "@/components/ui/use-toast";
import { useMe } from "@/hooks/use-me";
import {
  auditChannels,
  auditExportUrl,
  auditMessages,
  MessagingError,
  requestAccess,
  type AdminChannel,
  type AdminMessage,
  type ContentBlock,
} from "@/lib/messaging-api";
import { cachedUserName, resolveUserNames } from "@/lib/user-names";
import { cachedRoleName, resolveRoleNames } from "@/lib/role-names";
import { ChannelIcon } from "@/components/messages/primitives";
import { AccessRequestList } from "@/components/messages/access-requests";
import { AdminPage, Panel } from "./admin-page";

const KIND_LABEL: Record<AdminChannel["kind"], string> = { public: "公開", private: "非公開", dm: "DM" };

/// `YYYY-MM-DD`（端末の日付）。
function ymd(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/// 日付の入力を期間の端（開始はその日の 0 時・終了は翌日の 0 時）の ISO 文字列へ。
function rangeOf(from: string, until: string): { from: string; until: string } | null {
  if (!from || !until) return null;
  const a = new Date(`${from}T00:00:00`);
  const b = new Date(`${until}T00:00:00`);
  if (Number.isNaN(a.getTime()) || Number.isNaN(b.getTime()) || b < a) return null;
  b.setDate(b.getDate() + 1);
  return { from: a.toISOString(), until: b.toISOString() };
}

/// 本文を監査用の平文に（メンションは名前・添付は node id を出す）。
function blocksText(blocks: ContentBlock[]): string {
  return blocks
    .map((b) => {
      if (b.type === "text") return b.text;
      if (b.type === "mention") {
        const m = b.mention;
        if (m.kind === "user") return `@${cachedUserName(m.user_id) ?? m.user_id}`;
        if (m.kind === "role") return `@${cachedRoleName(m.role_id) ?? m.role_id}`;
        if (m.kind === "channel") return "@channel";
        return "@エージェント";
      }
      if (b.type === "file_ref") return `［添付 ${b.node_id}］`;
      return "";
    })
    .join("");
}

function MessageItem({ m }: { m: AdminMessage }) {
  const [open, setOpen] = React.useState(false);
  return (
    <li className="shiki-dash-top px-4 py-3 first:bg-none" data-testid="audit-message">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-[12px]">
        <span className="font-medium text-foreground">
          {m.author_name}
          {m.author_kind === "assistant" ? <span className="ml-1 text-muted-foreground">（エージェント）</span> : null}
        </span>
        <span className="text-muted-foreground">{formatDateTime(m.created_at)}</span>
        {m.parent_id ? <span className="rounded-full bg-muted px-1.5 text-[10.5px] text-muted-foreground">スレッド返信</span> : null}
        {m.edited_at ? (
          <span className="rounded-full bg-muted px-1.5 text-[10.5px] text-muted-foreground">編集 {formatDateTime(m.edited_at)}</span>
        ) : null}
        {m.deleted_at ? (
          <span className="flex items-center gap-0.5 rounded-full bg-[color-mix(in_oklab,var(--destructive)_14%,transparent)] px-1.5 text-[10.5px] text-destructive">
            <Trash2 className="size-2.5" aria-hidden />
            削除 {formatDateTime(m.deleted_at)}
            {m.deleted_by ? `（${cachedUserName(m.deleted_by) ?? m.deleted_by}）` : ""}
          </span>
        ) : null}
        {m.expired ? <span className="rounded-full bg-muted px-1.5 text-[10.5px] text-muted-foreground">保存期間切れ</span> : null}
      </div>
      <p className="mt-1 whitespace-pre-wrap break-words text-[13px] leading-relaxed text-foreground/90">
        {m.blocks.length > 0 ? blocksText(m.blocks) : <span className="italic text-muted-foreground">（本文なし）</span>}
      </p>
      {m.revisions.length > 0 ? (
        <div className="mt-1.5">
          <button
            type="button"
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
            className="flex items-center gap-1 rounded text-[11.5px] font-medium text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
          >
            <History className="size-3.5" aria-hidden />
            編集前・削除前の本文 {m.revisions.length} 件
          </button>
          {open ? (
            <ol className="mt-1.5 flex flex-col gap-1.5 border-l-2 border-border pl-3">
              {m.revisions.map((r) => (
                <li key={r.rev} className="text-[12px]">
                  <span className="text-muted-foreground">
                    版 {r.rev} ・ {r.reason === "delete" ? "削除前" : r.reason === "edit" ? "編集前" : r.reason} ・ {formatDateTime(r.recorded_at)}
                  </span>
                  <p className="whitespace-pre-wrap break-words text-foreground/85">{blocksText(r.blocks)}</p>
                </li>
              ))}
            </ol>
          ) : null}
        </div>
      ) : null}
    </li>
  );
}

function RequestForm({
  channel,
  from,
  until,
  onRequested,
}: {
  channel: AdminChannel;
  from: string;
  until: string;
  onRequested: () => void;
}) {
  const [reason, setReason] = React.useState("");
  const [days, setDays] = React.useState("7");
  const [saving, setSaving] = React.useState(false);
  const range = rangeOf(from, until);
  const n = Number(days);
  const daysOk = /^\d+$/.test(days) && n >= 1 && n <= 30;
  const ok = reason.trim().length > 0 && range && daysOk;

  const submit = async () => {
    if (!ok || !range) return;
    setSaving(true);
    try {
      await requestAccess({
        channel_id: channel.id,
        reason: reason.trim(),
        messages_from: range.from,
        messages_until: range.until,
        valid_days: n,
      });
      toast({ title: "閲覧を申請しました", description: "承認者が承認すると、期間内だけ読めます。" });
      setReason("");
      onRequested();
    } catch (e) {
      toast({ title: "申請できませんでした", description: e instanceof Error ? e.message : String(e), variant: "destructive" });
    } finally {
      setSaving(false);
    }
  };

  return (
    <form
      className="flex flex-col gap-3 px-4 py-4"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <p className="flex items-start gap-2 text-[12.5px] leading-relaxed text-muted-foreground">
        <Lock className="mt-0.5 size-3.5 shrink-0" aria-hidden />
        {KIND_LABEL[channel.kind]}は、指名された承認者の承認がある期間だけ読めます。対象は上で選んだ発言の期間です。
      </p>
      <div className="flex flex-col gap-1.5">
        <label htmlFor="ar-reason" className="text-[12.5px] font-medium">
          理由
        </label>
        <Textarea
          id="ar-reason"
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          placeholder="例: 2026 年 10 月の情報漏えい調査（案件番号 …）"
          rows={3}
        />
      </div>
      <div className="flex flex-wrap items-end gap-3">
        <div className="flex flex-col gap-1.5">
          <label htmlFor="ar-days" className="text-[12.5px] font-medium">
            閲覧できる日数
          </label>
          <div className="flex items-center gap-2">
            <Input
              id="ar-days"
              inputMode="numeric"
              value={days}
              onChange={(e) => setDays(e.target.value)}
              aria-invalid={!daysOk ? true : undefined}
              className="h-9 w-20 text-right tabular-nums"
            />
            <span className="text-xs text-muted-foreground">日（既定 7・最長 30）</span>
          </div>
        </div>
        <span className="flex-1" />
        <Button type="submit" size="sm" disabled={!ok} loading={saving}>
          閲覧を申請
        </Button>
      </div>
      {!range ? <p className="text-[12px] text-destructive">発言の期間を正しく選んでください。</p> : null}
    </form>
  );
}

export function MessagingAuditView() {
  const { data: me } = useMe();
  const [tab, setTab] = React.useState("channels");
  const [q, setQ] = React.useState("");
  const [channels, setChannels] = React.useState<AdminChannel[] | null>(null);
  const [listError, setListError] = React.useState<string | null>(null);
  const [selected, setSelected] = React.useState<AdminChannel | null>(null);
  const [from, setFrom] = React.useState(() => {
    const d = new Date();
    d.setDate(d.getDate() - 30);
    return ymd(d);
  });
  const [until, setUntil] = React.useState(() => ymd(new Date()));
  const [messages, setMessages] = React.useState<AdminMessage[] | null>(null);
  const [readState, setReadState] = React.useState<"idle" | "loading" | "forbidden" | "error">("idle");
  const [readError, setReadError] = React.useState<string | null>(null);
  const [requestsKey, setRequestsKey] = React.useState(0);
  const [, setNamesTick] = React.useState(0);
  const allNames = React.useRef(new Map<string, string>());

  React.useEffect(() => {
    let active = true;
    const t = setTimeout(() => {
      auditChannels(q.trim())
        .then((r) => {
          if (!active) return;
          setChannels(r);
          setListError(null);
          for (const c of r) allNames.current.set(c.id, c.name);
        })
        .catch((e: unknown) => active && setListError(e instanceof Error ? e.message : String(e)));
    }, 200);
    return () => {
      active = false;
      clearTimeout(t);
    };
  }, [q]);

  const range = rangeOf(from, until);

  const read = React.useCallback(
    async (c: AdminChannel) => {
      const r = rangeOf(from, until);
      if (!r) return;
      setReadState("loading");
      setReadError(null);
      setMessages(null);
      try {
        const list = await auditMessages(c.id, r.from, r.until);
        setMessages(list);
        setReadState("idle");
        const users = new Set<string>();
        const roles = new Set<string>();
        for (const m of list) {
          if (m.deleted_by) users.add(m.deleted_by);
          for (const b of [...m.blocks, ...m.revisions.flatMap((x) => x.blocks)]) {
            if (b.type !== "mention") continue;
            if (b.mention.kind === "user") users.add(b.mention.user_id);
            if (b.mention.kind === "role") roles.add(b.mention.role_id);
          }
        }
        void Promise.all([resolveUserNames(users), resolveRoleNames(roles)]).then(([a, b]) => (a || b) && setNamesTick((n) => n + 1));
      } catch (e) {
        if (e instanceof MessagingError && e.status === 403) setReadState("forbidden");
        else {
          setReadState("error");
          setReadError(e instanceof Error ? e.message : String(e));
        }
      }
    },
    [from, until],
  );

  const select = (c: AdminChannel) => {
    setSelected(c);
    void read(c);
  };

  return (
    <AdminPage
      wide
      title="メッセージの閲覧"
      description="職員間メッセージの監査用の閲覧とエクスポートです。閲覧・エクスポートはすべて監査ログに残り、読まれた本人には通知されません。"
    >
      <Tabs value={tab} onValueChange={setTab}>
        <TabsList>
          <TabsTrigger value="channels">チャンネル</TabsTrigger>
          <TabsTrigger value="requests">閲覧の申請</TabsTrigger>
        </TabsList>

        <TabsContent value="channels" className="pt-3">
          <div className="grid gap-4 lg:grid-cols-[300px_minmax(0,1fr)]">
            <Panel title="チャンネル" className="min-w-0 self-start">
              <div className="border-b px-3 py-2.5">
                <div className="flex h-9 items-center gap-2 rounded-lg border border-input px-2.5 focus-within:border-ring/60">
                  <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden />
                  <input
                    value={q}
                    onChange={(e) => setQ(e.target.value)}
                    placeholder="名前で探す"
                    aria-label="チャンネルを探す"
                    className="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-muted-foreground/70"
                  />
                </div>
              </div>
              <ul className="max-h-[60vh] overflow-y-auto" aria-label="チャンネル">
                {listError ? (
                  <li className="px-4 py-6 text-center text-[12.5px] text-destructive" role="alert">
                    {listError}
                  </li>
                ) : channels === null ? (
                  <li className="flex items-center justify-center gap-2 py-8 text-[12.5px] text-muted-foreground">
                    <Loader2 className="size-4 animate-spin" aria-hidden />
                    読み込んでいます
                  </li>
                ) : channels.length === 0 ? (
                  <li className="px-4 py-8 text-center text-[12.5px] text-muted-foreground">該当するチャンネルはありません</li>
                ) : (
                  channels.map((c) => (
                    <li key={c.id}>
                      <button
                        type="button"
                        onClick={() => select(c)}
                        aria-current={selected?.id === c.id ? "true" : undefined}
                        className={cn(
                          "flex w-full items-center gap-2 px-3 py-2 text-left outline-none transition-colors hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
                          selected?.id === c.id && "bg-accent",
                        )}
                      >
                        <ChannelIcon kind={c.kind} className="shrink-0 text-muted-foreground" />
                        <span className="min-w-0 flex-1 truncate text-[13px]">{c.name}</span>
                        <span className="shrink-0 text-[11px] text-muted-foreground">
                          {KIND_LABEL[c.kind]}
                          {c.archived ? "・アーカイブ" : ""}
                        </span>
                      </button>
                    </li>
                  ))
                )}
              </ul>
            </Panel>

            <div className="flex min-w-0 flex-col gap-4">
              {!selected ? (
                <Panel>
                  <EmptyState icon={FileText} title="チャンネルを選んでください" description="公開チャンネルはそのまま読めます。DM と非公開チャンネルは承認が要ります。" />
                </Panel>
              ) : (
                <Panel
                  title={
                    <span className="flex items-center gap-1.5">
                      <ChannelIcon kind={selected.kind} className="size-3.5 text-muted-foreground" />
                      {selected.name}
                      <span className="text-[11.5px] font-normal text-muted-foreground">（{KIND_LABEL[selected.kind]}）</span>
                    </span>
                  }
                  description="発言の期間を選んで表示します（端末の日付・終了日を含む）。"
                  actions={
                    range && readState === "idle" && messages ? (
                      <Button asChild size="sm" variant="outline">
                        <a href={auditExportUrl(selected.id, range.from, range.until)} download>
                          <Download className="size-3.5" aria-hidden />
                          エクスポート（JSON）
                        </a>
                      </Button>
                    ) : null
                  }
                >
                  <form
                    className="flex flex-wrap items-end gap-3 border-b px-4 py-3"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void read(selected);
                    }}
                  >
                    <label className="flex flex-col gap-1 text-[12px] font-medium">
                      開始
                      <Input type="date" value={from} onChange={(e) => setFrom(e.target.value)} className="h-9 w-40" />
                    </label>
                    <label className="flex flex-col gap-1 text-[12px] font-medium">
                      終了
                      <Input type="date" value={until} onChange={(e) => setUntil(e.target.value)} className="h-9 w-40" />
                    </label>
                    <Button type="submit" size="sm" disabled={!range} loading={readState === "loading"}>
                      表示
                    </Button>
                  </form>

                  {readState === "forbidden" ? (
                    <div>
                      <div className="flex items-start gap-2 px-4 pt-4 text-[13px]">
                        <ShieldAlert className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
                        <p>
                          この期間を読む承認がありません。
                          {selected.kind === "public" ? "" : "承認者に閲覧を申請してください。"}
                        </p>
                      </div>
                      {selected.kind !== "public" ? (
                        <RequestForm
                          channel={selected}
                          from={from}
                          until={until}
                          onRequested={() => {
                            setRequestsKey((k) => k + 1);
                            setTab("requests");
                          }}
                        />
                      ) : null}
                    </div>
                  ) : readState === "error" ? (
                    <p className="px-4 py-8 text-center text-[12.5px] text-destructive" role="alert">
                      {readError}
                    </p>
                  ) : readState === "loading" || !messages ? (
                    <div className="flex items-center justify-center gap-2 py-10 text-[12.5px] text-muted-foreground">
                      <Loader2 className="size-4 animate-spin" aria-hidden />
                      読み込んでいます
                    </div>
                  ) : messages.length === 0 ? (
                    <p className="px-4 py-10 text-center text-[12.5px] text-muted-foreground">この期間の発言はありません</p>
                  ) : (
                    <ol aria-label="発言">
                      {messages.map((m) => (
                        <MessageItem key={m.id} m={m} />
                      ))}
                    </ol>
                  )}
                </Panel>
              )}
            </div>
          </div>
        </TabsContent>

        <TabsContent value="requests" className="pt-3">
          <p className="mb-3 text-[12.5px] leading-relaxed text-muted-foreground">
            あなたの申請の一覧です（承認者に指名されている場合は、組織の申請がすべて並びます）。承認された申請は、期間内なら取り消せます。
          </p>
          <AccessRequestList me={me?.id ?? ""} channelNames={allNames.current} refreshKey={requestsKey} />
        </TabsContent>
      </Tabs>
    </AdminPage>
  );
}
