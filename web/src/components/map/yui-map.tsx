// 回答の ```ui の map を、背景の地図（MapLibre）つきで描く（@imanect/yui/react の renderMap）。
//
// Yui はデータ（地点・道順・縮尺）と静止画（位置関係だけ）を持ち、背景の地図はホスト（ここ）が描く。
// 地図が無効な構成（オンプレ・エアギャップ）や読み込みに失敗したときは、Yui の静止画に戻す。
// 見た目は ChatGPT の地図に寄せず、番号のピン・地点のカード・全画面の 3 つに絞る。
"use client";

import * as React from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { ChevronLeft, ChevronRight, ExternalLink, LocateFixed, Maximize2, Minus, Navigation, Plus, X } from "lucide-react";
import { useTheme } from "next-themes";
import type { MapFigure } from "@imanect/yui";

import { fetchMapConfig, type MapConfig } from "@/lib/map-api";
import { cn } from "@/lib/utils";
import { useNearViewport, useYuiMap, type MapPadding } from "./use-yui-map";

const MODE_LABEL = { walk: "徒歩", drive: "車", transit: "電車" } as const;
const TRAVEL_MODE = { walk: "walking", drive: "driving", transit: "transit" } as const;

/// 地点を外部の地図アプリで開く URL（緯度・経度だけから作る。モデルが書いた文字列を URL に入れない）。
function placeUrl(lat: number, lng: number): string {
  return `https://www.google.com/maps/search/?api=1&query=${lat},${lng}`;
}

function directionsUrl(figure: MapFigure): string | null {
  const at = new Map(figure.markers.map((m) => [m.label, `${m.lat},${m.lng}`]));
  const stops = (figure.route?.stops ?? []).map((s) => at.get(s)).filter((s): s is string => !!s);
  if (stops.length < 2) return null;
  const q = new URLSearchParams({ api: "1", origin: stops[0]!, destination: stops[stops.length - 1]! });
  if (stops.length > 2) q.set("waypoints", stops.slice(1, -1).join("|"));
  if (figure.route?.mode) q.set("travelmode", TRAVEL_MODE[figure.route.mode]);
  return `https://www.google.com/maps/dir/?${q.toString()}`;
}

let resolvedConfig: MapConfig | null = null;

/// 地図の設定を読む。2 つ目以降の地図は読み込み済みの値で最初から描く（静止画からの切り替わりを出さない）。
function useMapConfig(): MapConfig | null {
  const [config, setConfig] = React.useState<MapConfig | null>(resolvedConfig);
  React.useEffect(() => {
    if (config) return;
    let alive = true;
    void fetchMapConfig().then((c) => {
      if (c.style_url_light) resolvedConfig = c;
      if (alive) setConfig(c);
    });
    return () => {
      alive = false;
    };
  }, [config]);
  return config;
}

export function YuiMap({ figure, fallback }: { figure: MapFigure; fallback: React.ReactNode }) {
  const config = useMapConfig();
  const [failed, setFailed] = React.useState(false);
  if (config && (!config.style_url_light || failed)) {
    return (
      <div data-testid="yui-map-static">
        {fallback}
        {failed ? (
          <p className="-mt-1 mb-3 text-[11px] text-muted-foreground">地図を読み込めなかったため、位置関係だけを表示しています。</p>
        ) : null}
      </div>
    );
  }
  return <MapCard figure={figure} config={config} onFail={() => setFailed(true)} />;
}

