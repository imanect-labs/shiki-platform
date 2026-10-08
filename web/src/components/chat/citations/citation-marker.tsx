"use client";

/// 本文中の引用番号チップ（issue #505・案 C）。
///
/// - マウス: ホバーで原文カード、クリックで出典パネル。
/// - タッチ: タップで原文カード（ホバーが無いため）。カードの「出典を開く」でパネルへ。
/// - キーボード: Tab でフォーカス、Enter / Space で出典パネル。
///
/// メッセージの引用文脈（`MessageCitationsProvider`）の外では、従来どおり番号だけを描く。
import * as React from "react";
import * as PopoverPrimitive from "@radix-ui/react-popover";
import { PanelRight } from "lucide-react";

import { cn } from "@/lib/utils";
import { citationAt } from "@/lib/citation";
import {
  passageOf,
  useMessageCitations,
  useNodeMeta,
  useSourcePanel,
  type MessageCitations,
} from "./citation-context";
import {
  DocIcon,
  KindBadge,
  OpenOriginalLink,
  SnippetText,
  docColorStyle,
  formatDate,
} from "./citation-parts";

const OPEN_DELAY_MS = 160;
const CLOSE_DELAY_MS = 220;

const CHIP =
  "mx-px inline-flex h-[1.25em] min-w-[1.25em] -translate-y-[0.3em] items-center justify-center rounded-[5px] px-1 align-baseline text-[0.68em] font-semibold leading-none tabular-nums";

export function CitationMarker({ n, children }: { n: number; children: React.ReactNode }) {
  const message = useMessageCitations();
  if (!message || !citationAt(message.citations, n)) {
    return <span className={cn(CHIP, "bg-primary/12 text-primary")}>{children}</span>;
  }
  return <InteractiveMarker n={n} message={message} />;
}

function InteractiveMarker({ n, message }: { n: number; message: MessageCitations }) {
  const panel = useSourcePanel();
  const citation = citationAt(message.citations, n)!;
  const color = message.groups.colorOf[citation.node_id];
  // 同じ箇所を指す別番号（同じチャンクが 2 回返った場合）も、開いている間は強調する。
  const active =
    panel?.state?.key === message.key && !!passageOf(message, panel.state.n)?.ns.includes(n);

  const [open, setOpen] = React.useState(false);
  const timer = React.useRef<number | null>(null);
  const pointerType = React.useRef<string>("mouse");
  const anchorRef = React.useRef<HTMLButtonElement>(null);

  const clear = () => {
    if (timer.current != null) window.clearTimeout(timer.current);
    timer.current = null;
  };
  const schedule = (next: boolean, ms: number) => {
    clear();
    timer.current = window.setTimeout(() => setOpen(next), ms);
  };
  React.useEffect(() => clear, []);

  const openPanel = () => {
    clear();
    setOpen(false);
    panel?.open(message, n);
  };

  // Trigger ではなく Anchor にする: Trigger は aria-haspopup / aria-expanded を付けるが、
  // マウスとキーボードではこのボタンはカードではなく出典パネルを開くため、読み上げが食い違う。
  return (
    <PopoverPrimitive.Root open={open} onOpenChange={setOpen}>
      <PopoverPrimitive.Anchor asChild>
        <button
          ref={anchorRef}
          type="button"
          style={docColorStyle(color)}
          aria-label={`出典 ${n}: ${message.nameOf(citation.node_id)}`}
          onPointerDown={(e) => {
            pointerType.current = e.pointerType;
          }}
          onPointerEnter={(e) => {
            if (e.pointerType === "mouse") schedule(true, OPEN_DELAY_MS);
          }}
          onPointerLeave={(e) => {
            if (e.pointerType === "mouse") schedule(false, CLOSE_DELAY_MS);
          }}
          onClick={(e) => {
            // タッチはカードを出す（ホバーが無い）。マウス / キーボードはパネルへ直行する。
            const touch = pointerType.current !== "mouse" && e.detail !== 0;
            pointerType.current = "mouse";
            if (touch || !panel) setOpen((v) => !v);
            else openPanel();
          }}
          className={cn(
            CHIP,
            "cursor-pointer transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            active
              ? "bg-[var(--doc)] text-background"
              : "bg-[var(--doc)]/15 text-[var(--doc)] hover:bg-[var(--doc)]/25",
          )}
        >
          {n}
        </button>
      </PopoverPrimitive.Anchor>
      <PopoverPrimitive.Portal>
        <PopoverPrimitive.Content
          side="bottom"
          align="center"
          sideOffset={6}
          collisionPadding={12}
          onOpenAutoFocus={(e) => e.preventDefault()}
          // チップ自身のタップは「外側」扱いにしない（閉じた直後に onClick で開き直すのを防ぐ）。
          onInteractOutside={(e) => {
            if (anchorRef.current?.contains(e.target as Node)) e.preventDefault();
          }}
          onPointerEnter={clear}
          onPointerLeave={(e) => {
            if (e.pointerType === "mouse") schedule(false, CLOSE_DELAY_MS);
          }}
          data-testid="citation-card"
          className={cn(
            "z-50 w-[min(25rem,calc(100vw-24px))] rounded-xl border border-border bg-popover p-3.5 text-popover-foreground shadow-lg shadow-black/[0.06] outline-none",
            "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-95",
            "data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-95",
          )}
        >
          <CitationCardBody n={n} message={message} onOpenPanel={panel ? openPanel : undefined} />
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}

/// 原文カードの中身（ギャラリーからも静的に描けるよう分けている）。
export function CitationCardBody({
  n,
  message,
  onOpenPanel,
}: {
  n: number;
  message: MessageCitations;
  onOpenPanel?: () => void;
}) {
  const citation = citationAt(message.citations, n)!;
  const meta = message.metas[citation.node_id];
  const folder = useNodeMeta(meta?.parentId)?.name;
  const updated = formatDate(meta?.updatedAt);
  const where = [folder, updated ? `${updated} 更新` : null].filter(Boolean).join(" ・ ");
  const heading = citation.heading_path ?? [];

  return (
    <div>
      <div className="flex min-w-0 items-center gap-2">
        <DocIcon meta={meta} />
        <span className="truncate text-[13.5px] font-medium">{message.nameOf(citation.node_id)}</span>
        <KindBadge name={meta?.name} />
      </div>
      {where ? <div className="mt-0.5 truncate pl-6 text-[11.5px] text-muted-foreground">{where}</div> : null}
      <div className="mt-2.5 rounded-lg bg-muted/50 px-3 py-2.5">
        {heading.length > 0 || citation.page != null ? (
          <div className="mb-1 line-clamp-1 text-[11px] text-muted-foreground">
            {[citation.page != null ? `p.${citation.page}` : null, heading.join(" › ") || null]
              .filter(Boolean)
              .join(" ・ ")}
          </div>
        ) : null}
        <p className="line-clamp-6 whitespace-pre-wrap text-[12.5px] leading-relaxed text-foreground/80">
          <SnippetText text={citation.snippet} claim={message.claims.get(n)} />
        </p>
      </div>
      <div className="mt-2.5 flex items-center gap-1.5">
        {onOpenPanel ? (
          <button
            type="button"
            onClick={onOpenPanel}
            className="inline-flex items-center gap-1 rounded-lg bg-primary px-2.5 py-1.5 text-[12px] font-medium text-primary-foreground transition-opacity hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            <PanelRight className="size-3.5" aria-hidden />
            出典を開く
          </button>
        ) : null}
        <OpenOriginalLink nodeId={citation.node_id} meta={meta} />
      </div>
    </div>
  );
}
