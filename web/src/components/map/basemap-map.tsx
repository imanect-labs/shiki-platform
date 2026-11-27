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
import type { Map as MlMap, MapGeoJSONFeature, Popup } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";

import { loadBasemapConfig, loadBasemapStyle, peekBasemapConfig, type BasemapConfig } from "@/lib/basemap";
import { cn } from "@/lib/utils";
import { initialView, markerFeatures, routeFeature } from "./yui-map-data";

/// Yui の図と同じ系列色（s1）と文字色（ink / surface）。ピンと名前を背景なしの図と同じ見た目にする。
const COLORS = {
  light: { pin: "#398cb8", route: "#2f7aa3", ink: "#18181b", halo: "#fcfcfc" },
  dark: { pin: "#4d9ac8", route: "#6fb1da", ink: "#f2f2f2", halo: "#0d0d0f" },
} as const;

/// 文字は端末のフォントで描く（Yui の図と同じ並び）。
const FONT_FAMILY =
  '"Hiragino Sans", "Hiragino Kaku Gothic ProN", "Noto Sans JP", "Noto Sans CJK JP", "Yu Gothic", Meiryo, sans-serif';

/// 全地点を収めるときの余白（右は操作ボタンの幅と名前の長さの分だけ広く取り、ピンの右に出す名前がボタンの下に入らないようにする）。
const FIT_PADDING = { top: 40, bottom: 36, left: 40, right: 128 };

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
  const popupRef = React.useRef<Popup | null>(null);
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
      const showPopup = (f: MapGeoJSONFeature | undefined) => {
        if (!f || f.geometry.type !== "Point") return;
        const p = f.properties as { n: number; label: string; desc: string };
        popupRef.current?.remove();
        popupRef.current = new maplibre.Popup({ closeButton: true, offset: 14, maxWidth: "260px" })
          .setLngLat(f.geometry.coordinates as [number, number])
          .setDOMContent(popupContent(p.n, p.label, p.desc))
          .addTo(m);
      };
      m.on("load", () => {
        loadedOnce = true;
        addFigureLayers(m, figure, colors);
        // レイヤを足してから受ける（無いレイヤを指すと MapLibre が問い合わせで失敗する）。
        m.on("click", "yui-pins", (e) => showPopup(e.features?.[0]));
        m.on("mouseenter", "yui-pins", () => (m.getCanvas().style.cursor = "pointer"));
        m.on("mouseleave", "yui-pins", () => (m.getCanvas().style.cursor = ""));
        m.once("idle", () => {
          window.clearTimeout(timeout);
          if (!disposed && !failed) setPhase("ready");
        });
      });
    })().catch(fail);
    return () => {
      disposed = true;
      window.clearTimeout(timeout);
      popupRef.current?.remove();
      popupRef.current = null;
      map?.remove();
      mapRef.current = null;
      setPhase((p) => (p === "fallback" ? p : "idle"));
    };
  }, [config, keepAlive, theme, figure]);

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
    m.easeTo({ center: [mk.lng, mk.lat], zoom: Math.max(m.getZoom(), 15), animate });
    popupRef.current?.remove();
    void import("maplibre-gl").then((maplibre) => {
      if (mapRef.current !== m) return;
      popupRef.current = new maplibre.Popup({ closeButton: true, offset: 14, maxWidth: "260px" })
        .setLngLat([mk.lng, mk.lat])
        .setDOMContent(popupContent(i + 1, mk.label, mk.desc ?? ""))
        .addTo(m);
    });
  };

  return (
    <div className="min-w-0" data-testid="basemap-figure" data-state={phase}>
      {figure.title ? (
        <div id={titleId} className="mb-2 text-[14px] font-semibold leading-snug text-foreground">
          {figure.title}
        </div>
      ) : null}
      <div
        ref={frameRef}
        role="region"
        aria-labelledby={figure.title ? titleId : undefined}
        aria-label={figure.title ? undefined : "地図"}
        className={cn(
          "relative overflow-hidden border border-border/60 bg-secondary/40",
          // 高さは Yui の背景なしの図（最大 320px）にそろえる。
          fullscreen ? "h-screen w-screen rounded-none" : "h-[280px] rounded-xl sm:h-[320px]",
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
        <div className="absolute right-2 top-2 flex flex-col gap-1">
          <div className="flex flex-col overflow-hidden rounded-lg border border-border/60 bg-background/90 shadow-sm backdrop-blur">
            <MapButton label="拡大" onClick={() => zoomBy(1)} disabled={!ready}>
              <Plus className="size-4" />
            </MapButton>
            <MapButton label="縮小" onClick={() => zoomBy(-1)} disabled={!ready} className="border-t border-border/60">
              <Minus className="size-4" />
            </MapButton>
          </div>
          <div className="flex flex-col overflow-hidden rounded-lg border border-border/60 bg-background/90 shadow-sm backdrop-blur">
            <MapButton label="全地点を表示" onClick={fitAll} disabled={!ready}>
              <Scan className="size-4" />
            </MapButton>
            {canFullscreen ? (
              <MapButton
                label={fullscreen ? "全画面を終了" : "全画面で表示"}
                onClick={toggleFullscreen}
                className="border-t border-border/60"
                data-testid="basemap-fullscreen"
              >
                {fullscreen ? <Minimize2 className="size-4" /> : <Maximize2 className="size-4" />}
              </MapButton>
            ) : null}
          </div>
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
      {/* ノートの本文（prose）の ol/li の装飾を受けないよう、要素ではなく role で一覧にする。 */}
      <div
        role="list"
        aria-label="地点の一覧"
        className="mt-2 grid gap-x-4 gap-y-0.5 text-[12px] leading-5 text-muted-foreground sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4"
        data-testid="basemap-legend"
      >
        {figure.markers.map((mk, i) => (
          <div role="listitem" key={`${i}-${mk.label}`} className="min-w-0">
            <button
              type="button"
              onClick={() => focusMarker(i)}
              disabled={!ready}
              className="flex w-full min-w-0 items-baseline gap-1.5 rounded px-1 text-left hover:bg-secondary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:hover:bg-transparent"
            >
              <span className="w-5 shrink-0 text-right font-semibold tabular-nums text-foreground">{i + 1}.</span>
              <span className="truncate">
                <span className="text-foreground">{mk.label}</span>
                {mk.desc ? <span>　{mk.desc}</span> : null}
              </span>
            </button>
          </div>
        ))}
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

/// ピンの吹き出し。名前・説明は回答の文字なので、HTML として解釈させない（textContent）。
function popupContent(n: number, label: string, desc: string): HTMLElement {
  const root = document.createElement("div");
  root.className = "text-[12px] leading-5 text-zinc-900";
  const title = document.createElement("div");
  title.className = "font-semibold";
  title.textContent = `${n}. ${label}`;
  root.append(title);
  if (desc) {
    const d = document.createElement("div");
    d.className = "text-zinc-600";
    d.textContent = desc;
    root.append(d);
  }
  return root;
}

/// 地点（番号付きのピン・名前）と道順のレイヤを足す。
function addFigureLayers(
  m: MlMap,
  figure: MapFigure,
  colors: (typeof COLORS)["light"] | (typeof COLORS)["dark"],
) {
  const route = routeFeature(figure);
  // 地点が多い（20 を超える）図はピンを小さくし、引いた表示でピン同士が塗りつぶし合わないようにする。
  const dense = figure.markers.length > 20;
  const pin = dense ? { radius: 8, number: 9, padding: 6 } : { radius: 11, number: 11, padding: 9 };
  m.addSource("yui-points", { type: "geojson", data: markerFeatures(figure) });
  if (route) {
    m.addSource("yui-route", { type: "geojson", data: route });
    m.addLayer({
      id: "yui-route",
      type: "line",
      source: "yui-route",
      layout: { "line-cap": "round", "line-join": "round" },
      paint: { "line-color": colors.route, "line-width": 3, "line-dasharray": [2, 1.6] },
    });
  }
  // 名前は重なるものを隠す（番号の若い地点を優先）。ピンと番号は必ず出す。
  // 記号は上のレイヤから順に場所を決めるので、名前は番号より下に置き、番号の後に置き場所を探させる
  // （名前がほかのピンに重ならない）。
  m.addLayer({
    id: "yui-labels",
    type: "symbol",
    source: "yui-points",
    layout: {
      "text-field": ["get", "label"],
      "text-size": 12.5,
      "text-variable-anchor": ["left", "right", "top", "bottom"],
      "text-radial-offset": 1.3,
      "text-justify": "auto",
      "text-max-width": 10,
      "symbol-sort-key": ["get", "n"],
    },
    paint: { "text-color": colors.ink, "text-halo-color": colors.halo, "text-halo-width": 1.6 },
  });
  m.addLayer({
    id: "yui-pins",
    type: "circle",
    source: "yui-points",
    // 近い地点でピンが重なるときは、番号の若いピンを上に描く。
    layout: { "circle-sort-key": ["-", 0, ["get", "n"]] },
    paint: {
      "circle-radius": pin.radius,
      "circle-color": colors.pin,
      "circle-stroke-color": colors.halo,
      "circle-stroke-width": dense ? 1.5 : 2,
    },
  });
  m.addLayer({
    id: "yui-pin-numbers",
    type: "symbol",
    source: "yui-points",
    layout: {
      "text-field": ["to-string", ["get", "n"]],
      "text-size": pin.number,
      // 番号の当たり判定をピンの円まで広げ、名前がほかのピンに重ならないようにする。
      "text-padding": pin.padding,
      "text-allow-overlap": true,
      "symbol-sort-key": ["-", 0, ["get", "n"]],
    },
    paint: { "text-color": "#ffffff" },
  });
}
