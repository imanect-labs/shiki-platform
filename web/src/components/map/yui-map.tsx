// 回答の ```ui の map を、背景の地図（MapLibre）つきで描く（@imanect/yui/react の renderMap）。
//
// Yui はデータ（地点・道順・縮尺）と静止画（位置関係だけ）を持ち、背景の地図はホスト（ここ）が描く。
// 地図が無効な構成（オンプレ・エアギャップ）や読み込みに失敗したときは、Yui の静止画に戻す。
// 見た目は ChatGPT の地図に寄せず、番号のピン・地点のカード・全画面の 3 つに絞る。
"use client";

import * as React from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { ExternalLink, LocateFixed, Maximize2, Minus, Navigation, Plus, X } from "lucide-react";
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
        className="h-[260px] rounded-[14px] border border-border sm:h-[340px]"
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
              className="inline-flex items-center gap-1 font-medium text-foreground underline-offset-2 hover:underline"
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
            <div className="flex h-12 shrink-0 items-center gap-2 border-b border-border px-4 pt-[env(safe-area-inset-top)]">
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
                  className="absolute inset-0"
                />
              ) : null}
              <div className="pointer-events-none absolute inset-x-0 bottom-0 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
                <PlaceList figure={figure} selected={selected} onSelect={select} floating className="pointer-events-auto px-3" />
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
        // 読み込み中: 地の色の上に、薄い格子を流す（白い箱や回転するアイコンは出さない）。
        <div
          aria-hidden
          className="pointer-events-none absolute inset-0 animate-pulse bg-[linear-gradient(var(--border)_1px,transparent_1px),linear-gradient(90deg,var(--border)_1px,transparent_1px)] bg-[size:48px_48px] opacity-60"
        />
      ) : null}
      <span className="sr-only" aria-live="polite">
        {status === "loading" ? "地図を読み込んでいます" : ""}
      </span>
      <div className="absolute right-2.5 top-2.5 z-10 flex flex-col items-end gap-2">
        {onExpand ? (
          <MapButton label="全画面で見る" onClick={onExpand} testId="yui-map-expand">
            <Maximize2 className="size-4" aria-hidden />
          </MapButton>
        ) : null}
        <div className="flex flex-col overflow-hidden rounded-lg border border-border bg-card/95 shadow-[var(--elevation-sm)] backdrop-blur">
          <MapButton label="拡大" onClick={() => zoom(1)} bare disabled={status !== "ready"}>
            <Plus className="size-4" aria-hidden />
          </MapButton>
          <div className="h-px bg-border" />
          <MapButton label="縮小" onClick={() => zoom(-1)} bare disabled={status !== "ready"}>
            <Minus className="size-4" aria-hidden />
          </MapButton>
        </div>
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
  bare,
  disabled,
  testId,
}: {
  label: string;
  onClick: () => void;
  children: React.ReactNode;
  bare?: boolean;
  disabled?: boolean;
  testId?: string;
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
        "flex size-8 items-center justify-center text-foreground/80 transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-40 pointer-coarse:size-10",
        !bare && "rounded-lg border border-border bg-card/95 shadow-[var(--elevation-sm)] backdrop-blur",
      )}
    >
      {children}
    </button>
  );
}

/// 地点のカード。番号はピンと同じ。←→ で選び、選んだ地点はピンが大きくなる。
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
  React.useEffect(() => {
    if (selected === null) return;
    // 横にだけ送る（scrollIntoView は会話のスクロールまで動かし、全画面の裏の読む位置がずれる）。
    const box = list.current;
    const card = box?.querySelector<HTMLElement>(`[data-index="${selected}"]`);
    if (!box || !card) return;
    const left = card.offsetLeft - box.offsetLeft;
    if (left < box.scrollLeft || left + card.offsetWidth > box.scrollLeft + box.clientWidth) {
      box.scrollTo({ left: Math.max(0, left - 12), behavior: "smooth" });
    }
  }, [selected]);
  if (figure.markers.length === 0) return null;
  const single = figure.markers.length === 1;
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
    e.preventDefault();
    const n = figure.markers.length;
    const next = selected === null ? 0 : (selected + (e.key === "ArrowRight" ? 1 : -1) + n) % n;
    onSelect(next);
    list.current?.querySelector<HTMLElement>(`[data-index="${next}"] button`)?.focus();
  };
  return (
    <div
      ref={list}
      role="list"
      aria-label="地点"
      onKeyDown={onKeyDown}
      data-testid="yui-map-places"
      className={cn(
        "flex snap-x snap-mandatory gap-2 overflow-x-auto overscroll-x-contain [scrollbar-width:none] [&::-webkit-scrollbar]:hidden",
        className,
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
              "group relative flex shrink-0 snap-start items-start gap-2.5 rounded-xl border bg-card p-3 text-left transition-[border-color,box-shadow] duration-150",
              // 次のカードの端を見せて、横に続くことが分かるようにする。
              single ? "w-full" : "w-[min(14rem,78%)]",
              floating && "shadow-[var(--elevation-md)]",
              on ? "border-primary/50 ring-2 ring-primary/15" : "border-border hover:border-foreground/20",
            )}
          >
            <span
              className={cn(
                "mt-px flex size-5 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold tabular-nums",
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
              {m.desc ? <span className="mt-0.5 line-clamp-2 text-[12px] leading-[18px] text-muted-foreground">{m.desc}</span> : null}
            </button>
            <a
              href={placeUrl(m.lat, m.lng)}
              target="_blank"
              rel="noopener noreferrer"
              aria-label={`${m.label} を Google マップで開く`}
              title="Google マップで開く"
              className="relative z-10 -m-1 flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground opacity-70 transition hover:bg-accent hover:text-foreground hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <ExternalLink className="size-3.5" aria-hidden />
            </a>
          </div>
        );
      })}
    </div>
  );
}
