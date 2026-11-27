// 全画面の地図に浮かべるパネル（広い画面は左に角丸で浮かせ、狭い画面は下から出す）。
// - 一覧: 図の題名・地点の数・絞り込み・地点の行。行に乗せる／移ると地図の札とカードが連動し、押すとその地点の詳しい表示へ。
// - 詳しい表示: 名前・説明・位置（座標のコピー）・前後の地点への移動・その地点へ寄る。
// 名前・説明は回答やノートの文字なので、React の文字として入れる（HTML として解釈させない）。
"use client";

import * as React from "react";
import { ArrowLeft, Check, ChevronLeft, ChevronRight, Copy, LocateFixed, MapPin, PanelLeftClose, Search, X } from "lucide-react";

import { cn } from "@/lib/utils";
import type { MapListProps } from "./map-list";

export type MapPanelProps = MapListProps & {
  title?: string | undefined;
  /// 選んだ地点を外す（一覧へ戻る）。
  onClear: () => void;
  /// その地点へ寄る（拡大して真ん中に）。
  onZoomTo: (i: number) => void;
  /// パネルを畳む。
  onCollapse: () => void;
};

/// これより地点が多いときだけ絞り込みの欄を出す。
const FILTER_MIN = 6;

export const MapPanel = React.forwardRef<HTMLElement, MapPanelProps>(function MapPanel(p, ref) {
  const detail = p.selected !== null && p.markers[p.selected] ? p.selected : null;
  return (
    <aside
      ref={ref}
      aria-label="地点の一覧"
      data-testid="basemap-panel"
      data-view={detail === null ? "list" : "detail"}
      className={cn(
        "absolute z-30 flex flex-col overflow-hidden rounded-3xl bg-background/95 text-foreground",
        "shadow-[0_10px_40px_rgba(0,0,0,0.18),0_2px_8px_rgba(0,0,0,0.08)] ring-1 ring-black/5 backdrop-blur-xl dark:ring-white/10",
        // 狭い画面は下から出す。広い画面は左に浮かせ、中身の分だけの高さにする（長ければ一覧だけをスクロール）。
        "inset-x-3 bottom-3 max-h-[48%] sm:inset-x-auto sm:bottom-auto sm:left-4 sm:top-4 sm:max-h-[calc(100%-2rem)] sm:w-[360px]",
      )}
    >
      {detail === null ? <PanelList {...p} /> : <PanelDetail {...p} index={detail} />}
    </aside>
  );
});

