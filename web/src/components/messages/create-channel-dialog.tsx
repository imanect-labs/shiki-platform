"use client";

/// チャンネルの作成と招待。公開 / 非公開の選択が **そのまま閲覧できる範囲**（design §4.14）に
/// 対応することを、選択肢の説明文で示す。

import * as React from "react";
import { Globe, Lock } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { createChannel, MessagingError, type ChannelView } from "@/lib/messaging-api";
import { UserMultiPicker, type PickedUser } from "./user-picker";

const KINDS = [
  { kind: "public", icon: Globe, label: "公開", note: "組織の全員が読め、誰でも参加できます。" },
  { kind: "private", icon: Lock, label: "非公開", note: "招待された人だけが読めます。一覧にも出ません。" },
] as const;

export function CreateChannelDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (c: ChannelView) => void;
}) {
  const [kind, setKind] = React.useState<"public" | "private">("public");
  const [name, setName] = React.useState("");
  const [topic, setTopic] = React.useState("");
  const [announcement, setAnnouncement] = React.useState(false);
  const [invited, setInvited] = React.useState<PickedUser[]>([]);
  const [error, setError] = React.useState<string | null>(null);
  const [saving, setSaving] = React.useState(false);

  // 開くたびに初期状態へ戻す（前回の入力が残らない）。
  React.useEffect(() => {
    if (open) {
      setKind("public");
      setName("");
      setTopic("");
      setAnnouncement(false);
      setInvited([]);
      setError(null);
    }
  }, [open]);

  const valid = name.trim().length > 0;

  const submit = async () => {
    if (!valid || saving) return;
    setSaving(true);
    setError(null);
    try {
      const c = await createChannel({
        name: name.trim(),
        topic: topic.trim(),
        kind,
        announcement,
        members: invited.map((u) => u.id),
      });
      onCreated(c);
      onOpenChange(false);
    } catch (e) {
      setError(
        e instanceof MessagingError && e.status === 409
          ? "同じ名前のチャンネルがあります。別の名前にしてください。"
          : e instanceof Error
            ? e.message
            : String(e),
      );
    } finally {
      setSaving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[92dvh] overflow-y-auto sm:max-w-[520px]">
        <DialogHeader>
          <DialogTitle>チャンネルを作成</DialogTitle>
          <DialogDescription>
            話題や案件ごとに場を分けます。公開／非公開の選択が、そのまま閲覧できる範囲になります。
          </DialogDescription>
        </DialogHeader>

        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <div className="grid grid-cols-2 gap-2" role="radiogroup" aria-label="種類">
            {KINDS.map((k) => {
              const active = kind === k.kind;
              return (
                <button
                  key={k.kind}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  onClick={() => setKind(k.kind)}
                  className={cn(
                    "flex flex-col gap-1 rounded-xl border px-3 py-2.5 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                    active ? "border-border bg-accent" : "border-border/60 bg-card/40 hover:bg-accent/50",
                  )}
                >
                  <span className="flex items-center gap-1.5 text-[13px] font-medium text-foreground">
                    <k.icon className="size-4 text-muted-foreground" aria-hidden />
                    {k.label}
                  </span>
                  <span className="text-[11.5px] leading-snug text-muted-foreground">{k.note}</span>
                </button>
              );
            })}
          </div>

          <div className="flex flex-col gap-1.5">
            <label htmlFor="ch-name" className="text-[12.5px] font-medium text-foreground">
              名前
            </label>
            <Input
              id="ch-name"
              value={name}
              onChange={(e) => {
                setName(e.target.value);
                setError(null);
              }}
              placeholder="例: 総務-備品管理"
              maxLength={80}
              aria-invalid={error ? true : undefined}
              aria-describedby={error ? "ch-error" : undefined}
            />
          </div>

          <div className="flex flex-col gap-1.5">
            <label htmlFor="ch-topic" className="text-[12.5px] font-medium text-foreground">
              トピック（任意）
            </label>
            <Input
              id="ch-topic"
              value={topic}
              onChange={(e) => setTopic(e.target.value)}
              placeholder="このチャンネルで何を話すか"
              maxLength={250}
            />
          </div>

          <label className="flex items-start gap-3 rounded-xl border border-border/60 bg-card/40 px-3 py-2.5">
            <Switch checked={announcement} onCheckedChange={setAnnouncement} aria-label="アナウンス専用" className="mt-0.5" />
            <span className="min-w-0">
              <span className="block text-[13px] font-medium text-foreground">アナウンス専用</span>
              <span className="block text-[11.5px] leading-snug text-muted-foreground">
                投稿できるのは管理者と指名した投稿者だけです。ほかの人はリアクションとスレッドの閲覧ができます。
              </span>
            </span>
          </label>

          <div className="flex flex-col gap-1.5">
            <p className="text-[12.5px] font-medium text-foreground">
              招待{invited.length > 0 ? `（${invited.length} 名）` : "（任意）"}
            </p>
            <UserMultiPicker value={invited} onChange={setInvited} label="招待する人" />
            {kind === "public" ? (
              <p className="text-[11px] leading-snug text-muted-foreground">
                公開チャンネルは招待しなくても組織の全員が読めます。招待した人は最初から一覧に出ます。
              </p>
            ) : null}
          </div>

          {error ? (
            <p id="ch-error" className="text-[12.5px] text-destructive" role="alert">
              {error}
            </p>
          ) : null}

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              キャンセル
            </Button>
            <Button type="submit" disabled={!valid} loading={saving}>
              作成する
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
