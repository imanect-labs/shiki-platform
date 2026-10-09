"use client";

/// 出典パネル（issue #505・案 D）。
///
/// 本文の番号や一覧の行を押すと開き、「回答の記述」と「原文」を並べて見せる。‹ › で
/// 本文で使われた引用を番号順に送る。広い画面のチャットページでは右カラム、狭い画面と
/// 分割ビューのチャットパネルでは右からのシートで出す。
///
/// 原文は検索結果のチャンク（抜粋）まで。前後の文脈や原本上の位置へのジャンプは、
/// 位置情報を保存するコア側の対応（提案資料の 3 章）が入ってから足す。
import * as React from "react";
import { ChevronLeft, ChevronRight, X } from "lucide-react";
import * as DialogPrimitive from "@radix-ui/react-dialog";

import { cn } from "@/lib/utils";
import { citationAt } from "@/lib/citation";
import { Sheet, SheetContent } from "@/components/ui/sheet";
import { SourceBlocks, hasBlockAnchor } from "./source-blocks";
import {
  panelOrder,
  passageOf,
  useNodeMeta,
  usePanelMessage,
  useSourcePanel,
  type MessageCitations,
} from "./citation-context";
import {
  DocIcon,
  KindBadge,
  NumberBadge,
  OpenOriginalLink,
  SnippetText,
  docColorStyle,
  formatDate,
} from "./citation-parts";

/// 右カラム版（チャットページの広い画面）。
export function SourcePanelAside({ className }: { className?: string }) {
  const panel = useSourcePanel();
  const message = usePanelMessage(panel);
  if (!panel?.state || !message) return null;
  return (
    <aside
      aria-label="出典"
      data-testid="source-panel"
      className={cn("flex h-full min-h-0 w-[26rem] shrink-0 flex-col border-l border-border bg-background", className)}
    >
      <SourcePanelView
        state={{ message, n: panel.state.n }}
        onNavigate={(n) => panel.open(message, n)}
        onClose={panel.close}
      />
    </aside>
  );
}

/// シート版（狭い画面・分割ビュー）。
export function SourcePanelSheet() {
  const panel = useSourcePanel();
  const message = usePanelMessage(panel);
  return (
    <Sheet open={!!panel?.state} onOpenChange={(o) => (!o ? panel?.close() : undefined)}>
      <SheetContent side="right" className="max-w-md [&>button:last-child]:hidden" data-testid="source-panel">
        <DialogPrimitive.Title className="sr-only">出典</DialogPrimitive.Title>
        <DialogPrimitive.Description className="sr-only">回答の記述と、根拠にした原文</DialogPrimitive.Description>
        {panel?.state && message ? (
          <SourcePanelView
            state={{ message, n: panel.state.n }}
            onNavigate={(n) => panel.open(message, n)}
            onClose={panel.close}
          />
        ) : null}
      </SheetContent>
    </Sheet>
  );
}

