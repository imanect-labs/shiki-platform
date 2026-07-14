"use client";

/// レイアウト/コンテンツ系 generative UI コンポーネント（PR2・すべて表示専用）。
/// 配色は四季/セマンティックトークン。accordion/tabs は循環 import を避けるため
/// 子の描画を `renderChildren`（SpecRenderer 側の NodeView）に委ねる。

import * as React from "react";

import { AlertCircle, AlertTriangle, CheckCircle2, Info } from "lucide-react";

import type {
  AccordionProps,
  BadgeListProps,
  BadgeTone,
  CalloutProps,
  CalloutTone,
  CodeBlockProps,
  KeyValueProps,
  StepperProps,
  StepStatus,
  TabsProps,
  UiNode,
} from "@/generated/gui-spec";
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from "@/components/ui/accordion";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";

type RenderChildren = (nodes: UiNode[]) => React.ReactNode;

/// callout: トーンごとの色（境界/背景/前景）とアイコン。
const CALLOUT_META: Record<CalloutTone, { icon: React.ElementType; cls: string; iconCls: string }> = {
  info: { icon: Info, cls: "border-primary/30 bg-primary/5", iconCls: "text-primary" },
  success: {
    icon: CheckCircle2,
    cls: "border-[var(--season-summer)]/40 bg-[var(--season-summer)]/10",
    iconCls: "text-[var(--season-summer)]",
  },
  warning: {
    icon: AlertTriangle,
    cls: "border-amber-500/40 bg-amber-500/10",
    iconCls: "text-amber-600 dark:text-amber-400",
  },
  danger: {
    icon: AlertCircle,
    cls: "border-destructive/40 bg-destructive/10",
    iconCls: "text-destructive",
  },
};

export function GenUiCallout({ callout }: { callout: CalloutProps }) {
  const meta = CALLOUT_META[callout.tone] ?? CALLOUT_META.info;
  const Icon = meta.icon;
  return (
    <div
      data-testid="genui-callout"
      className={cn("flex gap-2.5 rounded-xl border p-3.5", meta.cls)}
      role="note"
    >
      <Icon className={cn("mt-0.5 size-4 shrink-0", meta.iconCls)} aria-hidden />
      <div className="min-w-0">
        {callout.title ? (
          <p className="mb-0.5 text-[13px] font-semibold text-foreground">{callout.title}</p>
        ) : null}
        <p className="whitespace-pre-wrap text-[13px] leading-relaxed text-foreground/85">
          {callout.text}
        </p>
      </div>
    </div>
  );
}

const STEP_META: Record<StepStatus, { dot: string; text: string; ring: string; label: string }> = {
  todo: { dot: "bg-muted-foreground/30", text: "text-foreground/70", ring: "border-border", label: "未着手" },
  doing: { dot: "bg-primary", text: "text-foreground font-medium", ring: "border-primary", label: "進行中" },
  done: {
    dot: "bg-[var(--season-summer)]",
    text: "text-foreground/60",
    ring: "border-[var(--season-summer)]/50",
    label: "完了",
  },
};

export function GenUiStepper({ stepper }: { stepper: StepperProps }) {
  const steps = stepper.steps ?? [];
  return (
    <ol data-testid="genui-stepper" className="space-y-0">
      {steps.map((s, i) => {
        const meta = STEP_META[s.status] ?? STEP_META.todo;
        const last = i === steps.length - 1;
        return (
          <li key={i} className="flex gap-3">
            <div className="flex flex-col items-center">
              <span
                className={cn(
                  "flex size-5 items-center justify-center rounded-full border bg-card",
                  meta.ring,
                )}
              >
                <span className={cn("size-2 rounded-full", meta.dot)} aria-hidden />
              </span>
              {!last ? <span className="w-px flex-1 bg-border" aria-hidden /> : null}
            </div>
            <div className={cn("pb-4 text-[13px]", meta.text)}>
              {/* 状態を色だけに頼らず読み上げ/高コントラストにも伝える。 */}
              <span className="sr-only">{`（${meta.label}）`}</span>
              {s.title}
              {s.description ? (
                <p className="mt-0.5 text-[12px] text-muted-foreground">{s.description}</p>
              ) : null}
            </div>
          </li>
        );
      })}
    </ol>
  );
}

