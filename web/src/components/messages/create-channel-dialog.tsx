"use client";

/// チャンネルの作成と招待。公開 / 非公開の選択が **そのまま認可の形**（design §4.14）に対応する
/// ことを、選択肢の説明文で示す。

import * as React from "react";
import { Check, Globe, Lock } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { MEMBERS, type ChannelKind } from "@/lib/messages-mock";
import { MemberAvatar } from "./primitives";

const KINDS: {
  kind: Extract<ChannelKind, "public" | "private">;
  icon: typeof Globe;
  label: string;
  note: string;
}[] = [
  {
    kind: "public",
    icon: Globe,
    label: "公開",
    note: "組織の全員が参加でき、発言を読めます。",
  },
  {
    kind: "private",
    icon: Lock,
    label: "非公開",
    note: "招待された人だけが参加でき、一覧にも出ません。",
  },
];

export function CreateChannelDialog({
  open,
  viewerId,
  onOpenChange,
  onCreate,
}: {
  open: boolean;
  viewerId: string;
  onOpenChange: (open: boolean) => void;
  onCreate: (input: {
    kind: Extract<ChannelKind, "public" | "private">;
    name: string;
    topic: string;
    memberIds: string[];
  }) => void;
}) {
  const [kind, setKind] = React.useState<"public" | "private">("public");
  const [name, setName] = React.useState("");
  const [topic, setTopic] = React.useState("");
  const [invited, setInvited] = React.useState<string[]>([]);

  // 開くたびに初期状態へ戻す（前回の入力が残らない）。
  React.useEffect(() => {
    if (open) {
      setKind("public");
      setName("");
      setTopic("");
      setInvited([]);
    }
  }, [open]);

  const candidates = MEMBERS.filter((m) => m.id !== "shiki" && m.id !== viewerId);
  const valid = name.trim().length > 0;

  const submit = () => {
    if (!valid) return;
    onCreate({
      kind,
      name: name.trim(),
      topic: topic.trim(),
      memberIds: [viewerId, ...invited],
    });
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[520px]">
        <DialogHeader>
          <DialogTitle>チャンネルを作成</DialogTitle>
          <DialogDescription>
            話題や案件ごとに場を分けます。公開／非公開の選択が、そのまま閲覧できる範囲になります。
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-col gap-4">
          <div className="grid grid-cols-2 gap-2">
            {KINDS.map((k) => {
              const active = kind === k.kind;
              return (
                <button
                  key={k.kind}
                  type="button"
                  onClick={() => setKind(k.kind)}
                  aria-pressed={active}
                  className={cn(
                    "flex flex-col gap-1 rounded-xl border px-3 py-2.5 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring",
                    active
                      ? "border-border bg-accent"
                      : "border-border/60 bg-card/40 hover:bg-accent/50",
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
              onChange={(e) => setName(e.target.value)}
              placeholder="例: 総務-備品管理"
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
            />
          </div>

          <div className="flex flex-col gap-1.5">
            <p className="text-[12.5px] font-medium text-foreground">
              招待{invited.length > 0 ? `（${invited.length} 名）` : ""}
            </p>
            <div className="rule-soft flex flex-col overflow-hidden rounded-xl border border-border/60">
              {candidates.map((m, i) => {
                const on = invited.includes(m.id);
                return (
                  <button
                    key={m.id}
                    type="button"
                    onClick={() =>
                      setInvited((prev) =>
                        prev.includes(m.id) ? prev.filter((x) => x !== m.id) : [...prev, m.id],
                      )
                    }
                    aria-pressed={on}
                    className={cn(
                      "flex items-center gap-2.5 px-3 py-2 text-left outline-none transition-colors hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
                      i > 0 && "shiki-dash-top",
                      on && "bg-accent/60",
                    )}
                  >
                    <MemberAvatar memberId={m.id} size="sm" />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[13px] font-medium text-foreground">
                        {m.name}
                      </span>
                      <span className="block truncate text-[11px] text-muted-foreground">
                        {m.dept}
                      </span>
                    </span>
                    <span
                      className={cn(
                        "flex size-5 items-center justify-center rounded-md border",
                        on
                          ? "border-transparent bg-primary text-primary-foreground"
                          : "border-border",
                      )}
                    >
                      {on ? <Check className="size-3.5" aria-hidden /> : null}
                    </span>
                  </button>
                );
              })}
            </div>
            {kind === "public" ? (
              <p className="text-[11px] leading-snug text-muted-foreground">
                公開チャンネルは招待しなくても組織の全員が参加できます。
              </p>
            ) : null}
          </div>
        </div>

        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            キャンセル
          </Button>
          <Button onClick={submit} disabled={!valid}>
            作成する
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
