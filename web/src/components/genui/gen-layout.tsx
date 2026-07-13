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
      className={cn("flex gap-2.5 rounded-xl border p-3", meta.cls)}
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

const STEP_META: Record<StepStatus, { dot: string; text: string; ring: string }> = {
  todo: { dot: "bg-muted-foreground/30", text: "text-foreground/70", ring: "border-border" },
  doing: { dot: "bg-primary", text: "text-foreground font-medium", ring: "border-primary" },
  done: { dot: "bg-[var(--season-summer)]", text: "text-foreground/60", ring: "border-[var(--season-summer)]/50" },
};

export function GenUiStepper({ stepper }: { stepper: StepperProps }) {
  return (
    <ol data-testid="genui-stepper" className="space-y-0">
      {stepper.steps.map((s, i) => {
        const meta = STEP_META[s.status] ?? STEP_META.todo;
        const last = i === stepper.steps.length - 1;
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
      {badgeList.badges.map((b, i) => (
        <Badge key={i} variant={BADGE_VARIANT[b.tone] ?? "outline"}>
          {b.label}
        </Badge>
      ))}
    </div>
  );
}

export function GenUiKeyValue({ keyValue }: { keyValue: KeyValueProps }) {
  return (
    <div data-testid="genui-key-value" className="min-w-0">
      {keyValue.title ? (
        <p className="mb-2 text-[13px] font-semibold text-foreground/80">{keyValue.title}</p>
      ) : null}
      <dl className="divide-y divide-border/60 rounded-xl border border-border">
        {keyValue.items.map((kv, i) => (
          <div key={i} className="grid grid-cols-[minmax(6rem,32%)_1fr] gap-2 px-3 py-2">
            <dt className="truncate text-[12px] text-muted-foreground">{kv.key}</dt>
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
      <pre className="overflow-x-auto rounded-lg border border-border bg-secondary/50 p-3 text-[12px] leading-relaxed">
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
  const defaultOpen = accordion.items
    .map((it, i) => (it.open ? String(i) : null))
    .filter((v): v is string => v !== null);
  return (
    <Accordion
      type="multiple"
      defaultValue={defaultOpen}
      data-testid="genui-accordion"
      className="rounded-xl border border-border"
    >
      {accordion.items.map((it, i) => (
        <AccordionItem key={i} value={String(i)} className="border-b border-border/60 last:border-b-0 px-3">
          <AccordionTrigger className="text-[13px] font-medium">{it.title}</AccordionTrigger>
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
  if (tabs.tabs.length === 0) return null;
  return (
    <Tabs defaultValue="0" data-testid="genui-tabs" className="min-w-0">
      <TabsList>
        {tabs.tabs.map((t, i) => (
          <TabsTrigger key={i} value={String(i)}>
            {t.label}
          </TabsTrigger>
        ))}
      </TabsList>
      {tabs.tabs.map((t, i) => (
        <TabsContent key={i} value={String(i)}>
          <div className="flex flex-col gap-3">{renderChildren(t.children)}</div>
        </TabsContent>
      ))}
    </Tabs>
  );
}