function MapCard({ figure, config, onFail }: { figure: MapFigure; config: MapConfig | null; onFail: () => void }) {
  const [selected, setSelected] = React.useState<number | null>(null);
  const [expanded, setExpanded] = React.useState(false);
  const select = React.useCallback((i: number) => setSelected(i < 0 ? null : i), []);
  const directions = directionsUrl(figure);
  const title = figure.title.trim();

  return (
    <figure className="my-3" data-testid="yui-map-card" aria-label={title || "地図"}>
      {title ? <figcaption className="mb-2.5 text-[14px] font-semibold leading-5 text-foreground">{title}</figcaption> : null}
      <MapSurface
        figure={figure}
        config={config}
        selected={selected}
        onSelect={select}
        cooperative
        onFail={onFail}
        className="h-[260px] rounded-[14px] border border-border/60 sm:h-[340px]"
        onExpand={() => setExpanded(true)}
      />
      {figure.route ? (
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[12px] text-muted-foreground">
          <span>
            {figure.route.mode ? `${MODE_LABEL[figure.route.mode]}の道順。` : "道順。"}
            線は地点を順に結んだもので、実際の道筋ではありません。
          </span>
          {directions ? (
            <a
              href={directions}
              target="_blank"
              rel="noopener noreferrer"
              // スマホでも押しやすいよう、見た目を変えずに押せる範囲を上下に広げる。
              className="-mx-1 inline-flex items-center gap-1 rounded px-1 py-0.5 font-medium text-foreground underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-coarse:-my-2 pointer-coarse:py-2.5"
            >
              <Navigation className="size-3" aria-hidden />
              Google マップで経路を見る
            </a>
          ) : null}
        </div>
      ) : null}
      <PlaceList figure={figure} selected={selected} onSelect={select} className="mt-2.5" />
      {figure.note ? <p className="mt-2 text-[12px] text-muted-foreground">{figure.note}</p> : null}
      <DialogPrimitive.Root open={expanded} onOpenChange={setExpanded}>
        <DialogPrimitive.Portal>
          <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/40 data-[state=open]:animate-in data-[state=open]:fade-in-0" />
          <DialogPrimitive.Content
            className="fixed inset-0 z-50 flex flex-col bg-background outline-none data-[state=open]:animate-in data-[state=open]:fade-in-0 sm:inset-4 sm:overflow-hidden sm:rounded-2xl sm:border sm:border-border sm:shadow-[var(--elevation-lg)]"
            aria-describedby={undefined}
          >
            <div className="shiki-dash-bottom flex h-12 shrink-0 items-center gap-2 px-4 pt-[env(safe-area-inset-top)]">
              <DialogPrimitive.Title className="min-w-0 flex-1 truncate text-[15px] font-semibold">{title || "地図"}</DialogPrimitive.Title>
              <DialogPrimitive.Close
                aria-label="閉じる"
                className="flex size-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <X className="size-4" aria-hidden />
              </DialogPrimitive.Close>
            </div>
            <div className="relative min-h-0 flex-1">
              {expanded ? (
                <MapSurface
                  figure={figure}
                  config={config}
                  selected={selected}
                  onSelect={select}
                  cooperative={false}
                  onFail={onFail}
                  padding={{ bottom: 168 }}
                  fullscreen
                  className="absolute inset-0"
                />
              ) : null}
              <div className="pointer-events-none absolute inset-x-0 bottom-0 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
                <PlaceList figure={figure} selected={selected} onSelect={select} floating className="pointer-events-auto" />
              </div>
            </div>
          </DialogPrimitive.Content>
        </DialogPrimitive.Portal>
      </DialogPrimitive.Root>
    </figure>
  );
}

