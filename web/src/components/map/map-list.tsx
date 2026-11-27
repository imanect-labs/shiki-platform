// 会話・ノートの中の地図の下端に重ねる、地点のカードの列（全画面のパネルは map-panel.tsx）。
// 行・カードに乗せる／キーボードで移ると地図の札とカードが連動し、押すとその地点を選ぶ。
"use client";

import * as React from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";
import type { MapMarker } from "@imanect/yui";

import { cn } from "@/lib/utils";

export type MapListProps = {
  markers: MapMarker[];
  /// 指している・選んでいる地点（強調する）。
  active: number | null;
  selected: number | null;
  ready: boolean;
  onHover: (i: number) => void;
  /// 地点から離れた（ほかの地点を指していれば何もしない）。
  onLeave: (i: number) => void;
  onPick: (i: number) => void;
};

function itemHandlers(i: number, p: MapListProps) {
  return {
    onClick: () => p.onPick(i),
    onMouseEnter: () => p.onHover(i),
    // 行から離れたら指すのをやめる（一覧の空いた所・地図へ出たときも）。
    onMouseLeave: () => p.onLeave(i),
    onFocus: () => p.onHover(i),
    onBlur: () => p.onLeave(i),
    disabled: !p.ready,
    "aria-pressed": p.selected === i,
    "data-index": i,
  };
}