export function SourcePanelView({
  state,
  onNavigate,
  onClose,
}: {
  state: { message: MessageCitations; n: number };
  onNavigate: (n: number) => void;
  onClose: () => void;
}) {
  const { message, n } = state;
  const citation = citationAt(message.citations, n);
  const passage = passageOf(message, n);
  const order = panelOrder(message);
  const idx = passage ? order.indexOf(passage.n) : -1;
  const meta = citation ? message.metas[citation.node_id] : undefined;
  const folder = useNodeMeta(meta?.parentId)?.name;
  const updated = formatDate(meta?.updatedAt);
  const headingRef = React.useRef<HTMLHeadingElement>(null);

  // 開いた・送ったときに見出しへフォーカスを移す（読み上げとキーボード操作の起点）。
  React.useEffect(() => {
    headingRef.current?.focus({ preventScroll: true });
  }, [n, message.key]);

  if (!citation || !passage) return null;
  const color = message.groups.colorOf[citation.node_id];
  const claim = message.claims.get(passage.n) ?? message.claims.get(n);
  const siblings =
    message.groups.docs.find((d) => d.nodeId === citation.node_id)?.passages.filter((p) => p !== passage) ?? [];
  const prev = idx > 0 ? order[idx - 1] : null;
  const next = idx >= 0 && idx < order.length - 1 ? order[idx + 1] : null;
  const heading = citation.heading_path ?? [];
  const staleVersion =
    typeof citation.version === "number" && typeof meta?.version === "number" && citation.version < meta.version;
  const snippet = (
    <p className="whitespace-pre-wrap">
      <SnippetText text={citation.snippet} claim={claim} />
    </p>
  );

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowLeft" && prev != null) {
      e.preventDefault();
      onNavigate(prev);
    } else if (e.key === "ArrowRight" && next != null) {
      e.preventDefault();
      onNavigate(next);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col" onKeyDown={onKeyDown}>
      <div className="flex items-start gap-2 border-b border-border px-4 py-3">
        <DocIcon meta={meta} className="mt-0.5" />
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-1.5">
            <h2
              ref={headingRef}
              tabIndex={-1}
              className="truncate text-[13.5px] font-medium outline-none"
              title={message.nameOf(citation.node_id)}
            >
              {message.nameOf(citation.node_id)}
            </h2>
            <KindBadge name={meta?.name} />
          </div>
          {folder || updated ? (
            <div className="truncate text-[11.5px] text-muted-foreground">
              {[folder, updated ? `${updated} 更新` : null].filter(Boolean).join(" ・ ")}
            </div>
          ) : null}
        </div>
        {idx >= 0 ? (
          <div className="flex shrink-0 items-center text-[11.5px] tabular-nums text-muted-foreground">
            <NavButton label="前の引用" disabled={prev == null} onClick={() => prev != null && onNavigate(prev)}>
              <ChevronLeft className="size-4" />
            </NavButton>
            <span aria-live="polite">
              {idx + 1} / {order.length}
            </span>
            <NavButton label="次の引用" disabled={next == null} onClick={() => next != null && onNavigate(next)}>
              <ChevronRight className="size-4" />
            </NavButton>
          </div>
        ) : null}
        <NavButton label="閉じる" onClick={onClose}>
          <X className="size-4" />
        </NavButton>
      </div>

      <div className="min-h-0 flex-1 space-y-5 overflow-y-auto px-4 py-4 scrollbar-subtle">
        {claim ? (
          <section>
            <h3 className="mb-1.5 text-[11.5px] font-medium text-muted-foreground">回答の記述</h3>
            <div className="flex items-start gap-2 rounded-lg bg-muted/50 px-3 py-2.5 text-[13px] leading-relaxed text-foreground/85">
              <NumberBadge n={passage.n} colorIndex={color} solid className="mt-[3px]" />
              <span>{claim}</span>
            </div>
          </section>
        ) : null}

        <section>
          <h3 className="mb-1.5 text-[11.5px] font-medium text-muted-foreground">原文</h3>
          {heading.length > 0 || citation.page != null ? (
            <div className="mb-1.5 text-[12px] text-foreground/70">
              {[citation.page != null ? `p.${citation.page}` : null, heading.join(" › ") || null]
                .filter(Boolean)
                .join(" ・ ")}
            </div>
          ) : null}
          {staleVersion ? (
            <p className="mb-2 rounded-md bg-muted/60 px-2.5 py-1.5 text-[12px] text-muted-foreground" data-testid="stale-version">
              引用した時点の版（v{citation.version}）の内容です。このファイルはその後更新されています（最新は v
              {meta?.version}）。
            </p>
          ) : null}
          <div
            style={docColorStyle(color)}
            className="rounded-lg border border-[var(--doc)]/35 bg-card px-3.5 py-3 text-[13.5px] leading-[1.85] text-foreground/85"
          >
            {hasBlockAnchor(citation) ? (
              <SourceBlocks key={`${citation.chunk_id}:${citation.version}`} citation={citation} fallback={snippet} />
            ) : (
              snippet
            )}
          </div>
          {!passage.used ? (
            <p className="mt-2 text-[12px] text-muted-foreground">この箇所は検索で見つかりましたが、回答では引用していません。</p>
          ) : null}
        </section>

        {siblings.length > 0 ? (
          <section>
            <h3 className="mb-1.5 text-[11.5px] font-medium text-muted-foreground">この文書のほかの引用</h3>
            <ul className="space-y-0.5">
              {siblings.map((p) => (
                <li key={p.citation.chunk_id}>
                  <button
                    type="button"
                    onClick={() => onNavigate(p.n)}
                    className="flex w-full items-start gap-2 rounded-lg px-1.5 py-1.5 text-left transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <NumberBadge n={p.n} colorIndex={color} className="mt-[2px]" />
                    <span className="line-clamp-2 text-[12.5px] leading-relaxed text-muted-foreground">
                      {(p.citation.heading_path ?? []).slice(-1)[0] ?? p.citation.snippet.trim()}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </section>
        ) : null}
      </div>

      <div className="flex items-center justify-end gap-2 border-t border-border px-4 py-2.5">
        <OpenOriginalLink nodeId={citation.node_id} meta={meta} variant="outline" />
      </div>
    </div>
  );
}

function NavButton({
  label,
  disabled,
  onClick,
  children,
}: {
  label: string;
  disabled?: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
      className="flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-35"
    >
      {children}
    </button>
  );
}