function MapSurface({
  figure,
  config,
  selected,
  onSelect,
  cooperative,
  onFail,
  onExpand,
  padding,
  fullscreen,
  className,
}: {
  figure: MapFigure;
  config: MapConfig | null;
  selected: number | null;
  onSelect: (i: number) => void;
  cooperative: boolean;
  onFail: () => void;
  onExpand?: () => void;
  padding?: MapPadding;
  /// 全画面（下に地点のカードが重なるので、操作ボタンと出典を上に置く）。
  fullscreen?: boolean;
  className?: string;
}) {
  const box = React.useRef<HTMLDivElement>(null);
  const canvas = React.useRef<HTMLDivElement>(null);
  const near = useNearViewport(box);
  const { resolvedTheme } = useTheme();
  const mode = resolvedTheme === "dark" ? "dark" : "light";
  const { status, zoom, recenter } = useYuiMap({
    container: canvas,
    figure,
    config: config ?? { style_url_light: null, style_url_dark: null },
    mode,
    active: near && !!config?.style_url_light,
    cooperative,
    selected,
    onSelect,
    padding,
    attribution: fullscreen ? "top-left" : "bottom-left",
  });
  React.useEffect(() => {
    if (status === "error") onFail();
  }, [status, onFail]);

  return (
    <div
      ref={box}
      data-testid="yui-map-surface"
      data-map-status={status}
      className={cn("relative isolate overflow-hidden bg-[var(--map-land)]", "[--map-land:#f3f3f1] dark:[--map-land:#1c1c20]", className)}
    >
      {/* MapLibre は地図の要素に position: relative を当てるので、位置は外側の要素で決める。 */}
      <div className="absolute inset-0">
        <div
          ref={canvas}
          className={cn(
            "size-full",
            // 出典（畳んだ「i」と開いた帯）を Shiki の小さな面にそろえる。表示は提供元の規約上消さない。
            "[&_.maplibregl-ctrl-attrib]:!rounded-full [&_.maplibregl-ctrl-attrib]:!bg-card/85 [&_.maplibregl-ctrl-attrib]:text-[10px] [&_.maplibregl-ctrl-attrib]:text-muted-foreground [&_.maplibregl-ctrl-attrib]:backdrop-blur",
            "[&_.maplibregl-ctrl-attrib_a]:!text-muted-foreground [&_.maplibregl-ctrl-attrib-button]:opacity-60 dark:[&_.maplibregl-ctrl-attrib-button]:invert",
          )}
        />
      </div>
      {status !== "ready" ? (
        // 読み込み中: 地の色をゆっくり明滅させ、中央に小さく文字を流す（白い箱や回転するアイコンは出さない）。
        <div aria-hidden className="pointer-events-none absolute inset-0 flex items-center justify-center">
          <div className="absolute inset-0 animate-pulse bg-muted/60" />
          <span className="shiki-text-shimmer relative text-[12px] font-medium">地図を読み込んでいます</span>
        </div>
      ) : null}
      <span className="sr-only" aria-live="polite">
        {status === "loading" ? "地図を読み込んでいます" : ""}
      </span>
      {onExpand ? (
        <div className="absolute right-2.5 top-2.5 z-10">
          <MapButton label="全画面で見る" onClick={onExpand} testId="yui-map-expand" framed>
            <Maximize2 className="size-4" aria-hidden />
          </MapButton>
        </div>
      ) : null}
      {/* 拡大・縮小はマウスのときだけ（タッチは 2 本の指で動かす）。全画面では下にカードが重なるので上に置く。 */}
      <div
        className={cn(
          "absolute right-2.5 z-10 flex flex-col overflow-hidden rounded-lg border border-border/60 bg-card/90 shadow-[var(--elevation-sm)] backdrop-blur",
          fullscreen ? "top-2.5" : "bottom-2.5",
        )}
      >
        <MapButton label="拡大" onClick={() => zoom(1)} disabled={status !== "ready"} className="pointer-coarse:hidden">
          <Plus className="size-4" aria-hidden />
        </MapButton>
        <div className="h-px bg-border/60 pointer-coarse:hidden" />
        <MapButton label="縮小" onClick={() => zoom(-1)} disabled={status !== "ready"} className="pointer-coarse:hidden">
          <Minus className="size-4" aria-hidden />
        </MapButton>
        <div className="h-px bg-border/60 pointer-coarse:hidden" />
        <MapButton label="すべての地点を表示" onClick={recenter} disabled={status !== "ready"}>
          <LocateFixed className="size-4" aria-hidden />
        </MapButton>
      </div>
    </div>
  );
}