function prefersReducedMotion(): boolean {
  return typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

/// 会話・ノートの中の地図の下端に重ねる、横に流れるカードの列。
/// - 左右はスクロール（トラックパッド・横スクロール・タッチ）と、端の矢印ボタンで送る。
/// - 地図の札を指す・選ぶと、そのカードが見える所まで列だけを送る（ページはスクロールしない）。
/// - キーボードでは列全体が 1 つのタブ位置で、←→・Home・End でカードを移る。
export function MapCardCarousel(p: MapListProps) {
  const ref = React.useRef<HTMLDivElement>(null);
  const [edges, setEdges] = React.useState({ start: true, end: true });
  // 左右の余白（矢印の置き場）の幅。続きがある側は、この幅をぼかす。
  const [gutter, setGutter] = React.useState(12);
  // タブで入ったときに止まるカード（最後に移ったカード）。
  const [focusIndex, setFocusIndex] = React.useState(0);

  const updateEdges = React.useCallback(() => {
    const el = ref.current;
    if (!el) return;
    const start = el.scrollLeft <= 1;
    const end = el.scrollLeft + el.clientWidth >= el.scrollWidth - 1;
    setGutter(parseFloat(getComputedStyle(el).paddingLeft) || 0);
    setEdges((cur) => (cur.start === start && cur.end === end ? cur : { start, end }));
  }, []);

  React.useEffect(() => {
    updateEdges();
    const el = ref.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(updateEdges);
    ro.observe(el);
    return () => ro.disconnect();
  }, [updateEdges, p.markers]);

  const behavior: ScrollBehavior = prefersReducedMotion() ? "auto" : "smooth";

  // 指している・選んでいる地点のカードを見える所へ（列の中だけを動かす）。
  React.useEffect(() => {
    const box = ref.current;
    const el = p.active === null ? null : box?.querySelector<HTMLElement>(`[data-index="${p.active}"]`);
    if (!box || !el) return;
    // 左右の余白（矢印の置き場）の内側に収める。止める位置はカードの左端に揃える
    // （揃えないとスクロールの吸着で戻され、カードが端で切れたままになる）。
    const edge = parseFloat(getComputedStyle(box).paddingLeft) || 0;
    const starts = [...box.querySelectorAll<HTMLElement>("[data-index]")].map((c) => c.offsetLeft - edge);
    const left = el.offsetLeft - edge;
    if (left < box.scrollLeft) {
      box.scrollTo({ left, behavior });
    } else if (el.offsetLeft + el.offsetWidth + edge > box.scrollLeft + box.clientWidth) {
      const need = el.offsetLeft + el.offsetWidth + edge - box.clientWidth;
      box.scrollTo({ left: starts.find((x) => x >= need) ?? left, behavior });
    }
  }, [p.active, behavior]);

  const page = (dir: 1 | -1) => {
    const box = ref.current;
    if (!box) return;
    box.scrollBy({ left: dir * Math.max(box.clientWidth - 96, 160), behavior });
  };

  const moveFocus = (to: number) => {
    const n = Math.min(Math.max(to, 0), p.markers.length - 1);
    ref.current?.querySelector<HTMLButtonElement>(`[data-index="${n}"]`)?.focus({ preventScroll: true });
  };
  const onKeyDown = (e: React.KeyboardEvent) => {
    const keys: Record<string, number> = {
      ArrowRight: focusIndex + 1,
      ArrowLeft: focusIndex - 1,
      Home: 0,
      End: p.markers.length - 1,
    };
    const to = keys[e.key];
    if (to === undefined) return;
    e.preventDefault();
    moveFocus(to);
  };

  // 端が切れているときだけ、その側の余白をぼかす（続きがあることを示す。矢印の横にカードの切れ端を見せない）。
  const fade = Math.max(gutter, 12);
  const mask = `linear-gradient(to right, ${edges.start ? "#000 0" : `transparent 0, #000 ${fade}px`}, ${
    edges.end ? "#000 100%" : `#000 calc(100% - ${fade}px), transparent 100%`
  })`;

  return (
    <div className="relative">
      <div
        ref={ref}
        role="list"
        aria-label="地点の一覧"
        data-testid="basemap-legend"
        onScroll={updateEdges}
        onKeyDown={onKeyDown}
        style={{ maskImage: mask, WebkitMaskImage: mask }}
        className={cn(
          // 左右の余白は矢印の置き場（矢印がカードの文字に重ならない）。小さい画面は矢印を出さないので狭くする。
          "relative flex snap-x snap-proximity gap-2 overflow-x-auto overscroll-x-contain px-3 py-1 scroll-px-3 sm:px-11 sm:scroll-px-11",
          "[scrollbar-width:none] [&::-webkit-scrollbar]:hidden",
        )}
      >
        {p.markers.map((mk, i) => {
          const on = p.active === i;
          return (
            <div role="listitem" key={`${i}-${mk.label}`} className="flex snap-start">
              <button
                type="button"
                {...itemHandlers(i, p)}
                onFocus={() => {
                  setFocusIndex(i);
                  p.onHover(i);
                }}
                tabIndex={i === focusIndex ? 0 : -1}
                className={cn(
                  "flex w-[min(248px,70vw)] min-w-0 flex-col justify-center rounded-2xl px-4 py-2.5 text-left",
                  "shadow-[0_2px_12px_rgba(0,0,0,0.12)] ring-1 backdrop-blur-md transition-[background-color,color,box-shadow,transform] duration-150",
                  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                  on
                    ? "bg-foreground text-background ring-foreground"
                    : "bg-background/95 text-foreground ring-black/5 hover:-translate-y-0.5 hover:shadow-[0_6px_18px_rgba(0,0,0,0.16)] dark:ring-white/10",
                )}
              >
                <span className="w-full truncate text-[14px] font-semibold leading-snug tracking-tight">{mk.label}</span>
                {mk.desc ? (
                  <span
                    className={cn(
                      "mt-0.5 line-clamp-2 w-full text-[12px] leading-snug",
                      on ? "text-background/75" : "text-muted-foreground",
                    )}
                  >
                    {mk.desc}
                  </span>
                ) : null}
              </button>
            </div>
          );
        })}
      </div>
      {/* 端の矢印（マウスで送る。タッチ・キーボードではスクロール・←→ で送れるので小さい画面では出さない）。 */}
      {edges.start ? null : <CarouselArrow side="left" onClick={() => page(-1)} />}
      {edges.end ? null : <CarouselArrow side="right" onClick={() => page(1)} />}
    </div>
  );
}

function CarouselArrow({ side, onClick }: { side: "left" | "right"; onClick: () => void }) {
  const Icon = side === "left" ? ChevronLeft : ChevronRight;
  return (
    <button
      type="button"
      tabIndex={-1}
      aria-label={side === "left" ? "前の地点へ" : "次の地点へ"}
      onClick={onClick}
      className={cn(
        "absolute top-1/2 z-10 hidden size-8 -translate-y-1/2 items-center justify-center rounded-full sm:flex",
        "bg-background text-foreground/80 shadow-[0_2px_10px_rgba(0,0,0,0.18)] ring-1 ring-black/5 transition-colors hover:text-foreground dark:ring-white/10",
        side === "left" ? "left-1.5" : "right-1.5",
        // 余白は 44px、矢印は 32px なので余白の中に収まる。
      )}
    >
      <Icon className="size-4" />
    </button>
  );
}
