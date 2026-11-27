// Yui の map に背景地図を重ねて描く部品（YuiFigure の renderMap に渡す・design.md §4.7）。
//
// - 背景地図は shiki-server が配る PMTiles から読む。ブラウザは shiki 以外へ接続しない
//   （スタイルは glyphs・sprite を持たず、文字は端末のフォントで描く）。
// - 地点は番号付きのピンと名前、道順は Yui と同じく地点を直線で結ぶ。
// - 画面に近づいてから地図を作り、画面から十分離れたら破棄する（WebGL の文脈を溜めない）。
// - ページのスクロールを奪わない（Ctrl/⌘＋スクロール・2 本指で操作。全画面では通常操作）。
// - 地図データが無い・読み込めない・WebGL が使えないときは Yui の背景なしの図に戻す。
"use client";

import * as React from "react";
import { useTheme } from "next-themes";
import { Loader2, Maximize2, Minimize2, Minus, Plus, Scan } from "lucide-react";
import type { MapFigure } from "@imanect/yui";
import { YuiFigure } from "@imanect/yui/react";
import type { Map as MlMap } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";

import { loadBasemapConfig, loadBasemapStyle, peekBasemapConfig, type BasemapConfig } from "@/lib/basemap";
import { cn } from "@/lib/utils";
import { createFigureMarkers, type FigureMarkers } from "./map-markers";
import { initialView, routeFeature } from "./yui-map-data";

/// ピン・名前・道順の色。背景地図（青い水域・緑の森・淡い道路）のどれとも紛れない朱色にし、
/// 名前は濃い文字に太い縁取りを付けて道路の上でも読めるようにする。
const COLORS = {
  light: { pin: "#d4442e", route: "#d4442e", ink: "#1c1917", halo: "#ffffff" },
  dark: { pin: "#f06a52", route: "#f06a52", ink: "#f5f5f4", halo: "#141414" },
} as const;

/// 文字は端末のフォントで描く（Yui の図と同じ並び）。
const FONT_FAMILY =
  '"Hiragino Sans", "Hiragino Kaku Gothic ProN", "Noto Sans JP", "Noto Sans CJK JP", "Yu Gothic", Meiryo, sans-serif';

/// 全地点を収めるときの余白（地点のまわりに出す札の大きさと、左の操作ボタンの分）。
const FIT_PADDING = { top: 48, bottom: 40, left: 96, right: 96 };

/// これより地点の多い図は、ピンを小さくし、地図を縦に広げる。
const DENSE_MARKERS = 20;

/// 地図を作り始める距離（画面の外側）と、破棄するまでの猶予。
const LOAD_MARGIN = "300px 0px";
const DISPOSE_DELAY_MS = 2000;
/// これだけ待っても地図が出なければ背景なしの図に戻す。
const LOAD_TIMEOUT_MS = 20_000;

type Phase = "checking" | "fallback" | "idle" | "loading" | "ready";

let protocolReady: Promise<{ add: (url: string) => void; drop: (url: string) => void }> | null = null;

/// pmtiles:// を MapLibre に登録する（ページ内で 1 回）。
function pmtilesProtocol(maplibre: typeof import("maplibre-gl")) {
  if (!protocolReady) {
    protocolReady = import("pmtiles").then(({ PMTiles, Protocol }) => {
      const protocol = new Protocol();
      maplibre.addProtocol("pmtiles", protocol.tile);
      return {
        add: (url: string) => {
          if (!protocol.get(url)) protocol.add(new PMTiles(url));
        },
        // 読み込みに失敗した地図データは捨てる（pmtiles は失敗したヘッダの読み込みを覚えたままにするので、
        // 使い回すと一時的な失敗のあと再読み込みするまで背景地図が出なくなる）。
        drop: (url: string) => {
          protocol.tiles.delete(url);
        },
      };
    });
  }
  return protocolReady;
}