function MapButton({
  label,
  onClick,
  children,
  framed,
  disabled,
  testId,
  className,
}: {
  label: string;
  onClick: () => void;
  children: React.ReactNode;
  /// 単独で置く（枠と影を持つ）。
  framed?: boolean;
  disabled?: boolean;
  testId?: string;
  className?: string;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
      data-testid={testId}
      className={cn(
        "flex size-8 items-center justify-center text-foreground/75 transition-colors duration-[var(--duration-fast)] hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-40 pointer-coarse:size-10",
        framed && "rounded-lg border border-border/60 bg-card/90 shadow-[var(--elevation-sm)] backdrop-blur",
        className,
      )}
    >
      {children}
    </button>
  );
}

/// PC の会話の中で最初に並べるカードの数（それ以上は「ほか N 件」で開く）。
const GRID_LIMIT = 6;

/// 地点のカード。番号はピンと同じ。←→ で選び、選んだ地点はピンが大きくなる。
/// スマホと全画面では横に送るカルーセル、PC の会話の中では折り返して並べる（マウスで横に送らせない）。
function PlaceList({
  figure,
  selected,
  onSelect,
  floating,
  className,
}: {
  figure: MapFigure;
  selected: number | null;
  onSelect: (i: number) => void;
  floating?: boolean;
  className?: string;
}) {
  const list = React.useRef<HTMLDivElement>(null);
  const [showAll, setShowAll] = React.useState(false);
  const total = figure.markers.length;
  React.useEffect(() => {
    if (selected === null) return;
    // 畳んだカードの地点を選んだら開く。
    if (!floating && selected >= GRID_LIMIT) setShowAll(true);
    // 横にだけ送る（scrollIntoView は会話のスクロールまで動かし、全画面の裏の読む位置がずれる）。
    const box = list.current;
    const card = box?.querySelector<HTMLElement>(`[data-index="${selected}"]`);
    if (!box || !card || box.scrollWidth <= box.clientWidth) return;
    const pad = parseFloat(getComputedStyle(box).paddingLeft) || 0;
    const left = card.offsetLeft - box.offsetLeft - pad;
    if (left < box.scrollLeft || left + card.offsetWidth + pad * 2 > box.scrollLeft + box.clientWidth) {
      box.scrollTo({ left: Math.max(0, left), behavior: "smooth" });
    }
  }, [selected, floating]);
  if (total === 0) return null;
  const single = total === 1;
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
    e.preventDefault();
    const next = selected === null ? 0 : (selected + (e.key === "ArrowRight" ? 1 : -1) + total) % total;
    onSelect(next);
    list.current?.querySelector<HTMLElement>(`[data-index="${next}"] button`)?.focus();
  };
  const folded = !floating && !showAll && total > GRID_LIMIT;
  const page = (dir: 1 | -1) => list.current?.scrollBy({ left: dir * list.current.clientWidth * 0.8, behavior: "smooth" });
  return (
    <div className={cn("relative", className)}>
      {floating ? <CarouselArrows target={list} onPage={page} /> : null}
      <div
        ref={list}
        role="list"
        aria-label="地点"
        onKeyDown={onKeyDown}
        data-testid="yui-map-places"
        className={cn(
          "flex snap-x snap-mandatory gap-2 overflow-x-auto overscroll-x-contain [scrollbar-width:none] [&::-webkit-scrollbar]:hidden",
          floating ? "scroll-px-4 px-4" : "sm:grid sm:snap-none sm:grid-cols-[repeat(auto-fill,minmax(13rem,1fr))] sm:overflow-visible",
        )}
      >
        {figure.markers.map((m, i) => {
          const on = i === selected;
          return (
            <div
              key={`${i}:${m.label}`}
              role="listitem"
              data-index={i}
              className={cn(
                "group/card relative flex shrink-0 snap-start items-start gap-2.5 rounded-xl border p-3 text-left transition-colors duration-[var(--duration-fast)] ease-[var(--ease-standard)]",
                // カルーセルでは次のカードの端を見せて、横に続くことが分かるようにする。
                single ? "w-full" : floating ? "w-[min(15rem,80%)]" : "w-[min(14rem,78%)] sm:w-auto",
                folded && i >= GRID_LIMIT && "sm:hidden",
                // 選択は塗り（設計言語）。全画面では地図の上に浮かせるので不透明にする。
                floating ? "bg-card shadow-[var(--elevation-md)]" : "bg-card/40",
                on ? "border-border bg-accent" : "border-border/60 hover:border-border hover:bg-accent/60",
              )}
            >
              <span
                className={cn(
                  "mt-px flex size-5 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold tabular-nums transition-colors",
                  on ? "bg-primary text-primary-foreground" : "bg-secondary text-secondary-foreground",
                )}
                aria-hidden
              >
                {i + 1}
              </span>
              <button
                type="button"
                onClick={() => onSelect(on ? -1 : i)}
                aria-pressed={on}
                className="min-w-0 flex-1 text-left outline-none after:absolute after:inset-0 after:rounded-xl focus-visible:after:ring-2 focus-visible:after:ring-ring"
              >
                <span className="block truncate text-[13px] font-semibold leading-5 text-foreground">{m.label}</span>
                {m.desc ? <span className="mt-0.5 line-clamp-2 text-[12px] leading-[18px] text-muted-foreground [word-break:auto-phrase]">{m.desc}</span> : null}
              </button>
              <a
                href={placeUrl(m.lat, m.lng)}
                target="_blank"
                rel="noopener noreferrer"
                aria-label={`${m.label} を Google マップで開く`}
                title="Google マップで開く"
                // マウスではそのカードに載せたときだけ出す（会話のメッセージの group に反応しないよう名前付き）。タッチは常に・押しやすい大きさ。
                className="relative z-10 -m-1 flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground opacity-0 transition-opacity hover:bg-background/60 hover:text-foreground focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring group-hover/card:opacity-100 pointer-coarse:-m-2 pointer-coarse:size-10 pointer-coarse:opacity-70"
              >
                <ExternalLink className="size-3.5" aria-hidden />
              </a>
            </div>
          );
        })}
      </div>
      {folded ? (
        <button
          type="button"
          onClick={() => setShowAll(true)}
          className="mt-2 hidden rounded-md px-2 py-1 text-[12px] font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring sm:inline-flex"
        >
          ほか {total - GRID_LIMIT} 件の地点を表示
        </button>
      ) : null}
    </div>
  );
}