function IconButton({ label, className, children, ...props }: React.ComponentProps<"button"> & { label: string }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={cn(
        "flex size-8 shrink-0 items-center justify-center rounded-full text-foreground/70 transition-colors hover:bg-accent hover:text-foreground",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-35",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  );
}

function PanelList(p: MapPanelProps) {
  const [query, setQuery] = React.useState("");
  const listRef = React.useRef<HTMLDivElement>(null);
  const q = query.trim().toLowerCase();
  const shown = p.markers
    .map((mk, i) => ({ mk, i }))
    .filter(({ mk }) => !q || mk.label.toLowerCase().includes(q) || (mk.desc ?? "").toLowerCase().includes(q));

  // 地図の札を指したとき、その行が一覧の見える所に来るようにする（一覧の中だけを動かす）。
  React.useEffect(() => {
    const box = listRef.current;
    const el = p.active === null ? null : box?.querySelector<HTMLElement>(`[data-index="${p.active}"]`);
    if (!box || !el) return;
    const top = el.offsetTop - 8;
    const bottom = el.offsetTop + el.offsetHeight + 8;
    if (top < box.scrollTop) box.scrollTo({ top });
    else if (bottom > box.scrollTop + box.clientHeight) box.scrollTo({ top: bottom - box.clientHeight });
  }, [p.active]);

  return (
    <>
      <div className="flex items-start gap-2 px-5 pb-3 pt-4">
        <div className="min-w-0 flex-1">
          {p.title ? <div className="line-clamp-2 text-[16px] font-semibold leading-snug tracking-tight">{p.title}</div> : null}
          <div className={cn("text-[12.5px] text-muted-foreground", p.title ? "mt-0.5" : "text-[15px] font-semibold text-foreground")}>
            {q ? `${shown.length} / ${p.markers.length} か所` : `${p.markers.length} か所`}
          </div>
        </div>
        <IconButton label="一覧を隠す" onClick={p.onCollapse} className="-mr-2 -mt-0.5">
          <PanelLeftClose className="size-4" />
        </IconButton>
      </div>
      {p.markers.length >= FILTER_MIN ? (
        <div className="px-4 pb-2">
          <label className="flex h-10 items-center gap-2 rounded-2xl bg-secondary/70 px-3 ring-1 ring-transparent transition focus-within:bg-background focus-within:ring-ring/40">
            <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="名前・説明で絞り込む"
              aria-label="地点を絞り込む"
              // 枠（label）が自分でフォーカスを示すので、欄そのものの輪は出さない。
              className="min-w-0 flex-1 bg-transparent text-[13.5px] outline-none placeholder:text-muted-foreground focus-visible:ring-0 focus-visible:ring-offset-0"
            />
            {query ? (
              <IconButton label="絞り込みを消す" onClick={() => setQuery("")} className="-mr-1.5 size-6">
                <X className="size-3.5" />
              </IconButton>
            ) : null}
          </label>
        </div>
      ) : null}
      <div
        ref={listRef}
        role="list"
        aria-label="地点"
        data-testid="basemap-legend"
        className="relative min-h-0 flex-1 overflow-y-auto overscroll-contain px-2 pb-2"
      >
        {shown.map(({ mk, i }) => {
          const on = p.active === i;
          return (
            <div role="listitem" key={`${i}-${mk.label}`}>
              <button
                type="button"
                onClick={() => p.onPick(i)}
                onMouseEnter={() => p.onHover(i)}
                onMouseLeave={() => p.onLeave(i)}
                onFocus={() => p.onHover(i)}
                onBlur={() => p.onLeave(i)}
                disabled={!p.ready}
                data-index={i}
                className={cn(
                  "group flex w-full min-w-0 items-start gap-3 rounded-2xl px-3 py-2.5 text-left transition-colors",
                  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
                  on ? "bg-accent" : "hover:bg-accent/60",
                )}
              >
                <span
                  aria-hidden="true"
                  className={cn(
                    "mt-0.5 flex size-9 shrink-0 items-center justify-center rounded-xl transition-colors",
                    on ? "bg-foreground text-background" : "bg-[#e5484d]/10 text-[#e5484d] dark:bg-[#e5484d]/20",
                  )}
                >
                  <MapPin className="size-[18px]" />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[14px] font-semibold leading-snug">{mk.label}</span>
                  {mk.desc ? (
                    <span className="mt-0.5 line-clamp-2 block text-[12.5px] leading-snug text-muted-foreground">{mk.desc}</span>
                  ) : null}
                </span>
                <ChevronRight
                  aria-hidden="true"
                  className={cn(
                    "mt-2.5 size-4 shrink-0 transition-colors",
                    on ? "text-foreground" : "text-muted-foreground/40 group-hover:text-muted-foreground",
                  )}
                />
              </button>
            </div>
          );
        })}
        {shown.length === 0 ? (
          <div className="px-3 py-8 text-center text-[13px] text-muted-foreground">該当する地点はありません</div>
        ) : null}
      </div>
    </>
  );
}

function PanelDetail(p: MapPanelProps & { index: number }) {
  const mk = p.markers[p.index];
  const [copied, setCopied] = React.useState(false);
  React.useEffect(() => setCopied(false), [p.index]);
  if (!mk) return null;
  const n = p.markers.length;
  const coords = `${mk.lat.toFixed(5)}, ${mk.lng.toFixed(5)}`;
  const copy = () => {
    void navigator.clipboard?.writeText(coords).then(
      () => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      },
      () => {
        /* 書き込めない環境（権限なし）では何もしない */
      },
    );
  };
  return (
    <div className="flex min-h-0 flex-col overflow-y-auto overscroll-contain">
      <div className="flex items-center gap-1 px-3 pt-3">
        <button
          type="button"
          onClick={p.onClear}
          className="flex h-8 items-center gap-1.5 rounded-full pl-2 pr-3 text-[13px] font-medium text-foreground/80 transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <ArrowLeft className="size-4" />
          一覧
        </button>
        <div className="ml-auto flex items-center gap-0.5">
          <span className="mr-1 text-[12px] tabular-nums text-muted-foreground" aria-live="polite">
            {p.index + 1} / {n}
          </span>
          <IconButton label="前の地点" onClick={() => p.onPick(p.index - 1)} disabled={p.index === 0}>
            <ChevronLeft className="size-4" />
          </IconButton>
          <IconButton label="次の地点" onClick={() => p.onPick(p.index + 1)} disabled={p.index === n - 1}>
            <ChevronRight className="size-4" />
          </IconButton>
        </div>
      </div>
      <div className="px-5 pb-5 pt-3">
        <div className="flex items-start gap-3">
          <span
            aria-hidden="true"
            className="flex size-11 shrink-0 items-center justify-center rounded-2xl bg-[#e5484d] text-white shadow-[0_4px_12px_rgba(229,72,77,0.35)]"
          >
            <MapPin className="size-5" />
          </span>
          <h3 className="min-w-0 flex-1 pt-1 text-[20px] font-bold leading-tight tracking-tight" data-testid="basemap-panel-title">
            {mk.label}
          </h3>
        </div>
        {mk.desc ? <p className="mt-3 text-[14px] leading-relaxed text-foreground/85">{mk.desc}</p> : null}
        <div className="mt-4 flex items-center gap-3 rounded-2xl bg-secondary/60 px-4 py-3">
          <div className="min-w-0 flex-1">
            <div className="text-[11.5px] font-medium text-muted-foreground">位置（緯度, 経度）</div>
            <div className="mt-0.5 truncate font-mono text-[13px] tabular-nums">{coords}</div>
          </div>
          <IconButton label={copied ? "コピーしました" : "座標をコピー"} onClick={copy} className="bg-background shadow-sm">
            {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
          </IconButton>
        </div>
        <button
          type="button"
          onClick={() => p.onZoomTo(p.index)}
          className="mt-4 flex h-10 w-full items-center justify-center gap-2 rounded-full bg-foreground text-[13.5px] font-semibold text-background transition-opacity hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
        >
          <LocateFixed className="size-4" />
          この地点へ寄る
        </button>
      </div>
    </div>
  );
}
