"use client";

/// 回答への 👍/👎（#751）。👎 のときは理由を任意で 1 つ選べる（定型の選択肢だけ・自由記述は
/// 持たない）。評価は本人だけが見え、管理者には組織全体の集計としてだけ出る。

import * as React from "react";
import { ThumbsDown, ThumbsUp } from "lucide-react";

import {
  clearMessageFeedback,
  FEEDBACK_REASONS,
  fetchThreadFeedback,
  setMessageFeedback,
  type FeedbackCategory,
  type FeedbackItem,
  type FeedbackRating,
} from "@/lib/insights-api";
import { cn } from "@/lib/utils";
import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";
import { toast } from "@/components/ui/use-toast";
import { FooterButton } from "./message-footer";

type Entry = { rating: FeedbackRating; category: FeedbackCategory | null };

type Ctx = {
  get: (messageId: string) => Entry | undefined;
  set: (messageId: string, entry: Entry | null) => Promise<void>;
};

const FeedbackContext = React.createContext<Ctx | null>(null);

/// 会話 1 つ分の自分の評価（開いたときに 1 回読む）。
export function ThreadFeedbackProvider({
  threadId,
  children,
}: {
  threadId: string;
  children: React.ReactNode;
}) {
  const [entries, setEntries] = React.useState<Record<string, Entry>>({});
  /// 同じ回答への保存は順に送る（👎 の直後に理由を選ぶと 2 本が並走し、後から届いた方で
  /// 理由が消えることがある）。
  const queues = React.useRef(new Map<string, Promise<void>>());
  /// サーバに保存できている値（失敗したらここへ戻す）。
  const saved = React.useRef<Record<string, Entry | undefined>>({});
  /// 回答ごとの最新の操作番号（古い操作の失敗で新しい表示を巻き戻さない）。
  const latest = React.useRef(new Map<string, number>());

  React.useEffect(() => {
    let active = true;
    setEntries({});
    saved.current = {};
    queues.current = new Map();
    latest.current = new Map();
    fetchThreadFeedback(threadId)
      .then((items: FeedbackItem[]) => {
        if (!active) return;
        const loaded: Record<string, Entry> = Object.fromEntries(
          items.map((i) => [i.message_id, { rating: i.rating, category: i.category ?? null }]),
        );
        saved.current = { ...loaded };
        setEntries(loaded);
      })
      .catch(() => {
        // 読めなくても評価は付けられる（表示が未選択から始まるだけ）
      });
    return () => {
      active = false;
    };
  }, [threadId]);

  const set = React.useCallback(
    (messageId: string, entry: Entry | null) => {
      const apply = (e: Entry | null | undefined) =>
        setEntries((cur) => {
          const next = { ...cur };
          if (e) next[messageId] = e;
          else delete next[messageId];
          return next;
        });
      apply(entry);
      const seq = (latest.current.get(messageId) ?? 0) + 1;
      latest.current.set(messageId, seq);
      const run = async () => {
        try {
          if (entry) await setMessageFeedback(threadId, messageId, entry.rating, entry.category);
          else await clearMessageFeedback(threadId, messageId);
          saved.current[messageId] = entry ?? undefined;
        } catch (e) {
          if (latest.current.get(messageId) === seq) apply(saved.current[messageId]);
          toast({
            description: `評価を保存できませんでした${e instanceof Error ? `（${e.message}）` : ""}`,
          });
        }
      };
      const next = (queues.current.get(messageId) ?? Promise.resolve()).then(run);
      queues.current.set(messageId, next);
      return next;
    },
    [threadId],
  );

  const value = React.useMemo<Ctx>(() => ({ get: (id) => entries[id], set }), [entries, set]);
  return <FeedbackContext.Provider value={value}>{children}</FeedbackContext.Provider>;
}

/// 回答の下の 👍/👎（Provider の外では出さない）。
export function MessageFeedbackButtons({ messageId }: { messageId: string }) {
  const ctx = React.useContext(FeedbackContext);
  const [reasonOpen, setReasonOpen] = React.useState(false);
  if (!ctx) return null;
  const entry = ctx.get(messageId);

  const up = () => void ctx.set(messageId, entry?.rating === "up" ? null : { rating: "up", category: null });
  const down = () => {
    if (entry?.rating === "down") {
      setReasonOpen(false);
      void ctx.set(messageId, null);
      return;
    }
    void ctx.set(messageId, { rating: "down", category: null });
    setReasonOpen(true);
  };
  const choose = (category: FeedbackCategory) => {
    setReasonOpen(false);
    void ctx.set(messageId, {
      rating: "down",
      category: entry?.category === category ? null : category,
    });
  };

  return (
    <Popover open={reasonOpen} onOpenChange={setReasonOpen}>
      <span className="contents">
        <FooterButton label="役に立った" pressed={entry?.rating === "up"} onClick={up}>
          <ThumbsUp className={cn("size-3.5", entry?.rating === "up" && "fill-current")} />
        </FooterButton>
        <PopoverAnchor asChild>
          <span className="inline-flex">
            <FooterButton label="役に立たなかった" pressed={entry?.rating === "down"} onClick={down}>
              <ThumbsDown className={cn("size-3.5", entry?.rating === "down" && "fill-current")} />
            </FooterButton>
          </span>
        </PopoverAnchor>
      </span>
      <PopoverContent align="start" className="w-64 p-2" onOpenAutoFocus={(e) => e.preventDefault()}>
        <p className="px-1.5 pb-1.5 pt-0.5 text-xs text-muted-foreground">
          よければ理由を教えてください（任意）
        </p>
        <div className="flex flex-wrap gap-1.5" role="group" aria-label="役に立たなかった理由">
          {FEEDBACK_REASONS.map((r) => (
            <button
              key={r.value}
              type="button"
              aria-pressed={entry?.category === r.value}
              onClick={() => choose(r.value)}
              className={cn(
                "rounded-full border px-2.5 py-1 text-xs transition-colors",
                "hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                "pointer-coarse:py-2",
                entry?.category === r.value && "border-transparent bg-secondary font-medium",
              )}
            >
              {r.label}
            </button>
          ))}
        </div>
      </PopoverContent>
    </Popover>
  );
}