/// 全画面のカード列の左右の送りボタン（マウスのときだけ・送れる向きだけ出す）。
function CarouselArrows({ target, onPage }: { target: React.RefObject<HTMLDivElement | null>; onPage: (dir: 1 | -1) => void }) {
  const [edge, setEdge] = React.useState({ left: false, right: false });
  React.useEffect(() => {
    const el = target.current;
    if (!el) return;
    const update = () =>
      setEdge({ left: el.scrollLeft > 4, right: el.scrollLeft + el.clientWidth < el.scrollWidth - 4 });
    update();
    el.addEventListener("scroll", update, { passive: true });
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => {
      el.removeEventListener("scroll", update);
      ro.disconnect();
    };
  }, [target]);
  const arrow = (dir: 1 | -1) => (
    <button
      type="button"
      aria-label={dir < 0 ? "前の地点" : "次の地点"}
      onClick={() => onPage(dir)}
      className={cn(
        "absolute top-1/2 z-10 hidden size-8 -translate-y-1/2 items-center justify-center rounded-full border border-border/60 bg-card/95 text-foreground/75 shadow-[var(--elevation-md)] backdrop-blur transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring pointer-fine:flex",
        dir < 0 ? "left-2" : "right-2",
      )}
    >
      {dir < 0 ? <ChevronLeft className="size-4" aria-hidden /> : <ChevronRight className="size-4" aria-hidden />}
    </button>
  );
  return (
    <>
      {edge.left ? arrow(-1) : null}
      {edge.right ? arrow(1) : null}
    </>
  );
}