const BADGE_VARIANT: Record<BadgeTone, "outline" | "default" | "success" | "warning" | "destructive"> = {
  neutral: "outline",
  info: "default",
  success: "success",
  warning: "warning",
  danger: "destructive",
};

export function GenUiBadgeList({ badgeList }: { badgeList: BadgeListProps }) {
  return (
    <div data-testid="genui-badge-list" className="flex flex-wrap gap-1.5">
      {(badgeList.badges ?? []).map((b, i) => (
        <Badge key={i} variant={BADGE_VARIANT[b.tone] ?? "outline"}>
          {b.label}
        </Badge>
      ))}
    </div>
  );
}

export function GenUiKeyValue({ keyValue }: { keyValue: KeyValueProps }) {
  const items = keyValue.items ?? [];
  return (
    <div data-testid="genui-key-value" className="min-w-0">
      {keyValue.title ? (
        <p className="mb-2 text-sm font-medium tracking-tight text-foreground">{keyValue.title}</p>
      ) : null}
      {/* 区切りは既存画面と同じ破線グラデ（shiki-dash）。枠は柔らかく背景は半透明。 */}
      <dl className="overflow-hidden rounded-xl border border-border/60 bg-card/40">
        {items.map((kv, i) => (
          <div
            key={i}
            className={cn(
              "grid grid-cols-[minmax(6rem,32%)_1fr] gap-3 px-3.5 py-2.5",
              i < items.length - 1 && "shiki-dash-bottom",
            )}
          >
            <dt className="truncate text-[13px] text-muted-foreground">{kv.key}</dt>
            <dd className="min-w-0 whitespace-pre-wrap break-words text-[13px] text-foreground">
              {kv.value}
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

export function GenUiCodeBlock({ codeBlock }: { codeBlock: CodeBlockProps }) {
  return (
    <figure data-testid="genui-code-block" className="min-w-0">
      {codeBlock.language ? (
        <figcaption className="mb-1 font-mono text-[11px] uppercase tracking-wide text-muted-foreground">
          {codeBlock.language}
        </figcaption>
      ) : null}
      <pre className="overflow-x-auto rounded-xl border border-border/60 bg-secondary/40 p-3.5 text-[12px] leading-relaxed">
        <code className="font-mono text-foreground/90">{codeBlock.code}</code>
      </pre>
    </figure>
  );
}

export function GenUiAccordion({
  accordion,
  renderChildren,
}: {
  accordion: AccordionProps;
  renderChildren: RenderChildren;
}) {
  const items = accordion.items ?? [];
  const defaultOpen = items
    .map((it, i) => (it.open ? String(i) : null))
    .filter((v): v is string => v !== null);
  return (
    <Accordion
      type="multiple"
      defaultValue={defaultOpen}
      data-testid="genui-accordion"
      className="overflow-hidden rounded-xl border border-border/60 bg-card/40"
    >
      {items.map((it, i) => (
        <AccordionItem
          key={i}
          value={String(i)}
          className={cn("px-3.5", i < items.length - 1 && "shiki-dash-bottom")}
        >
          <AccordionTrigger className="text-sm font-medium">{it.title}</AccordionTrigger>
          <AccordionContent>
            <div className="flex flex-col gap-3 pb-1">{renderChildren(it.children)}</div>
          </AccordionContent>
        </AccordionItem>
      ))}
    </Accordion>
  );
}

export function GenUiTabs({
  tabs,
  renderChildren,
}: {
  tabs: TabsProps;
  renderChildren: RenderChildren;
}) {
  const list = tabs.tabs ?? [];
  if (list.length === 0) return null;
  return (
    <Tabs defaultValue="0" data-testid="genui-tabs" className="min-w-0">
      {/* タブが多い/ラベルが長い場合もカードからはみ出さないよう折り返す。 */}
      <TabsList className="h-auto flex-wrap">
        {list.map((t, i) => (
          <TabsTrigger key={i} value={String(i)}>
            {t.label}
          </TabsTrigger>
        ))}
      </TabsList>
      {list.map((t, i) => (
        <TabsContent key={i} value={String(i)}>
          <div className="flex flex-col gap-3">{renderChildren(t.children)}</div>
        </TabsContent>
      ))}
    </Tabs>
  );
}
