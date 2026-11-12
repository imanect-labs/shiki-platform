"use client";

/// エージェントのタスク（計画のサブタスク）パネル。
///
/// - 広い画面: 会話の右カラム（本文に重ねない・開閉で本文の位置を動かさない）。
/// - 狭い画面・分割ビュー: 右（スマホは下）から出るシート。
///
/// 各タスクの下に「そのタスクの間に何をしたか」（ツールの操作）を出す。着手したタスクは開いたまま、
/// 未着手のタスクは行を押したときに開く。見た目の切替でレイアウトを跳ねさせない（アニメーションなし）。

import * as React from "react";
import { CheckCircle2, ChevronRight, Circle, Loader2, PauseCircle, X } from "lucide-react";

import { Sheet, SheetBody, SheetContent, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import type { PlanSubtask } from "@/lib/chat-api";
import { describeTool, withTense } from "@/lib/tool-display";
import { cn } from "@/lib/utils";

import { wasRejected, type ToolActivityItem } from "./tool-activity";

/// タスクごとの操作（ツール呼び出し）。キーはサブタスク id。
export type TaskActivities = Record<string, ToolActivityItem[]>;

const STATUS: Record<
  string,
  { label: string; icon: React.ComponentType<{ className?: string }>; iconClass: string }
> = {
  todo: { label: "未着手", icon: Circle, iconClass: "text-muted-foreground/50" },
  doing: { label: "進行中", icon: Loader2, iconClass: "animate-spin text-primary" },
  done: { label: "完了", icon: CheckCircle2, iconClass: "text-emerald-600 dark:text-emerald-400" },
  blocked: { label: "保留", icon: PauseCircle, iconClass: "text-amber-600 dark:text-amber-400" },
};

/// 完了数と、いま進めているタスク。
export function taskSummary(tasks: PlanSubtask[]) {
  const done = tasks.filter((t) => t.status === "done").length;
  const current =
    tasks.find((t) => t.status === "doing") ?? tasks.find((t) => t.status !== "done") ?? null;
  return { done, total: tasks.length, current };
}

function TaskList({ tasks, activities }: { tasks: PlanSubtask[]; activities: TaskActivities }) {
  // 利用者が開いた/閉じたタスク。未操作のタスクは「着手したら開いたまま」（完了で勝手に閉じない＝
  // 進むたびに行が開いたり閉じたりしない）。
  const [toggled, setToggled] = React.useState<Record<string, boolean>>({});
  return (
    <ol className="flex flex-col" data-testid="task-list">
      {tasks.map((t, i) => {
        const meta = STATUS[t.status] ?? STATUS.todo;
        const Icon = meta.icon;
        const items = activities[t.id] ?? [];
        const open = toggled[t.id] ?? t.status !== "todo";
        const detailId = `task-detail-${t.id}`;
        return (
          <li key={t.id} className="relative" data-testid="task-row" data-status={t.status}>
            {/* タスク間の縦線（最後は引かない）。 */}
            {i < tasks.length - 1 ? (
              <span aria-hidden className="absolute bottom-0 left-[21px] top-8 w-px bg-border" />
            ) : null}
            <button
              type="button"
              aria-expanded={open}
              aria-controls={detailId}
              onClick={() => setToggled((m) => ({ ...m, [t.id]: !open }))}
              className="flex w-full items-start gap-2.5 rounded-md px-3 py-1.5 text-left hover:bg-secondary/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <Icon className={cn("mt-[3px] size-4 shrink-0", meta.iconClass)} aria-hidden />
              <span className="sr-only">{meta.label}: </span>
              <span
                className={cn(
                  "min-w-0 flex-1 text-[13px] leading-5 [overflow-wrap:anywhere]",
                  t.status === "done" ? "text-muted-foreground" : "text-foreground",
                  t.status === "doing" && "font-medium",
                )}
                data-testid="task-title"
              >
                {t.title}
              </span>
              <ChevronRight
                aria-hidden
                className={cn("mt-[3px] size-3.5 shrink-0 text-muted-foreground/70", open && "rotate-90")}
              />
            </button>
            {open ? (
              <div id={detailId} className="pb-2 pl-[38px] pr-3" data-testid="task-detail">
                {items.length === 0 ? (
                  <p className="py-0.5 text-[12px] text-muted-foreground">
                    {t.status === "todo"
                      ? "まだ着手していません。"
                      : t.status === "doing"
                        ? "考えています…"
                        : "ツールは使っていません。"}
                  </p>
                ) : (
                  <ul className="flex flex-col gap-1">
                    {items.map((a) => (
                      <ActivityLine key={a.key} item={a} />
                    ))}
                  </ul>
                )}
              </div>
            ) : null}
          </li>
        );
      })}
    </ol>
  );
}

function ActivityLine({ item }: { item: ToolActivityItem }) {
  const d = describeTool(item);
  const Icon = d.icon;
  const rejected = !item.running && wasRejected(item);
  const failed = !item.running && item.ok === false && !rejected;
  const text = rejected ? `${d.lead}を却下しました` : withTense(d, item.running, failed);
  return (
    <li className="flex items-start gap-1.5 text-[12px] leading-[18px]" data-testid="task-activity">
      {item.running ? (
        <Loader2 aria-hidden className="mt-[2px] size-3 shrink-0 animate-spin text-muted-foreground" />
      ) : (
        <Icon aria-hidden className="mt-[2px] size-3 shrink-0 text-muted-foreground" />
      )}
      <span
        className={cn(
          "min-w-0 [overflow-wrap:anywhere]",
          failed ? "text-destructive" : "text-muted-foreground",
        )}
      >
        {text}
      </span>
    </li>
  );
}

function PanelHeader({ tasks }: { tasks: PlanSubtask[] }) {
  const { done, total } = taskSummary(tasks);
  return (
    <div className="flex items-baseline gap-2">
      <span className="text-[14px] font-semibold text-foreground">タスク</span>
      <span className="text-[12px] tabular-nums text-muted-foreground" data-testid="task-progress">
        {done}/{total} 完了
      </span>
    </div>
  );
}

/// 右カラム（広い画面）。
export function TaskPanelAside({
  tasks,
  activities,
  onClose,
}: {
  tasks: PlanSubtask[];
  activities: TaskActivities;
  onClose: () => void;
}) {
  return (
    <aside
      aria-label="タスク"
      data-testid="task-panel"
      className="flex h-full min-h-0 w-80 shrink-0 flex-col border-l border-border bg-background"
    >
      <div className="flex h-14 shrink-0 items-center justify-between pl-4 pr-2">
        <PanelHeader tasks={tasks} />
        <button
          type="button"
          onClick={onClose}
          aria-label="タスクを閉じる"
          title="タスクを閉じる"
          className="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground hover:bg-secondary hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          data-testid="task-panel-close"
        >
          <X className="size-4" aria-hidden />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-1 pb-6">
        <TaskList tasks={tasks} activities={activities} />
      </div>
    </aside>
  );
}

/// シート（狭い画面・分割ビュー）。
export function TaskPanelSheet({
  tasks,
  activities,
  open,
  onOpenChange,
  side,
}: {
  tasks: PlanSubtask[];
  activities: TaskActivities;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  side: "right" | "bottom";
}) {
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent side={side} className="md:max-w-sm" data-testid="task-panel">
        <SheetHeader>
          <SheetTitle asChild>
            <div>
              <PanelHeader tasks={tasks} />
            </div>
          </SheetTitle>
        </SheetHeader>
        <SheetBody className="px-1">
          <TaskList tasks={tasks} activities={activities} />
        </SheetBody>
      </SheetContent>
    </Sheet>
  );
}

/// 回答内の 1 行（上のバーが無い分割ビュー）。押すとタスクのシートを開く。枠は付けない。
export function TaskInlineLine({ tasks, onOpen }: { tasks: PlanSubtask[]; onOpen: () => void }) {
  const { done, total, current } = taskSummary(tasks);
  return (
    <button
      type="button"
      onClick={onOpen}
      className="group flex max-w-full items-center gap-2 rounded-md py-1 text-left text-[13px] text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      data-testid="task-inline"
    >
      {current?.status === "doing" ? (
        <Loader2 className="size-3.5 shrink-0 animate-spin text-primary" aria-hidden />
      ) : (
        <CheckCircle2 className="size-3.5 shrink-0" aria-hidden />
      )}
      <span className="shrink-0 tabular-nums">タスク {done}/{total}</span>
      {current ? (
        <>
          <span aria-hidden>·</span>
          <span className="min-w-0 truncate">{current.title}</span>
        </>
      ) : null}
      <ChevronRight className="size-3.5 shrink-0 opacity-60 group-hover:opacity-100" aria-hidden />
    </button>
  );
}