function prefersReducedMotion(): boolean {
  return typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

export function BasemapMap({ figure, fallbackClassName }: { figure: MapFigure; fallbackClassName?: string }) {
  const { resolvedTheme } = useTheme();
  const theme = resolvedTheme === "dark" ? "dark" : "light";
  // 読み込み済みなら最初から使う（2 つめ以降の地図で、背景なしの図を一瞬挟まない）。
  const known = peekBasemapConfig();
  const [config, setConfig] = React.useState<BasemapConfig | null>(known ?? null);
  const [phase, setPhase] = React.useState<Phase>(known === undefined ? "checking" : known ? "idle" : "fallback");
  const [near, setNear] = React.useState(false);
  const [fullscreen, setFullscreen] = React.useState(false);
  // 要素の全画面に対応しない環境（iPhone の Safari など）ではボタンを出さない。
  const [canFullscreen, setCanFullscreen] = React.useState(false);
  const frameRef = React.useRef<HTMLDivElement>(null);
  const containerRef = React.useRef<HTMLDivElement>(null);
  const mapRef = React.useRef<MlMap | null>(null);
  const markersRef = React.useRef<FigureMarkers | null>(null);
  // 指している地点（札・一覧に乗せている）と、押して選んだ地点。カードは指している方を優先する。
  const [hover, setHover] = React.useState<number | null>(null);
  const [selected, setSelected] = React.useState<number | null>(null);
  const active = hover ?? selected;
  const titleId = React.useId();

  // 地図データがあるか（無ければ背景なしの図）。
  React.useEffect(() => {
    let alive = true;
    void loadBasemapConfig().then((c) => {
      if (!alive) return;
      setConfig(c);
      setPhase(c ? "idle" : "fallback");
    });
    return () => {
      alive = false;
    };
  }, []);

  // 画面に近づいたら作り、離れて少し経ったら破棄する。全画面の間は破棄しない。
  React.useEffect(() => {
    const el = frameRef.current;
    if (!el || !config) return;
    let timer: number | undefined;
    const io = new IntersectionObserver(
      ([entry]) => {
        window.clearTimeout(timer);
        if (entry?.isIntersecting) setNear(true);
        else timer = window.setTimeout(() => setNear(false), DISPOSE_DELAY_MS);
      },
      { rootMargin: LOAD_MARGIN },
    );
    io.observe(el);
    return () => {
      window.clearTimeout(timer);
      io.disconnect();
    };
  }, [config]);

  const keepAlive = near || fullscreen;

  // 地図の生成と破棄（テーマが変わったら作り直す）。
  React.useEffect(() => {
    const container = containerRef.current;
    if (!config || !keepAlive || !container) return;
    let disposed = false;
    let map: MlMap | null = null;
    let failed = false;
    let archives: string[] = [];
    setPhase("loading");
    const fail = (why: unknown) => {
      if (disposed || failed) return;
      failed = true;
      window.clearTimeout(timeout);
      // 背景なしの図に戻したら地図は捨てる（後から load・idle が来ても地図へ戻らない）。
      map?.remove();
      map = null;
      mapRef.current = null;
      void protocolReady?.then((p) => archives.forEach((u) => p.drop(u)));
      console.warn("背景地図を表示できないため、背景なしの図を表示します", why);
      setPhase("fallback");
    };
    // スタイルや部品の読み込みも含めて待つ上限（応答しない接続で「読み込み中」のまま残さない）。
    const timeout = window.setTimeout(() => fail(new Error("timeout")), LOAD_TIMEOUT_MS);
    (async () => {
      const [maplibre, loaded] = await Promise.all([
        import("maplibre-gl"),
        loadBasemapStyle(config.styleUrl, theme, window.location.origin),
      ]);
      if (disposed || failed) return;
      if (loaded.archives.length === 0) throw new Error("スタイルに地図データがありません");
      archives = loaded.archives;
      const protocol = await pmtilesProtocol(maplibre);
      loaded.archives.forEach((u) => protocol.add(u));
      if (disposed || failed) return;
      const view = initialView(figure);
      const colors = COLORS[theme];
      map = new maplibre.Map({
        container,
        style: loaded.style as unknown as import("maplibre-gl").StyleSpecification,
        ...(view.kind === "center"
          ? { center: view.center, zoom: view.zoom }
          : { bounds: view.bounds, fitBoundsOptions: { padding: FIT_PADDING, maxZoom: view.maxZoom } }),
        maxZoom: 18,
        attributionControl: false,
        cooperativeGestures: true,
        dragRotate: false,
        pitchWithRotate: false,
        touchPitch: false,
        localIdeographFontFamily: FONT_FAMILY,
        locale: {
          "Map.Title": figure.title ? `地図: ${figure.title}` : "地図",
          "CooperativeGesturesHandler.WindowsHelpText": "Ctrl キーを押しながらスクロールで拡大・縮小",
          "CooperativeGesturesHandler.MacHelpText": "⌘ キーを押しながらスクロールで拡大・縮小",
          "CooperativeGesturesHandler.MobileHelpText": "2 本の指で地図を動かします",
        },
      });
      mapRef.current = map;
      const m = map;
      // 枠の大きさが変わったら（一覧の表示・幅の変化）地図を合わせる（MapLibre が見るのは窓の大きさだけ）。
      const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => m.resize());
      ro?.observe(container);
      m.once("remove", () => ro?.disconnect());
      m.touchZoomRotate.disableRotation();
      m.keyboard.disableRotation();
      // 全画面のまま作り直したとき（テーマの切り替えなど）も、全画面ではスクロールで拡大縮小できるようにする。
      if (document.fullscreenElement === frameRef.current) m.cooperativeGestures.disable();
      let loadedOnce = false;
      m.on("error", (e) => {
        // 読み込み前の失敗（スタイル・地図データ）は背景なしの図に戻す。読み込み後のタイル単位の
        // 失敗は、その範囲が空くだけなので図はそのまま見せる。
        if (!loadedOnce) fail(e.error);
      });
      m.on("load", () => {
        loadedOnce = true;
        // 枠の大きさが決まってから全地点に合わせ直す（作った時点では一覧の幅が反映されていないことがある）。
        m.resize();
        if (view.kind === "bounds") m.fitBounds(view.bounds, { padding: FIT_PADDING, maxZoom: view.maxZoom, animate: false });
        addRouteLayer(m, figure, colors);
        markersRef.current = createFigureMarkers(maplibre, m, figure, {
          onHover: setHover,
          onSelect: (i) => setSelected((cur) => (cur === i ? null : i)),
        });
        // 地図の何も無い所を押したら選んだ地点を外す（札の上の押下は札が受ける）。
        m.on("click", (e) => {
          if (e.originalEvent.target === m.getCanvas()) setSelected(null);
        });
        m.once("idle", () => {
          window.clearTimeout(timeout);
          if (!disposed && !failed) setPhase("ready");
        });
      });
    })().catch(fail);
    return () => {
      disposed = true;
      window.clearTimeout(timeout);
      markersRef.current?.remove();
      markersRef.current = null;
      setHover(null);
      setSelected(null);
      map?.remove();
      mapRef.current = null;
      setPhase((p) => (p === "fallback" ? p : "idle"));
    };
  }, [config, keepAlive, theme, figure]);

  // 指している・選んでいる地点を札とカードに映す。
  React.useEffect(() => {
    markersRef.current?.setActive(active);
  }, [active, phase]);

  // 全画面の出入り。全画面の間はスクロールで拡大縮小できるようにする。
  React.useEffect(() => {
    setCanFullscreen(document.fullscreenEnabled === true);
    const onChange = () => {
      const on = document.fullscreenElement === frameRef.current;
      setFullscreen(on);
      const m = mapRef.current;
      if (!m) return;
      if (on) m.cooperativeGestures.disable();
      else m.cooperativeGestures.enable();
      requestAnimationFrame(() => m.resize());
    };
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  }, []);

  if (phase === "fallback" || phase === "checking") {
    // 設定を読む間も背景なしの図を出しておく（地図データが無い環境ではそのまま）。
    return <YuiFigure figure={figure} {...(fallbackClassName ? { className: fallbackClassName } : {})} />;
  }

  const ready = phase === "ready";
  const animate = !prefersReducedMotion();
  const zoomBy = (d: number) => mapRef.current?.zoomTo(mapRef.current.getZoom() + d, { animate });
  const fitAll = () => {
    const m = mapRef.current;
    if (!m) return;
    const v = initialView(figure);
    if (v.kind === "center") m.easeTo({ center: v.center, zoom: v.zoom, animate });
    else m.fitBounds(v.bounds, { padding: FIT_PADDING, maxZoom: v.maxZoom, animate });
  };
  const toggleFullscreen = () => {
    const done = document.fullscreenElement ? document.exitFullscreen() : frameRef.current?.requestFullscreen();
    void done?.catch(() => {
      /* 権限などで全画面にできないときは何もしない */
    });
  };
  const focusMarker = (i: number) => {
    const m = mapRef.current;
    const mk = figure.markers[i];
    if (!m || !mk) return;
    setSelected(i);
    // 画面の外にあるときだけ寄せる（見えているなら、ほかの地点との位置関係を崩さない）。
    if (!m.getBounds().contains([mk.lng, mk.lat])) m.easeTo({ center: [mk.lng, mk.lat], animate });
  };

  return (
    <div className="min-w-0" data-testid="basemap-figure" data-state={phase}>
      {figure.title ? (
        <div id={titleId} className="mb-2 text-[14px] font-semibold leading-snug text-foreground">
          {figure.title}
        </div>
      ) : null}
      {/* 地図と地点の一覧を 1 枚のカードにまとめる（広い画面は一覧を右に、狭い画面は下に）。 */}
      <div
        ref={frameRef}
        role="region"
        aria-labelledby={figure.title ? titleId : undefined}
        aria-label={figure.title ? undefined : "地図"}
        className={cn(
          "flex flex-col overflow-hidden border border-border/60 bg-background md:flex-row",
          // 広い画面は地図と一覧を同じ高さにそろえる（一覧は中でスクロールする）。
          fullscreen
            ? "h-screen w-screen rounded-none"
            : cn("rounded-xl", figure.markers.length > DENSE_MARKERS ? "md:h-[520px]" : "md:h-[380px]"),
        )}
      >
        <div
          className={cn(
            "relative min-w-0 flex-1 bg-secondary/40",
            // 地点の様子が分かる大きさにする（地点の多い図はさらに縦に広げる）。
            fullscreen ? "h-full" : figure.markers.length > DENSE_MARKERS ? "h-[380px] md:h-full" : "h-[320px] md:h-full",
          )}
        >
          {/* MapLibre は自分の要素に position: relative を付けるので、絶対配置ではなく大きさで埋める。 */}
          <div ref={containerRef} className="h-full w-full" data-testid="basemap-canvas" />
          {!ready ? (
            <div
              className="pointer-events-none absolute inset-0 flex items-center justify-center gap-2 bg-secondary/60 text-xs text-muted-foreground"
              aria-live="polite"
            >
              <Loader2 className="size-3.5 animate-spin" />
              地図を読み込んでいます…
            </div>
          ) : null}
          <div className="absolute left-2 top-2 flex flex-col gap-1.5">
            <div className="flex flex-col overflow-hidden rounded-full border border-border/60 bg-background/95 shadow-sm backdrop-blur">
              <MapButton label="拡大" onClick={() => zoomBy(1)} disabled={!ready}>
                <Plus className="size-4" />
              </MapButton>
              <MapButton label="縮小" onClick={() => zoomBy(-1)} disabled={!ready} className="border-t border-border/60">
                <Minus className="size-4" />
              </MapButton>
            </div>
            <div className="flex flex-col overflow-hidden rounded-full border border-border/60 bg-background/95 shadow-sm backdrop-blur">
              <MapButton label="全地点を表示" onClick={fitAll} disabled={!ready}>
                <Scan className="size-4" />
              </MapButton>
            </div>
            {canFullscreen ? (
              <div className="flex flex-col overflow-hidden rounded-full border border-border/60 bg-background/95 shadow-sm backdrop-blur">
                <MapButton
                  label={fullscreen ? "全画面を終了" : "全画面で表示"}
                  onClick={toggleFullscreen}
                  data-testid="basemap-fullscreen"
                >
                  {fullscreen ? <Minimize2 className="size-4" /> : <Maximize2 className="size-4" />}
                </MapButton>
              </div>
            ) : null}
          </div>
          {/* 出典（地図データの利用条件）。閉じたり隠したりできない形で常に出す。 */}
          <div
            className="absolute bottom-0 right-0 max-w-full truncate rounded-tl-md bg-background/85 px-1.5 py-0.5 text-[10px] leading-4 text-muted-foreground"
            data-testid="basemap-attribution"
          >
            {config?.attribution.map((a, i) => (
              <React.Fragment key={a.url}>
                {i > 0 ? " " : null}
                <a href={a.url} target="_blank" rel="noopener noreferrer" className="hover:underline">
                  {a.text}
                </a>
              </React.Fragment>
            ))}
          </div>
        </div>
        {/* 地点の一覧。行に乗せると地図の札とカードが連動し、押すと選ぶ（画面の外なら寄せる）。
            ノートの本文（prose）の ol/li の装飾を受けないよう、要素ではなく role で一覧にする。 */}
        <div
          className={cn(
            "flex min-h-0 flex-col border-t border-border/60 md:w-[260px] md:shrink-0 md:border-l md:border-t-0",
            fullscreen ? "" : "max-h-[240px] md:max-h-none",
          )}
        >
          <div className="px-3.5 pb-1.5 pt-3 text-[13px] font-semibold text-foreground">{figure.markers.length} か所</div>
          <div
            role="list"
            aria-label="地点の一覧"
            className="min-h-0 flex-1 overflow-y-auto pb-1.5"
            data-testid="basemap-legend"
            onMouseLeave={() => setHover(null)}
          >
            {figure.markers.map((mk, i) => (
              <div role="listitem" key={`${i}-${mk.label}`} className="min-w-0">
                <button
                  type="button"
                  onClick={() => focusMarker(i)}
                  onMouseEnter={() => setHover(i)}
                  onFocus={() => setHover(i)}
                  onBlur={() => setHover(null)}
                  disabled={!ready}
                  aria-pressed={selected === i}
                  className={cn(
                    "flex w-full min-w-0 flex-col items-start border-b border-border/40 px-3.5 py-2 text-left transition-colors",
                    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
                    active === i ? "bg-accent" : "hover:bg-accent/60",
                  )}
                >
                  <span className="w-full truncate text-[13.5px] font-semibold leading-snug text-foreground">
                    {mk.label}
                  </span>
                  {mk.desc ? (
                    <span className="w-full truncate text-[12px] leading-snug text-muted-foreground">{mk.desc}</span>
                  ) : null}
                </button>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

function MapButton({
  label,
  className,
  children,
  ...props
}: React.ComponentProps<"button"> & { label: string }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={cn(
        "flex size-8 items-center justify-center text-foreground/80 transition-colors hover:bg-secondary hover:text-foreground",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:opacity-40 disabled:hover:bg-transparent",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  );
}

/// 道順の線のレイヤを足す（地点の札は DOM で描く・map-markers.ts）。
function addRouteLayer(m: MlMap, figure: MapFigure, colors: (typeof COLORS)["light"] | (typeof COLORS)["dark"]) {
  const route = routeFeature(figure);
  if (!route) return;
  m.addSource("yui-route", { type: "geojson", data: route });
  m.addLayer({
    id: "yui-route",
    type: "line",
    source: "yui-route",
    layout: { "line-cap": "round", "line-join": "round" },
    paint: { "line-color": colors.route, "line-width": 3.5, "line-opacity": 0.85, "line-dasharray": [2, 1.6] },
  });
}
