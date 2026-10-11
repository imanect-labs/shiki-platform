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
import { PdfCitePreview } from "./pdf-cite-preview";
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
  // PDF の引用の見せ方。引用を送ったら本文に戻す。
  const [view, setView] = React.useState<"text" | "page">("text");
  const [pdfFailed, setPdfFailed] = React.useState(false);
  // 別の引用へ移ったら（同じ箇所を指す別番号でも）表示の状態を戻す。
  const viewKey = `${message.key}:${citation?.chunk_id}:${n}`;
  React.useEffect(() => {
    setView("text");
    setPdfFailed(false);
  }, [viewKey]);

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

  // PDF が読めない（取得・解析に失敗）ときは、ページ表示を出さず本文だけにする。
  const isPdf = (citation.boxes?.length ?? 0) > 0 && typeof citation.version === "number" && !pdfFailed;
  const where = [citation.page != null ? `p.${citation.page}` : null, ...heading].filter(Boolean).join(" › ");

  return (
    <div className="flex h-full min-h-0 flex-col" onKeyDown={onKeyDown}>
      <div className="flex items-center gap-2.5 px-4 py-3 shiki-dash-bottom">
        <DocIcon meta={meta} className="shrink-0" />
        <div className="min-w-0 flex-1">
          <h2
            ref={headingRef}
            tabIndex={-1}
            className="truncate text-[13.5px] font-medium leading-snug outline-none"
            title={message.nameOf(citation.node_id)}
          >
            {message.nameOf(citation.node_id)}
          </h2>
          {folder || updated ? (
            <div className="truncate text-[11.5px] leading-snug text-muted-foreground">
              {[folder, updated ? `${updated} 更新` : null].filter(Boolean).join(" · ")}
            </div>
          ) : null}
        </div>
        {idx >= 0 && order.length > 1 ? (
          <div className="flex shrink-0 items-center text-[11.5px] tabular-nums text-muted-foreground">
            <NavButton label="前の引用" disabled={prev == null} onClick={() => prev != null && onNavigate(prev)}>
              <ChevronLeft className="size-4" />
            </NavButton>
            <span aria-live="polite" className="min-w-[2.5rem] text-center">
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

      {/* 本文は固定の段組み: 引用した文 → 場所 → 原文（残りの高さいっぱい・中だけスクロール）。
          原文の枠がパネルの外へ押し出されないよう、パネル全体はスクロールさせない。 */}
      <div className="flex min-h-0 flex-1 flex-col gap-3 px-4 pb-3 pt-4" style={docColorStyle(color)}>
        {claim ? (
          <blockquote
            aria-label="回答の記述"
            className="flex shrink-0 gap-2 border-l-2 border-[var(--doc)]/45 pl-3 text-[12.5px] leading-relaxed text-muted-foreground"
          >
            <NumberBadge n={passage.n} colorIndex={color} className="mt-[3px]" />
            <span className="line-clamp-2" title={claim}>
              {claim}
            </span>
          </blockquote>
        ) : null}

        <div className="flex shrink-0 items-center gap-2">
          <span className="min-w-0 truncate text-[11.5px] text-muted-foreground" title={where || undefined}>
            {where || "原文"}
          </span>
          {/* 版の注記は省略しない（場所の文字列が長くても、古い版だと必ず分かるように）。 */}
          {staleVersion ? (
            <span className="shrink-0 text-[11.5px] text-muted-foreground" data-testid="stale-version">
              · 引用時点 v{citation.version}（最新 v{meta?.version}）
            </span>
          ) : null}
          <span className="flex-1" />
          {isPdf ? <ViewToggle value={view} onChange={setView} /> : null}
        </div>


        <div className="min-h-0 flex-1 text-[13.5px] leading-[1.85] text-foreground/85">
          {isPdf && view === "page" ? (
            <PdfCitePreview
              key={citation.chunk_id}
              citation={citation}
              zoom="fit"
              className="h-full"
              onUnavailable={() => {
                setPdfFailed(true);
                setView("text");
              }}
            />
          ) : hasBlockAnchor(citation) ? (
            <SourceBlocks
              key={`${citation.chunk_id}:${citation.version}`}
              citation={citation}
              fallback={<div className="h-full overflow-y-auto scrollbar-subtle">{snippet}</div>}
              className="h-full max-h-none pr-1"
            />
          ) : (
            <div className="h-full overflow-y-auto pr-1 scrollbar-subtle">{snippet}</div>
          )}
        </div>

        {!passage.used ? (
          <p className="shrink-0 text-[11.5px] text-muted-foreground">検索で見つかった箇所です（回答では引用していません）。</p>
        ) : null}

      </div>

      {/* 下部は 1 本の帯: 同じ文書の他の箇所（左）と原本を開く（右）。 */}
      <div className="flex items-center gap-3 px-4 py-2.5 shiki-dash-top">
          {siblings.length > 0 ? (
            <div className="flex min-w-0 flex-1 items-center gap-1.5 overflow-x-auto scrollbar-subtle">
              <span className="shrink-0 text-[11.5px] text-muted-foreground">同じ文書</span>
              {siblings.map((p) => (
                <button
                  key={p.citation.chunk_id}
                  type="button"
                  onClick={() => onNavigate(p.n)}
                  title={(p.citation.heading_path ?? []).join(" › ") || p.citation.snippet.trim()}
                  className="inline-flex max-w-[12rem] shrink-0 items-center gap-1.5 rounded-full border border-border py-0.5 pl-1 pr-2.5 text-[12px] text-foreground/75 transition-colors hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <NumberBadge n={p.n} colorIndex={color} />
                  <span className="truncate">{(p.citation.heading_path ?? []).slice(-1)[0] ?? p.citation.snippet.trim()}</span>
                </button>
              ))}
            </div>
          ) : null}
        {siblings.length === 0 ? <span className="flex-1" /> : null}
        <OpenOriginalLink nodeId={citation.node_id} meta={meta} citation={citation} variant="outline" />
      </div>
    </div>
  );
}

/// PDF の引用で、原文の見せ方（ブロックの本文 / ページの画像と枠）を切り替える。
function ViewToggle({ value, onChange }: { value: "text" | "page"; onChange: (v: "text" | "page") => void }) {
  return (
    <div role="tablist" aria-label="原文の表示" className="flex shrink-0 rounded-lg bg-muted/70 p-0.5 text-[11.5px]">
      {(
        [
          ["text", "本文"],
          ["page", "ページ"],
        ] as const
      ).map(([v, label]) => (
        <button
          key={v}
          type="button"
          role="tab"
          aria-selected={value === v}
          onClick={() => onChange(v)}
          className={cn(
            "rounded-md px-2.5 py-0.5 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            value === v ? "bg-accent text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground",
          )}
        >
          {label}
        </button>
      ))}
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
