"use client";

/// チャンネルの設定（概要・参加者・通知）。名前・トピック・アーカイブ・参加者の権限は
/// チャンネルの管理者だけが変えられる（サーバが判定し、画面は操作を出し分けるだけ）。

import * as React from "react";
import { Archive, ArchiveRestore, Crown, LogOut, Megaphone, UserMinus, UserPlus } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { toast } from "@/components/ui/use-toast";
import {
  inviteMembers,
  MessagingError,
  removeMember,
  setArchived,
  setMemberRole,
  setNotify,
  updateChannel,
  type ChannelView,
  type MemberView,
  type NotifyLevel,
} from "@/lib/messaging-api";
import { UserAvatar, useMessagingDisplay } from "./primitives";
import { UserMultiPicker, type PickedUser } from "./user-picker";

export type SettingsTab = "about" | "members" | "notify";

const NOTIFY: { value: NotifyLevel; label: string; note: string }[] = [
  { value: "all", label: "すべて", note: "すべての発言を未読として数え、メンションを通知します。" },
  { value: "mentions", label: "メンションだけ", note: "自分あてのメンションだけを通知します。" },
  { value: "none", label: "なし", note: "このチャンネルの通知と未読の件数を出しません。" },
];

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function ChannelSettingsDialog({
  open,
  onOpenChange,
  tab,
  onTabChange,
  channel,
  members,
  onChannelChanged,
  onMembersChanged,
  onLeft,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  tab: SettingsTab;
  onTabChange: (t: SettingsTab) => void;
  channel: ChannelView;
  members: MemberView[];
  onChannelChanged: (c?: ChannelView) => void;
  onMembersChanged: (m?: MemberView[]) => void;
  /// 自分が退出した。
  onLeft: () => void;
}) {
  const { me } = useMessagingDisplay();
  const isDm = channel.kind === "dm";
  const admin = channel.is_admin;
  const [name, setName] = React.useState(channel.name);
  const [topic, setTopic] = React.useState(channel.topic);
  const [saving, setSaving] = React.useState(false);
  const [aboutError, setAboutError] = React.useState<string | null>(null);
  const [invite, setInvite] = React.useState<PickedUser[]>([]);
  const [busy, setBusy] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (open) {
      setName(channel.name);
      setTopic(channel.topic);
      setAboutError(null);
      setInvite([]);
    }
  }, [open, channel.name, channel.topic]);

  const run = async (key: string, fn: () => Promise<void>, failTitle: string) => {
    setBusy(key);
    try {
      await fn();
    } catch (e) {
      toast({ title: failTitle, description: errorText(e), variant: "destructive" });
    } finally {
      setBusy(null);
    }
  };

  const saveAbout = async () => {
    setSaving(true);
    setAboutError(null);
    try {
      const patch: { name?: string; topic?: string } = {};
      if (name.trim() !== channel.name) patch.name = name.trim();
      if (topic.trim() !== channel.topic) patch.topic = topic.trim();
      onChannelChanged(await updateChannel(channel.id, patch));
      toast({ title: "チャンネルを更新しました" });
    } catch (e) {
      setAboutError(
        e instanceof MessagingError && e.status === 409 ? "同じ名前のチャンネルがあります。別の名前にしてください。" : errorText(e),
      );
    } finally {
      setSaving(false);
    }
  };

  const adminCount = members.filter((m) => m.is_admin).length;
  const lastAdmin = admin && adminCount <= 1;
  const dirty = name.trim() !== channel.name || topic.trim() !== channel.topic;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92dvh] overflow-y-auto sm:max-w-[560px]">
        <DialogHeader>
          <DialogTitle>{isDm ? `${channel.name} との DM` : `${channel.name} の設定`}</DialogTitle>
          <DialogDescription>
            {isDm
              ? "通知の受け方を変えられます。DM は退出できません（一覧から隠せます）。"
              : admin
                ? "あなたはこのチャンネルの管理者です。"
                : "名前やトピック・参加者の権限は、チャンネルの管理者が変えられます。"}
          </DialogDescription>
        </DialogHeader>

        <Tabs value={tab} onValueChange={(v) => onTabChange(v as SettingsTab)}>
          <TabsList className="w-full">
            {!isDm ? (
              <TabsTrigger value="about" className="flex-1">
                概要
              </TabsTrigger>
            ) : null}
            {!isDm ? (
              <TabsTrigger value="members" className="flex-1">
                参加者 {members.length}
              </TabsTrigger>
            ) : null}
            <TabsTrigger value="notify" className="flex-1">
              通知
            </TabsTrigger>
          </TabsList>

          {!isDm ? (
            <TabsContent value="about" className="flex flex-col gap-4 pt-2">
              <form
                className="flex flex-col gap-3"
                onSubmit={(e) => {
                  e.preventDefault();
                  if (admin && dirty) void saveAbout();
                }}
              >
                <div className="flex flex-col gap-1.5">
                  <label htmlFor="cs-name" className="text-[12.5px] font-medium">
                    名前
                  </label>
                  <Input id="cs-name" value={name} onChange={(e) => setName(e.target.value)} disabled={!admin} maxLength={80} />
                </div>
                <div className="flex flex-col gap-1.5">
                  <label htmlFor="cs-topic" className="text-[12.5px] font-medium">
                    トピック
                  </label>
                  <Input id="cs-topic" value={topic} onChange={(e) => setTopic(e.target.value)} disabled={!admin} maxLength={250} />
                </div>
                {aboutError ? (
                  <p className="text-[12.5px] text-destructive" role="alert">
                    {aboutError}
                  </p>
                ) : null}
                {admin ? (
                  <div className="flex justify-end">
                    <Button type="submit" size="sm" disabled={!dirty || !name.trim()} loading={saving}>
                      保存
                    </Button>
                  </div>
                ) : null}
              </form>

              <div className="shiki-dash-top flex flex-col gap-2 pt-4">
                {admin ? (
                  <div className="flex items-center justify-between gap-3">
                    <p className="text-[12.5px] text-muted-foreground">
                      {channel.archived
                        ? "アーカイブ中です。戻すと再び投稿できます。"
                        : "アーカイブすると読み取り専用になり、一覧の下にまとまります（元に戻せます）。"}
                    </p>
                    <Button
                      size="sm"
                      variant="outline"
                      loading={busy === "archive"}
                      onClick={() =>
                        void run(
                          "archive",
                          async () => onChannelChanged(await setArchived(channel.id, !channel.archived)),
                          "変更できませんでした",
                        )
                      }
                    >
                      {channel.archived ? <ArchiveRestore className="size-3.5" aria-hidden /> : <Archive className="size-3.5" aria-hidden />}
                      {channel.archived ? "アーカイブを戻す" : "アーカイブする"}
                    </Button>
                  </div>
                ) : null}
                {channel.joined ? (
                  <div className="flex items-center justify-between gap-3">
                    <p className="text-[12.5px] text-muted-foreground">
                      {lastAdmin
                        ? "最後の管理者は退出できません。先に他の参加者を管理者にしてください。"
                        : channel.kind === "private"
                          ? "退出すると、招待されるまで読めなくなります。"
                          : "退出しても公開チャンネルは読めます。"}
                    </p>
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={lastAdmin}
                      loading={busy === "leave"}
                      className="text-destructive hover:text-destructive"
                      onClick={() =>
                        void run(
                          "leave",
                          async () => {
                            await removeMember(channel.id, me);
                            onOpenChange(false);
                            onLeft();
                          },
                          "退出できませんでした",
                        )
                      }
                    >
                      <LogOut className="size-3.5" aria-hidden />
                      退出する
                    </Button>
                  </div>
                ) : null}
              </div>
            </TabsContent>
          ) : null}

          {!isDm ? (
            <TabsContent value="members" className="flex flex-col gap-3 pt-2">
              {(channel.kind === "public" ? channel.joined : admin) && !channel.archived ? (
                <div className="flex flex-col gap-2 rounded-xl border border-border/60 bg-card/40 p-3">
                  <p className="text-[12.5px] font-medium">招待する</p>
                  <UserMultiPicker
                    value={invite}
                    onChange={setInvite}
                    exclude={members.map((m) => m.user_id)}
                    label="招待する人を探す"
                  />
                  <div className="flex justify-end">
                    <Button
                      size="sm"
                      disabled={invite.length === 0}
                      loading={busy === "invite"}
                      onClick={() =>
                        void run(
                          "invite",
                          async () => {
                            onMembersChanged(await inviteMembers(channel.id, invite.map((u) => u.id)));
                            setInvite([]);
                            onChannelChanged();
                            toast({ title: `${invite.length} 名を招待しました` });
                          },
                          "招待できませんでした",
                        )
                      }
                    >
                      <UserPlus className="size-3.5" aria-hidden />
                      招待
                    </Button>
                  </div>
                </div>
              ) : null}

              <ul className="rule-soft overflow-hidden rounded-xl border border-border/60" aria-label="参加者">
                {members.map((m, i) => (
                  <li key={m.user_id} className={cn("flex flex-wrap items-center gap-2 px-3 py-2", i > 0 && "shiki-dash-top")}>
                    <UserAvatar userId={m.user_id} name={m.display_name} size="sm" />
                    <span className="min-w-0 flex-1">
                      <span className={cn("block truncate text-[13px] font-medium", !m.active && "text-muted-foreground")}>
                        {m.display_name}
                        {!m.active ? "（無効）" : ""}
                        {m.user_id === me ? <span className="ml-1 text-[11px] font-normal text-muted-foreground">（自分）</span> : null}
                      </span>
                      <span className="flex gap-1.5 text-[11px] text-muted-foreground">
                        {m.is_admin ? (
                          <span className="flex items-center gap-0.5">
                            <Crown className="size-3" aria-hidden />
                            管理者
                          </span>
                        ) : null}
                        {channel.announcement && m.can_post && !m.is_admin ? (
                          <span className="flex items-center gap-0.5">
                            <Megaphone className="size-3" aria-hidden />
                            投稿者
                          </span>
                        ) : null}
                        {!m.is_admin && !(channel.announcement && m.can_post) ? <span>参加者</span> : null}
                      </span>
                    </span>
                    {admin && m.user_id !== me ? (
                      <span className="flex flex-wrap gap-1">
                        <Button
                          size="sm"
                          variant="ghost"
                          loading={busy === `admin:${m.user_id}`}
                          onClick={() =>
                            void run(
                              `admin:${m.user_id}`,
                              async () => onMembersChanged(await setMemberRole(channel.id, m.user_id, { admin: !m.is_admin })),
                              "変更できませんでした",
                            )
                          }
                        >
                          {m.is_admin ? "管理者から外す" : "管理者にする"}
                        </Button>
                        {channel.announcement && !m.is_admin ? (
                          <Button
                            size="sm"
                            variant="ghost"
                            loading={busy === `poster:${m.user_id}`}
                            onClick={() =>
                              void run(
                                `poster:${m.user_id}`,
                                async () =>
                                  onMembersChanged(await setMemberRole(channel.id, m.user_id, { poster: !m.can_post })),
                                "変更できませんでした",
                              )
                            }
                          >
                            {m.can_post ? "投稿者から外す" : "投稿者にする"}
                          </Button>
                        ) : null}
                        <Button
                          size="sm"
                          variant="ghost"
                          aria-label={`${m.display_name} を外す`}
                          className="text-destructive hover:text-destructive"
                          loading={busy === `remove:${m.user_id}`}
                          onClick={() =>
                            void run(
                              `remove:${m.user_id}`,
                              async () => {
                                await removeMember(channel.id, m.user_id);
                                onMembersChanged();
                                onChannelChanged();
                              },
                              "外せませんでした",
                            )
                          }
                        >
                          <UserMinus className="size-3.5" aria-hidden />
                        </Button>
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
            </TabsContent>
          ) : null}

          <TabsContent value="notify" className="pt-2">
            <div className="flex flex-col gap-1.5" role="radiogroup" aria-label="通知の受け方">
              {NOTIFY.map((n) => {
                const on = channel.notify === n.value;
                return (
                  <button
                    key={n.value}
                    type="button"
                    role="radio"
                    aria-checked={on}
                    disabled={busy === "notify" || !channel.joined}
                    onClick={() =>
                      void run(
                        "notify",
                        async () => {
                          await setNotify(channel.id, n.value);
                          onChannelChanged({ ...channel, notify: n.value });
                        },
                        "変更できませんでした",
                      )
                    }
                    className={cn(
                      "flex flex-col gap-0.5 rounded-xl border px-3 py-2.5 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-60",
                      on ? "border-border bg-accent" : "border-border/60 bg-card/40 hover:bg-accent/50",
                    )}
                  >
                    <span className="text-[13px] font-medium">{n.label}</span>
                    <span className="text-[11.5px] text-muted-foreground">{n.note}</span>
                  </button>
                );
              })}
              {!channel.joined ? (
                <p className="text-[12px] text-muted-foreground">参加すると通知の受け方を選べます。</p>
              ) : null}
            </div>
          </TabsContent>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}
